//! indexer.rs — فهرسة محتوى الموقع في SQLite + بحث محلي FTS5
//!
//! لماذا؟ مسارات البحث والمنتدى في الموقع محمية بتحقق LiteSpeed reCAPTCHA،
//! والحل العملي: فهرسة دورية هادئة (طلب كل ~1.5 ث) في قاعدة محلية،
//! ثم البحث المحلي الفوري عبر FTS5 يدعم العربية.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::{params, Connection};

use crate::catalog;
use crate::config::{BASE_URL, CONFIG};
use crate::fetcher::{Fetcher, GetOpts};
use crate::scraper::{parse_items, parse_sections, parse_streams, Item};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token TEXT UNIQUE,
    kind TEXT,
    url TEXT,
    title TEXT,
    level TEXT,
    subject TEXT,
    section TEXT,
    stream TEXT,
    year TEXT,
    pdf_url TEXT,
    solution INTEGER DEFAULT 0,
    tags TEXT DEFAULT '',
    published INTEGER DEFAULT 0,
    updated_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_items_level ON items(level, subject);
CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
    title, level, subject, content='items', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, title, level, subject)
  VALUES (new.id, new.title, new.level, new.subject);
END;
CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, level, subject)
  VALUES ('delete', old.id, old.title, old.level, old.subject);
END;
CREATE TRIGGER IF NOT EXISTS items_au AFTER UPDATE OF title, level, subject ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, level, subject)
  VALUES ('delete', old.id, old.title, old.level, old.subject);
  INSERT INTO items_fts(rowid, title, level, subject)
  VALUES (new.id, new.title, new.level, new.subject);
END;
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT
);
CREATE TABLE IF NOT EXISTS subscribers (
    chat_id INTEGER PRIMARY KEY,
    added_at INTEGER
);
"#;

const INSERT_SQL: &str = "INSERT INTO items(token, kind, url, title, level, subject,
                                 section, stream, year, solution, tags, published, updated_at)
   VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,?12)
   ON CONFLICT(token) DO UPDATE SET
     title=excluded.title,
     year=excluded.year,
     solution=excluded.solution,
     tags=excluded.tags,
     updated_at=excluded.updated_at";

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub running: bool,
    pub level: String,
    pub subject: String,
    pub done: usize,
    pub total: usize,
    pub errors: usize,
    pub skipped: usize,
    pub started_at: Option<Instant>,
    pub last_error: String,
}

#[derive(Debug, Clone)]
pub struct Target {
    pub level: String,
    pub subject: String,
    pub section: String,
    pub stream: String,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total: i64,
    pub by_level: Vec<(String, i64)>,
    pub last_index: Option<i64>,
}

fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<Item> {
    let tags_raw: String = row.get("tags").unwrap_or_default();
    let tags: Vec<String> = tags_raw
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let solution: i64 = row.get("solution").unwrap_or(0);
    Ok(Item {
        kind: row.get("kind").unwrap_or_default(),
        token: row.get("token").unwrap_or_default(),
        title: row.get("title").unwrap_or_default(),
        url: row.get("url").unwrap_or_default(),
        pdf_url: row.get("pdf_url").unwrap_or_default(),
        level: row.get("level").unwrap_or_default(),
        subject: row.get("subject").unwrap_or_default(),
        section: row.get("section").unwrap_or_default(),
        stream: row.get("stream").unwrap_or_default(),
        year: row.get("year").unwrap_or_default(),
        solution: solution != 0,
        tags,
    })
}

pub struct Indexer {
    fetcher: Arc<Fetcher>,
    db: Mutex<Connection>,
    progress: Mutex<Progress>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl Indexer {
    pub fn new(fetcher: Arc<Fetcher>, db_path: Option<String>) -> anyhow::Result<Self> {
        let path = db_path.unwrap_or_else(|| CONFIG.db_path.clone());
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(&path)?;
        // WAL + مهلة انتظار: تسمح بتشغيل البوت والزاحف المستقل معًا على نفس
        // ملف قاعدة البيانات دون أخطاء «database is locked».
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()));
        let _ = conn.execute_batch("PRAGMA synchronous=NORMAL;");
        conn.execute_batch(SCHEMA)?;
        // ترحيل لقواعد قديمة: أضف الأعمدة الجديدة إن غابت
        let mut cols: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare("PRAGMA table_info(items)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for r in rows {
                cols.push(r?);
            }
        }
        if !cols.iter().any(|c| c == "solution") {
            conn.execute(
                "ALTER TABLE items ADD COLUMN solution INTEGER DEFAULT 0",
                [],
            )?;
        }
        if !cols.iter().any(|c| c == "tags") {
            conn.execute("ALTER TABLE items ADD COLUMN tags TEXT DEFAULT ''", [])?;
        }
        if !cols.iter().any(|c| c == "published") {
            // العناصر الموجودة قبل إضافة العمود تُعدّ منشورة سلفًا لتفادي نشر أرشيف كامل.
            conn.execute(
                "ALTER TABLE items ADD COLUMN published INTEGER DEFAULT 1",
                [],
            )?;
        }
        Ok(Indexer {
            fetcher,
            db: Mutex::new(conn),
            progress: Mutex::new(Progress::default()),
            db_path: path,
        })
    }

    // ─── الإدراج ─────────────────────────────────────────────────────
    pub fn upsert_items(&self, items: &[Item]) -> usize {
        let mut db = self.db.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let mut added = 0usize;
        let tx = match db.transaction() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        {
            let mut stmt = match tx.prepare(INSERT_SQL) {
                Ok(s) => s,
                Err(_) => return 0,
            };
            for it in items {
                if it.token.is_empty() || it.url.is_empty() {
                    continue;
                }
                let _ = stmt.execute(params![
                    it.token,
                    it.kind,
                    it.url,
                    it.title,
                    it.level,
                    it.subject,
                    it.section,
                    it.stream,
                    it.year,
                    if it.solution { 1 } else { 0 },
                    it.tags.join(","),
                    now,
                ]);
                added += 1;
            }
        }
        let _ = tx.commit();
        added
    }

    /// إعادة بناء فهرس البحث يدويًا.
    pub fn rebuild_fts(&self) {
        let db = self.db.lock().unwrap();
        let _ = db.execute("INSERT INTO items_fts(items_fts) VALUES('rebuild')", []);
    }

    // ─── الفهرسة ─────────────────────────────────────────────────────
    /// قائمة صفحات القوائم المطلوب فهرستها لمادة معينة.
    pub async fn targets_for_subject(&self, level: &str, subject: &str) -> Vec<Target> {
        let mut out = Vec::new();
        if catalog::EXAM_LEVELS.contains(&level) {
            return vec![Target {
                level: level.into(),
                subject: subject.into(),
                section: String::new(),
                stream: String::new(),
                path: format!("/ar/{level}/{subject}"),
            }];
        }
        let res = self
            .fetcher
            .get(
                &format!("{BASE_URL}/ar/{level}/{subject}"),
                GetOpts::default(),
            )
            .await;
        if !res.ok {
            {
                let mut p = self.progress.lock().unwrap();
                p.errors += 1;
                p.last_error = format!("/ar/{level}/{subject} -> {}", res.block.as_str());
            }
            log::warn!(
                "index skip subject page /ar/{}/{} ({})",
                level,
                subject,
                res.block.as_str()
            );
            return out;
        }
        if catalog::STREAM_LEVELS.contains(&level) {
            for s in parse_streams(level, subject, &res.text) {
                out.push(Target {
                    level: level.into(),
                    subject: subject.into(),
                    section: String::new(),
                    stream: s.clone(),
                    path: format!("/ar/bac/{subject}/{s}"),
                });
            }
            if !out.is_empty() {
                return out;
            }
        } else {
            for sec in parse_sections(level, subject, &res.text) {
                out.push(Target {
                    level: level.into(),
                    subject: subject.into(),
                    section: sec.clone(),
                    stream: String::new(),
                    path: format!("/ar/{level}/{subject}/{sec}"),
                });
            }
            if !out.is_empty() {
                return out;
            }
        }
        // المادة بلا أقسام/شعب ظاهرة: عناصر مباشرة من صفحتها
        vec![Target {
            level: level.into(),
            subject: subject.into(),
            section: String::new(),
            stream: String::new(),
            path: format!("/ar/{level}/{subject}"),
        }]
    }

    pub async fn index_subject(&self, level: &str, subject: &str) -> usize {
        let mut added_total = 0usize;
        for tg in self.targets_for_subject(level, subject).await {
            let url = format!("{BASE_URL}{}", tg.path);
            let res = self.fetcher.get(&url, GetOpts::default()).await;
            if !res.ok {
                if res.block == crate::fetcher::BlockType::NotFound {
                    self.progress.lock().unwrap().skipped += 1;
                    log::debug!("index skip {} (not_found)", tg.path);
                } else {
                    {
                        let mut p = self.progress.lock().unwrap();
                        p.errors += 1;
                        p.last_error = format!("{} -> {}", tg.path, res.block.as_str());
                    }
                    log::warn!("index skip {} ({})", tg.path, res.block.as_str());
                }
                continue;
            }
            let page = parse_items(&res.text, level, subject, &tg.section, &tg.stream);
            if !page.items.is_empty() {
                added_total += self.upsert_items(&page.items);
            }
        }
        added_total
    }

    pub async fn index_level(&self, level: &str, subject_filter: Option<&str>) -> usize {
        let mut subjects: Vec<&str> = catalog::subjects_for(level).to_vec();
        if let Some(f) = subject_filter {
            subjects.retain(|s| *s == f);
        }
        let total = subjects.len();
        {
            let mut p = self.progress.lock().unwrap();
            *p = Progress {
                running: true,
                level: level.into(),
                done: 0,
                total,
                started_at: Some(Instant::now()),
                ..Default::default()
            };
        }
        let mut added = 0usize;
        for (i, sub) in subjects.iter().enumerate() {
            self.progress.lock().unwrap().subject = (*sub).to_string();
            added += self.index_subject(level, sub).await;
            self.progress.lock().unwrap().done = i + 1;
            tokio::time::sleep(std::time::Duration::from_secs_f64(
                CONFIG.index_section_delay,
            ))
            .await;
        }
        self.progress.lock().unwrap().running = false;
        log::info!("index_level({}): +{} items", level, added);
        added
    }

    pub fn set_running(&self, running: bool) {
        self.progress.lock().unwrap().running = running;
    }

    pub fn is_running(&self) -> bool {
        self.progress.lock().unwrap().running
    }

    // ─── البحث ───────────────────────────────────────────────────────
    pub fn search(&self, query: &str, level: Option<&str>, limit: usize) -> Vec<Item> {
        let db = self.db.lock().unwrap();
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let mut rows: Vec<Item> = Vec::new();

        // بحث FTS ثم fallback إلى LIKE إن لم يجد
        let fts_q: String = q
            .split_whitespace()
            .take(6)
            .map(|t| format!("\"{}\"", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");
        let sql = if level.is_some() {
            "SELECT i.* FROM items_fts f JOIN items i ON i.id = f.rowid \
             WHERE items_fts MATCH ?1 AND i.level = ?2 LIMIT ?3"
        } else {
            "SELECT i.* FROM items_fts f JOIN items i ON i.id = f.rowid \
             WHERE items_fts MATCH ?1 LIMIT ?2"
        };
        if let Ok(mut stmt) = db.prepare(sql) {
            let mapped = if let Some(lv) = level {
                stmt.query_map(params![fts_q, lv, limit as i64], row_to_item)
            } else {
                stmt.query_map(params![fts_q, limit as i64], row_to_item)
            };
            if let Ok(m) = mapped {
                for it in m.flatten() {
                    rows.push(it);
                }
            }
        }

        if rows.is_empty() {
            let like = format!("%{q}%");
            let sql = if level.is_some() {
                "SELECT * FROM items WHERE (title LIKE ?1 OR subject LIKE ?2) AND level = ?3 LIMIT ?4"
            } else {
                "SELECT * FROM items WHERE (title LIKE ?1 OR subject LIKE ?2) LIMIT ?3"
            };
            if let Ok(mut stmt) = db.prepare(sql) {
                let mapped = if let Some(lv) = level {
                    stmt.query_map(params![like, like, lv, limit as i64], row_to_item)
                } else {
                    stmt.query_map(params![like, like, limit as i64], row_to_item)
                };
                if let Ok(m) = mapped {
                    for it in m.flatten() {
                        rows.push(it);
                    }
                }
            }
        }
        rows
    }

    pub fn item_by_token(&self, token: &str) -> Option<Item> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT * FROM items WHERE token = ?1").ok()?;
        let mut rows = stmt.query_map(params![token], row_to_item).ok()?;
        rows.next().and_then(|r| r.ok())
    }

    // ─── المشتركون (نشر تلقائي للجديد) ────────────────────────────────
    /// يضيف محادثة إلى قائمة المشتركين، ويعيد `true` إن كانت جديدة.
    pub fn add_subscriber(&self, chat_id: i64) -> bool {
        let db = self.db.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        db.execute(
            "INSERT OR IGNORE INTO subscribers(chat_id, added_at) VALUES(?1,?2)",
            params![chat_id, now],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// يحذف محادثة من المشتركين، ويعيد `true` إن كانت موجودة.
    pub fn remove_subscriber(&self, chat_id: i64) -> bool {
        let db = self.db.lock().unwrap();
        db.execute(
            "DELETE FROM subscribers WHERE chat_id = ?1",
            params![chat_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn is_subscriber(&self, chat_id: i64) -> bool {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT 1 FROM subscribers WHERE chat_id = ?1",
            params![chat_id],
            |_| Ok(true),
        )
        .unwrap_or(false)
    }

    pub fn subscribers(&self) -> Vec<i64> {
        let db = self.db.lock().unwrap();
        let mut out = Vec::new();
        if let Ok(mut stmt) = db.prepare("SELECT chat_id FROM subscribers") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, i64>(0)) {
                for r in rows.flatten() {
                    out.push(r);
                }
            }
        }
        out
    }

    pub fn subscriber_count(&self) -> i64 {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT COUNT(*) FROM subscribers", [], |r| r.get(0))
            .unwrap_or(0)
    }

    // ─── النشر التلقائي ──────────────────────────────────────────────
    /// عناصر لم تُنشر بعد (وصلت حديثًا) بحدّ أقصى.
    pub fn unpublished_items(&self, limit: usize) -> Vec<Item> {
        let db = self.db.lock().unwrap();
        let mut out = Vec::new();
        if let Ok(mut stmt) =
            db.prepare("SELECT * FROM items WHERE published = 0 ORDER BY id LIMIT ?1")
        {
            if let Ok(rows) = stmt.query_map(params![limit as i64], row_to_item) {
                for r in rows.flatten() {
                    out.push(r);
                }
            }
        }
        out
    }

    /// عدد العناصر المنتظرة للنشر.
    pub fn pending_publish_count(&self) -> i64 {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT COUNT(*) FROM items WHERE published = 0", [], |r| {
            r.get(0)
        })
        .unwrap_or(0)
    }

    /// يعلّم عناصر (بالرمز token) كمُنشَرة.
    pub fn mark_published(&self, tokens: &[String]) {
        if tokens.is_empty() {
            return;
        }
        let mut db = self.db.lock().unwrap();
        let tx = match db.transaction() {
            Ok(t) => t,
            Err(_) => return,
        };
        {
            let mut stmt = match tx.prepare("UPDATE items SET published = 1 WHERE token = ?1") {
                Ok(s) => s,
                Err(_) => return,
            };
            for t in tokens {
                let _ = stmt.execute(params![t]);
            }
        }
        let _ = tx.commit();
    }

    // ─── إحصائيات ────────────────────────────────────────────────────
    pub fn stats(&self) -> Stats {
        let db = self.db.lock().unwrap();
        let total: i64 = db
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap_or(0);
        let mut by_level = Vec::new();
        if let Ok(mut stmt) = db.prepare(
            "SELECT level, COUNT(*) c FROM items WHERE level != '' \
             GROUP BY level ORDER BY c DESC LIMIT 20",
        ) {
            if let Ok(rows) =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            {
                for r in rows.flatten() {
                    by_level.push(r);
                }
            }
        }
        let last_index: Option<i64> = db
            .query_row("SELECT MAX(updated_at) FROM items", [], |r| r.get(0))
            .ok()
            .flatten();
        Stats {
            total,
            by_level,
            last_index,
        }
    }

    pub fn stats_text(&self) -> String {
        let s = self.stats();
        let subs = self.subscriber_count();
        let pending = self.pending_publish_count();
        let mut lines = vec![format!("📚 إجمالي العناصر المفهرسة: {}", s.total)];
        if subs > 0 {
            lines.push(format!("👥 المشتركون: {subs}"));
        }
        if pending > 0 {
            lines.push(format!("🆕 بانتظار النشر: {pending}"));
        }
        for (lv, c) in &s.by_level {
            lines.push(format!("  • {}: {}", catalog::level_name(lv), c));
        }
        if let Some(ts) = s.last_index {
            let dt = chrono::DateTime::from_timestamp(ts, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| ts.to_string());
            lines.push(format!("🕒 آخر فهرسة: {dt}"));
        }
        lines.join("\n")
    }

    pub fn progress_text(&self) -> String {
        let p = self.progress.lock().unwrap();
        if p.running {
            let elapsed = p.started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            return format!(
                "⏳ الفهرسة جارية — المستوى: {}\n📄 المادة الحالية: {}\n✅ {}/{} مادة\n⚠️ أخطاء: {}\n⏱ الزمن: {} ث",
                p.level, p.subject, p.done, p.total, p.errors, elapsed
            );
        }
        if p.done > 0 {
            return format!(
                "✔ اكتملت آخر فهرسة ({})\n✅ {} مادة | ⚠️ {} خطأ\nآخر خطأ: {}",
                p.level,
                p.done,
                p.errors,
                if p.last_error.is_empty() {
                    "—"
                } else {
                    &p.last_error
                }
            );
        }
        "لا توجد فهرسة جارية. استخدم /index <level>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::warp::WarpManager;

    fn temp_indexer(tag: &str) -> Indexer {
        let fetcher = Arc::new(Fetcher::new(Arc::new(WarpManager::new())).unwrap());
        let path = std::env::temp_dir().join(format!("dzx_idx_{}_{}.db", tag, std::process::id()));
        let _ = std::fs::remove_file(&path);
        Indexer::new(fetcher, Some(path.to_string_lossy().into_owned())).unwrap()
    }

    fn item(token: &str) -> Item {
        Item {
            token: token.into(),
            title: format!("عنصر {token}"),
            url: format!("/ar/1as/math/{token}"),
            level: "1as".into(),
            subject: "math".into(),
            ..Default::default()
        }
    }

    #[test]
    fn new_items_are_unpublished_until_marked() {
        let idx = temp_indexer("pub");
        assert_eq!(idx.upsert_items(&[item("a"), item("b")]), 2);
        assert_eq!(idx.pending_publish_count(), 2);
        assert_eq!(idx.unpublished_items(10).len(), 2);

        idx.mark_published(&["a".to_string()]);
        assert_eq!(idx.pending_publish_count(), 1);

        // إعادة الإدراج (تحديث) لا تُلغي علم النشر.
        idx.upsert_items(&[item("a")]);
        assert_eq!(idx.pending_publish_count(), 1);
    }

    #[test]
    fn subscribers_roundtrip() {
        let idx = temp_indexer("subs");
        assert!(idx.add_subscriber(111));
        assert!(!idx.add_subscriber(111));
        assert!(idx.is_subscriber(111));
        assert_eq!(idx.subscriber_count(), 1);
        assert!(idx.remove_subscriber(111));
        assert!(!idx.remove_subscriber(111));
        assert_eq!(idx.subscriber_count(), 0);
    }
}
