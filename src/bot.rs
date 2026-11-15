//! bot.rs — واجهة تيليغرام (الأزرار، الأوامر، الجلسات)
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use once_cell::sync::Lazy;
use regex::Regex;
use teloxide::prelude::*;
use teloxide::types::{
    BotCommand, ChatAction, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile,
    LinkPreviewOptions, Message, MessageId, ParseMode,
};
use url::Url;

use crate::catalog;
use crate::config::{self, CONFIG};
use crate::fetcher::Fetcher;
use crate::indexer::Indexer;
use crate::scraper::Item;
use crate::service::{ContentError, ContentService, SubjectKind};
use crate::warp::WarpManager;

type HandlerResult = anyhow::Result<()>;

static RE_WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").expect("regex"));
static RE_NUM: Lazy<Regex> = Lazy::new(|| Regex::new(r"\d+").expect("regex"));
static RE_FILENAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^\w\u{0600}-\u{06FF} -]").expect("regex"));

const SCHOOL_STREAM_CODES: &[&str] = &["se", "m", "tm", "al", "ge", "tcst", "tcl"];

const WELCOME: &str = "بوت DzExams — دروس وفروض واختبارات ومواضيع شهادات.\n\n\
اذا أعجبك هذا البوت لا تنسى أن تشاركه مع أصدقائك @DZExamBot \
لكي يستفيدو معك فالدال على الخير كفاعله✅💯\n\n\
من تطوير: عبد الرؤوف Dev©\n\n\
▪︎لا تنسوني من صالح دعائكم 💯✅❤️\n\n\
اختر الطور للبدء:";

const SEARCH_HELP: &str =
    "للبحث في المحتوى، اكتب:\n/search كلمة البحث\n\nمثال: /search رياضيات فروض";

// ─── جلسات العرض ─────────────────────────────────────────────────────────
#[derive(Clone, Default)]
pub struct SessionState {
    pub level: String,
    pub subject: String,
    pub section: String,
    pub stream: String,
    pub items: Vec<Item>,
    pub search: bool,
    pub query: String,
    pub page: usize,
    pub key: String,
}

/// مخزن جلسات العرض: مفتاح -> حالة (LRU بسعة محدودة).
pub struct SessionStore {
    cap: usize,
    map: IndexMap<String, SessionState>,
}

impl SessionStore {
    pub fn new(cap: usize) -> Self {
        SessionStore {
            cap,
            map: IndexMap::new(),
        }
    }

    /// يخزّن الحالة ويعيد مفتاحًا عشوائيًا غير قابل للتخمين (ويضبط `key` داخلها).
    pub fn put(&mut self, mut value: SessionState) -> String {
        let mut key = random_hex(4);
        while self.map.contains_key(&key) {
            key = random_hex(4);
        }
        value.key = key.clone();
        self.map.insert(key.clone(), value);
        while self.map.len() > self.cap {
            self.map.shift_remove_index(0);
        }
        key
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut SessionState> {
        if !self.map.contains_key(key) {
            return None;
        }
        let v = self.map.shift_remove(key)?;
        self.map.insert(key.to_string(), v);
        self.map.get_mut(key)
    }

    pub fn get_clone(&mut self, key: &str) -> Option<SessionState> {
        self.get_mut(key).cloned()
    }
}

fn random_hex(n: usize) -> String {
    (0..n)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

// ─── الحالة المشتركة ─────────────────────────────────────────────────────
pub struct BotState {
    pub fetcher: Arc<Fetcher>,
    pub content: Arc<ContentService>,
    pub indexer: Arc<Indexer>,
    pub warp: Arc<WarpManager>,
    pub sessions: Mutex<SessionStore>,
    pub index_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// آخر وقت تنزيل لكل محادثة (حماية من إغراق الموقع).
    pub last_dl: Mutex<HashMap<i64, Instant>>,
}

// ─── أدوات مساعدة ─────────────────────────────────────────────────────────
fn esc(s: &str) -> String {
    html_escape::encode_text(s).into_owned()
}

fn trim_btn(s: &str, n: usize) -> String {
    let collapsed = RE_WS.replace_all(s, " ").trim().to_string();
    if collapsed.chars().count() <= n {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn no_preview() -> LinkPreviewOptions {
    LinkPreviewOptions {
        is_disabled: true,
        url: None,
        prefer_small_media: false,
        prefer_large_media: false,
        show_above_text: false,
    }
}

fn pdf_filename(title: &str) -> String {
    let cleaned: String = RE_FILENAME
        .replace_all(title, "")
        .trim()
        .chars()
        .take(80)
        .collect();
    let cleaned = if cleaned.is_empty() {
        "dzexams".to_string()
    } else {
        cleaned
    };
    format!("{cleaned}.pdf")
}

fn first_number(s: &str) -> i64 {
    RE_NUM
        .find(s)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0)
}

fn item_order_key(d: &Item) -> (i64, i64, String) {
    let year = d.year.parse::<i64>().unwrap_or(0);
    (-year, first_number(&d.title), d.title.clone())
}

fn parse_command(text: &str) -> Option<(String, String)> {
    let t = text.trim_start();
    if !t.starts_with('/') {
        return None;
    }
    let (cmd, rest) = match t.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (t, ""),
    };
    let cmd = cmd.trim_start_matches('/');
    let cmd = cmd.split('@').next().unwrap_or(cmd).to_lowercase();
    Some((cmd, rest.to_string()))
}

fn q_ids(q: &CallbackQuery) -> Option<(ChatId, MessageId)> {
    q.regular_message().map(|m| (m.chat.id, m.id))
}

// ─── لوحات الأزرار ────────────────────────────────────────────────────────
fn stages_markup() -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    for &(st, name, _) in catalog::STAGES {
        rows.push(vec![InlineKeyboardButton::callback(
            name,
            format!("STG:{st}"),
        )]);
    }
    rows.push(vec![InlineKeyboardButton::callback("🔎 بحث", "SEARCH")]);
    InlineKeyboardMarkup::new(rows)
}

fn levels_markup(stage: &str) -> InlineKeyboardMarkup {
    let levels = catalog::STAGES
        .iter()
        .find(|(s, _, _)| *s == stage)
        .map(|(_, _, l)| *l)
        .unwrap_or(&[]);
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    let mut row: Vec<InlineKeyboardButton> = Vec::new();
    for lv in levels {
        row.push(InlineKeyboardButton::callback(
            trim_btn(catalog::level_name(lv), 28),
            format!("L:{lv}"),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![InlineKeyboardButton::callback("🔙 رجوع", "MENU")]);
    InlineKeyboardMarkup::new(rows)
}

fn streams_markup(level: &str) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    for &(code, name) in catalog::streams_for_level(level) {
        rows.push(vec![InlineKeyboardButton::callback(
            trim_btn(name, 40),
            format!("STR:{level}:{code}"),
        )]);
    }
    rows.push(vec![InlineKeyboardButton::callback(
        "🔙 رجوع",
        format!("STG:{}", catalog::stage_of_level(level)),
    )]);
    InlineKeyboardMarkup::new(rows)
}

fn subjects_markup(level: &str, page: usize, stream: &str) -> InlineKeyboardMarkup {
    let subs = catalog::subjects_for(level);
    let per = CONFIG.subjects_per_page;
    let start = page * per;
    let end = ((page + 1) * per).min(subs.len());
    let chunk: &[&str] = if start < subs.len() {
        &subs[start..end]
    } else {
        &[]
    };

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    let mut row: Vec<InlineKeyboardButton> = Vec::new();
    for s in chunk {
        row.push(InlineKeyboardButton::callback(
            trim_btn(catalog::subject_ar(s), 26),
            format!("S:{level}:{s}:{stream}"),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }

    let total_pages = if subs.is_empty() {
        1
    } else {
        subs.len().div_ceil(per)
    };
    let mut nav: Vec<InlineKeyboardButton> = Vec::new();
    if page > 0 {
        nav.push(InlineKeyboardButton::callback(
            "⬅️",
            format!("SP:{level}:{stream}:{}", page - 1),
        ));
    }
    if subs.len() > per {
        nav.push(InlineKeyboardButton::callback(
            format!("{}/{}", page + 1, total_pages),
            "NOOP",
        ));
    }
    if (page + 1) * per < subs.len() {
        nav.push(InlineKeyboardButton::callback(
            "➡️",
            format!("SP:{level}:{stream}:{}", page + 1),
        ));
    }
    if !nav.is_empty() {
        rows.push(nav);
    }

    let back_data = if !stream.is_empty() {
        format!("L:{level}")
    } else {
        format!("STG:{}", catalog::stage_of_level(level))
    };
    rows.push(vec![InlineKeyboardButton::callback("🔙 رجوع", back_data)]);
    InlineKeyboardMarkup::new(rows)
}

fn list_view_markup(state: &SessionState, page: usize) -> InlineKeyboardMarkup {
    let items = &state.items;
    let per = CONFIG.items_per_page;
    let total_pages = items.len().div_ceil(per).max(1);
    let page = page.min(total_pages - 1);
    let key = &state.key;

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    let start = page * per;
    let end = ((page + 1) * per).min(items.len());
    for (i, it) in items.iter().enumerate().take(end).skip(start) {
        let mut label = trim_btn(&it.title, 56);
        if it.solution {
            label.push_str(" ✅");
        }
        rows.push(vec![InlineKeyboardButton::callback(
            label,
            format!("IT:{key}:{i}"),
        )]);
    }

    let mut nav: Vec<InlineKeyboardButton> = Vec::new();
    if page > 0 {
        nav.push(InlineKeyboardButton::callback(
            "⬅️",
            format!("PG:{key}:{}", page - 1),
        ));
    }
    if total_pages > 1 {
        nav.push(InlineKeyboardButton::callback(
            format!("{}/{}", page + 1, total_pages),
            "NOOP",
        ));
    }
    if page < total_pages - 1 {
        nav.push(InlineKeyboardButton::callback(
            "➡️",
            format!("PG:{key}:{}", page + 1),
        ));
    }
    if !nav.is_empty() {
        rows.push(nav);
    }
    rows.push(vec![InlineKeyboardButton::callback(
        "🔙 رجوع",
        format!("BK:{key}"),
    )]);
    rows.push(vec![InlineKeyboardButton::callback(
        "🏠 القائمة الرئيسية",
        "MENU",
    )]);
    InlineKeyboardMarkup::new(rows)
}

fn list_title(state: &SessionState) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !state.stream.is_empty() {
        parts.push(catalog::stream_ar(&state.level, &state.stream).to_string());
    } else if !state.section.is_empty() {
        parts.push(catalog::section_ar(&state.section).to_string());
    }
    parts.push(catalog::subject_ar(&state.subject).to_string());
    format!(
        "📚 {}\n🎓 {}",
        esc(&parts.join(" — ")),
        esc(catalog::level_name(&state.level))
    )
}

fn item_markup(it: &Item, key: &str, site: bool) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    if site && !it.url.is_empty() {
        if let Ok(u) = Url::parse(&format!("{}{}", config::BASE_URL, it.url)) {
            rows.push(vec![InlineKeyboardButton::url("🌐 فتح في الموقع", u)]);
        }
    }
    rows.push(vec![InlineKeyboardButton::callback(
        "🔙 رجوع للقائمة",
        format!("BK:{key}"),
    )]);
    InlineKeyboardMarkup::new(rows)
}

// ─── أدوات تيليغرام ───────────────────────────────────────────────────────
async fn answer(bot: &Bot, q: &CallbackQuery, text: Option<&str>, alert: bool) {
    let mut req = bot.answer_callback_query(q.id.clone());
    if let Some(t) = text {
        req = req.text(t.to_string());
    }
    if alert {
        req = req.show_alert(true);
    }
    let _ = req.await;
}

async fn edit_text(
    bot: &Bot,
    q: &CallbackQuery,
    text: impl Into<String>,
    markup: Option<InlineKeyboardMarkup>,
) -> HandlerResult {
    let (chat, mid) = match q_ids(q) {
        Some(x) => x,
        None => return Ok(()),
    };
    let mut req = bot
        .edit_message_text(chat, mid, text)
        .parse_mode(ParseMode::Html)
        .link_preview_options(no_preview());
    if let Some(m) = markup {
        req = req.reply_markup(m);
    }
    if let Err(e) = req.await {
        log::debug!("edit_message_text: {e}");
    }
    Ok(())
}

// ─── التنقل ──────────────────────────────────────────────────────────────
async fn cb_menu(bot: &Bot, q: &CallbackQuery) -> HandlerResult {
    answer(bot, q, None, false).await;
    edit_text(bot, q, WELCOME, Some(stages_markup())).await
}

async fn cb_search_help(bot: &Bot, q: &CallbackQuery) -> HandlerResult {
    answer(bot, q, None, false).await;
    let kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🏠 القائمة",
        "MENU",
    )]]);
    edit_text(bot, q, SEARCH_HELP, Some(kb)).await
}

async fn cb_stage(bot: &Bot, q: &CallbackQuery) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let stage = data.split(':').nth(1).unwrap_or("").to_string();
    let name = catalog::stage_name(&stage).to_string();
    answer(bot, q, None, false).await;
    edit_text(
        bot,
        q,
        format!("📚 {}\n\nاختر السنة:", esc(&name)),
        Some(levels_markup(&stage)),
    )
    .await
}

async fn cb_level(bot: &Bot, q: &CallbackQuery) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let level = data.split(':').nth(1).unwrap_or("").to_string();
    answer(bot, q, None, false).await;
    let name = catalog::level_name(&level).to_string();
    if !catalog::streams_for_level(&level).is_empty() {
        edit_text(
            bot,
            q,
            format!("🎓 {}\n\nاختر الشعبة:", esc(&name)),
            Some(streams_markup(&level)),
        )
        .await
    } else {
        edit_text(
            bot,
            q,
            format!("📚 {}\n\nاختر المادة:", esc(&name)),
            Some(subjects_markup(&level, 0, "")),
        )
        .await
    }
}

async fn cb_stream_select(bot: &Bot, q: &CallbackQuery) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let mut parts = data.splitn(3, ':');
    let _ = parts.next();
    let level = parts.next().unwrap_or("").to_string();
    let stream = parts.next().unwrap_or("").to_string();
    answer(bot, q, None, false).await;
    let name = catalog::level_name(&level).to_string();
    let sname = catalog::stream_ar(&level, &stream).to_string();
    edit_text(
        bot,
        q,
        format!("🎓 {} — {}\n\nاختر المادة:", esc(&name), esc(&sname)),
        Some(subjects_markup(&level, 0, &stream)),
    )
    .await
}

async fn cb_subjects_page(bot: &Bot, q: &CallbackQuery) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let mut parts = data.splitn(4, ':');
    let _ = parts.next();
    let level = parts.next().unwrap_or("").to_string();
    let stream = parts.next().unwrap_or("").to_string();
    let page: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    answer(bot, q, None, false).await;
    if let Some((chat, mid)) = q_ids(q) {
        let _ = bot
            .edit_message_reply_markup(chat, mid)
            .reply_markup(subjects_markup(&level, page, &stream))
            .await;
    }
    Ok(())
}

async fn cb_subject(bot: &Bot, q: &CallbackQuery, state: &Arc<BotState>) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let parts: Vec<String> = data.splitn(4, ':').map(|s| s.to_string()).collect();
    let level = parts.get(1).cloned().unwrap_or_default();
    let subject = parts.get(2).cloned().unwrap_or_default();
    let stream = parts.get(3).cloned().unwrap_or_default();
    answer(bot, q, Some("جاري التحميل…"), false).await;
    edit_text(bot, q, "جاري التحميل…", None).await?;
    render_subject_screen(bot, q, state, &level, &subject, &stream).await
}

async fn render_subject_screen(
    bot: &Bot,
    q: &CallbackQuery,
    state: &Arc<BotState>,
    level: &str,
    subject: &str,
    stream: &str,
) -> HandlerResult {
    let info = match state.content.subject_page(level, subject).await {
        Ok(i) => i,
        Err(e) => {
            err_edit(bot, q, &e).await;
            return Ok(());
        }
    };

    let sub_name = catalog::subject_ar(subject).to_string();
    let lv_name = catalog::level_name(level).to_string();
    let back_data = if !stream.is_empty() {
        format!("L:{level}")
    } else {
        format!("STG:{}", catalog::stage_of_level(level))
    };

    match info.kind {
        SubjectKind::Streams => {
            let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
            for st in &info.streams {
                rows.push(vec![InlineKeyboardButton::callback(
                    trim_btn(catalog::bac_stream_name(st), 40),
                    format!("ST:{subject}:{st}"),
                )]);
            }
            rows.push(vec![InlineKeyboardButton::callback("🔙 رجوع", back_data)]);
            edit_text(
                bot,
                q,
                format!(
                    "📚 {}\n🎓 {}\n\nاختر الشعبة:",
                    esc(&sub_name),
                    esc(&lv_name)
                ),
                Some(InlineKeyboardMarkup::new(rows)),
            )
            .await
        }
        SubjectKind::Exam => {
            let items = info.items.map(|p| p.items).unwrap_or_default();
            if !items.is_empty() {
                show_items(bot, q, state, level, subject, "", stream, items).await
            } else {
                let rows = vec![vec![InlineKeyboardButton::callback("🔙 رجوع", back_data)]];
                edit_text(
                    bot,
                    q,
                    format!("لا يوجد محتوى بعد في مادة {}.", esc(&sub_name)),
                    Some(InlineKeyboardMarkup::new(rows)),
                )
                .await
            }
        }
        SubjectKind::School => {
            if !info.sections.is_empty() {
                let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
                for sec in &info.sections {
                    rows.push(vec![InlineKeyboardButton::callback(
                        trim_btn(catalog::section_ar(sec), 40),
                        format!("SEC:{level}:{subject}:{sec}:{stream}"),
                    )]);
                }
                rows.push(vec![InlineKeyboardButton::callback("🔙 رجوع", back_data)]);
                edit_text(
                    bot,
                    q,
                    format!("📚 {}\n🎓 {}\n\nاختر القسم:", esc(&sub_name), esc(&lv_name)),
                    Some(InlineKeyboardMarkup::new(rows)),
                )
                .await
            } else {
                let rows = vec![vec![InlineKeyboardButton::callback("🔙 رجوع", back_data)]];
                edit_text(
                    bot,
                    q,
                    format!("لا يوجد محتوى بعد في مادة {}.", esc(&sub_name)),
                    Some(InlineKeyboardMarkup::new(rows)),
                )
                .await
            }
        }
    }
}

async fn cb_section(bot: &Bot, q: &CallbackQuery, state: &Arc<BotState>) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let parts: Vec<String> = data.splitn(5, ':').map(|s| s.to_string()).collect();
    let level = parts.get(1).cloned().unwrap_or_default();
    let subject = parts.get(2).cloned().unwrap_or_default();
    let section = parts.get(3).cloned().unwrap_or_default();
    let stream = parts.get(4).cloned().unwrap_or_default();
    answer(bot, q, Some("جاري التحميل…"), false).await;
    edit_text(bot, q, "جاري التحميل…", None).await?;
    let page = match state
        .content
        .section_items(&level, &subject, &section, "")
        .await
    {
        Ok(p) => p,
        Err(e) => {
            err_edit(bot, q, &e).await;
            return Ok(());
        }
    };
    show_items(
        bot, q, state, &level, &subject, &section, &stream, page.items,
    )
    .await
}

async fn cb_stream(bot: &Bot, q: &CallbackQuery, state: &Arc<BotState>) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let parts: Vec<String> = data.splitn(3, ':').map(|s| s.to_string()).collect();
    let subject = parts.get(1).cloned().unwrap_or_default();
    let stream = parts.get(2).cloned().unwrap_or_default();
    answer(bot, q, Some("جاري التحميل…"), false).await;
    edit_text(bot, q, "جاري التحميل…", None).await?;
    let page = match state
        .content
        .section_items("bac", &subject, "", &stream)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            err_edit(bot, q, &e).await;
            return Ok(());
        }
    };
    show_items(bot, q, state, "bac", &subject, "", &stream, page.items).await
}

// ─── عرض قائمة العناصر ────────────────────────────────────────────────────
#[allow(clippy::too_many_arguments)]
async fn show_items(
    bot: &Bot,
    q: &CallbackQuery,
    state: &Arc<BotState>,
    level: &str,
    subject: &str,
    section: &str,
    stream: &str,
    mut items: Vec<Item>,
) -> HandlerResult {
    if SCHOOL_STREAM_CODES.contains(&stream) {
        // الشعبة ترتّب ولا تُخفي: ملفات الشعبة (أو المشتركة بلا وسوم) أولًا،
        // ثم بقية ملفات المادة كما يعرضها الموقع.
        items.sort_by(|a, b| {
            let ka = if a.tags.is_empty() || a.tags.iter().any(|t| t == stream) {
                0
            } else {
                1
            };
            let kb = if b.tags.is_empty() || b.tags.iter().any(|t| t == stream) {
                0
            } else {
                1
            };
            ka.cmp(&kb)
                .then_with(|| item_order_key(a).cmp(&item_order_key(b)))
        });
    } else {
        items.sort_by_key(item_order_key);
    }

    if items.is_empty() {
        let back_data = format!("S:{level}:{subject}:{stream}");
        let empty_text = if !stream.is_empty() {
            "لا توجد مستندات لهذه الشعبة في هذا القسم."
        } else {
            "لا توجد مستندات في هذا القسم حاليًا."
        };
        let rows = vec![vec![InlineKeyboardButton::callback("🔙 رجوع", back_data)]];
        return edit_text(bot, q, empty_text, Some(InlineKeyboardMarkup::new(rows))).await;
    }

    let count = items.len();
    let mut st = SessionState {
        level: level.to_string(),
        subject: subject.to_string(),
        section: section.to_string(),
        stream: stream.to_string(),
        items,
        ..Default::default()
    };
    let key = state.sessions.lock().unwrap().put(st.clone());
    st.key = key;
    let text = format!(
        "{}\n\nعدد المستندات: {}\nاضغط على أي مستند لعرضه.",
        list_title(&st),
        count
    );
    edit_text(bot, q, text, Some(list_view_markup(&st, 0))).await
}

async fn cb_page(bot: &Bot, q: &CallbackQuery, state: &Arc<BotState>) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let mut parts = data.splitn(3, ':');
    let _ = parts.next();
    let key = parts.next().unwrap_or("").to_string();
    let page: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let markup = {
        let mut store = state.sessions.lock().unwrap();
        match store.get_mut(&key) {
            Some(s) => {
                s.page = page;
                Some(list_view_markup(s, page))
            }
            None => None,
        }
    };
    let markup = match markup {
        Some(m) => m,
        None => {
            answer(
                bot,
                q,
                Some("انتهت هذه القائمة. ابدأ من جديد بـ /start."),
                true,
            )
            .await;
            return Ok(());
        }
    };
    answer(bot, q, None, false).await;
    if let Some((chat, mid)) = q_ids(q) {
        let _ = bot
            .edit_message_reply_markup(chat, mid)
            .reply_markup(markup)
            .await;
    }
    Ok(())
}

async fn back_to_list_caption(bot: &Bot, q: &CallbackQuery, st: &SessionState) -> HandlerResult {
    let text = if st.search {
        format!(
            "🔎 نتائج البحث عن: {}\nعدد النتائج: {} — اضغط على أي نتيجة لعرضها.",
            esc(&st.query),
            st.items.len()
        )
    } else {
        format!(
            "{}\n\nعدد المستندات: {}\nاضغط على أي مستند لعرضه.",
            list_title(st),
            st.items.len()
        )
    };
    let markup = list_view_markup(st, st.page);
    let (chat, mid) = match q_ids(q) {
        Some(x) => x,
        None => return Ok(()),
    };
    let r = bot
        .edit_message_caption(chat, mid)
        .caption(text.clone())
        .parse_mode(ParseMode::Html)
        .reply_markup(markup.clone())
        .await;
    if r.is_err() {
        // التعليق مطابق أو غير قابل للتعديل — أرسل القائمة رسالة جديدة.
        let _ = bot.edit_message_reply_markup(chat, mid).await;
        let _ = bot
            .send_message(chat, text)
            .parse_mode(ParseMode::Html)
            .reply_markup(markup)
            .link_preview_options(no_preview())
            .await;
    }
    Ok(())
}

async fn cb_back(bot: &Bot, q: &CallbackQuery, state: &Arc<BotState>) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let key = data.split(':').nth(1).unwrap_or("").to_string();
    let st = {
        // نُسقط القفل قبل أي .await (وإلا يصبح المستقبل غير Send)
        let mut store = state.sessions.lock().unwrap();
        store.get_clone(&key)
    };
    let st = match st {
        Some(s) => s,
        None => {
            answer(
                bot,
                q,
                Some("انتهت هذه القائمة. ابدأ من جديد بـ /start."),
                true,
            )
            .await;
            return Ok(());
        }
    };

    // زر الرجوع المرفق مع ملف PDF: الرسالة وثيقة (لها تعليق لا نص).
    let is_media = q
        .regular_message()
        .map(|m| m.document().is_some() || m.photo().is_some() || m.video().is_some())
        .unwrap_or(false);
    if is_media {
        answer(bot, q, None, false).await;
        return back_to_list_caption(bot, q, &st).await;
    }
    if st.search {
        answer(bot, q, None, false).await;
        return edit_text(bot, q, "استخدم /search للبحث مجددًا أو /start للتصفح.", None).await;
    }
    answer(bot, q, None, false).await;

    let level = st.level.clone();
    let subject = st.subject.clone();
    let stream = st.stream.clone();
    if catalog::EXAM_LEVELS.contains(&level.as_str()) && st.section.is_empty() && stream.is_empty()
    {
        return edit_text(
            bot,
            q,
            format!("📚 {}\n\nاختر المادة:", esc(catalog::level_name(&level))),
            Some(subjects_markup(&level, 0, "")),
        )
        .await;
    }
    render_subject_screen(bot, q, state, &level, &subject, &stream).await
}

// ─── اختيار عنصر وإرسال PDF ──────────────────────────────────────────────
async fn cb_item(bot: &Bot, q: &CallbackQuery, state: &Arc<BotState>) -> HandlerResult {
    let data = q.data.clone().unwrap_or_default();
    let mut parts = data.splitn(3, ':');
    let _ = parts.next();
    let key = parts.next().unwrap_or("").to_string();
    let idx: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let st = {
        // نُسقط القفل قبل أي .await (وإلا يصبح المستقبل غير Send)
        let mut store = state.sessions.lock().unwrap();
        store.get_clone(&key)
    };
    let st = match st {
        Some(s) => s,
        None => {
            answer(
                bot,
                q,
                Some("انتهت هذه القائمة. ابدأ من جديد بـ /start."),
                true,
            )
            .await;
            return Ok(());
        }
    };
    if idx >= st.items.len() {
        answer(bot, q, Some("العنصر غير متوفر."), true).await;
        return Ok(());
    }
    let it = st.items[idx].clone();

    let (chat, _mid) = match q_ids(q) {
        Some(x) => x,
        None => return Ok(()),
    };

    // حد التحميل لكل مستخدم: مهلة بين ملف وآخر (حماية من إغراق الموقع)
    let now = Instant::now();
    let wait = {
        // نُسقط القفل قبل أي .await (وإلا يصبح المستقبل غير Send)
        let mut map = state.last_dl.lock().unwrap();
        let prev = map.get(&chat.0).copied();
        let wait = prev.and_then(|prev| {
            let elapsed = now.duration_since(prev).as_secs();
            if elapsed < CONFIG.user_dl_cooldown {
                Some(CONFIG.user_dl_cooldown - elapsed)
            } else {
                None
            }
        });
        if wait.is_none() {
            map.insert(chat.0, now);
        }
        wait
    };
    if let Some(wait) = wait {
        answer(
            bot,
            q,
            Some(&format!("انتظر {wait} ثوانٍ ثم أعد المحاولة.")),
            true,
        )
        .await;
        return Ok(());
    }
    answer(bot, q, Some("جاري التحميل…"), false).await;

    let _ = bot.send_chat_action(chat, ChatAction::UploadDocument).await;

    let mut detail_title = it.title.clone();
    let mut pdf_url = String::new();
    let mut pdf_path: Option<PathBuf> = None;

    match state.content.item_detail(&it).await {
        Ok(detail) => {
            if let Some(u) = detail.pdf_url {
                pdf_url = u;
            }
            if let Some(t) = detail.title {
                if !t.is_empty() {
                    detail_title = t;
                }
            }
        }
        Err(e) => {
            let kb = item_markup(&it, &key, true);
            let _ = bot
                .send_message(chat, esc(&e.msg_ar))
                .parse_mode(ParseMode::Html)
                .reply_markup(kb)
                .await;
            return Ok(());
        }
    }
    if !pdf_url.is_empty() {
        pdf_path = state.content.download_pdf_from_url(&it, &pdf_url).await;
    }

    let has_solution = it.solution;
    let mut caption = format!(
        "📄 <b>{}</b>\n🎓 {} | {}",
        esc(&trim_btn(&detail_title, 120)),
        esc(catalog::level_name(&it.level)),
        esc(catalog::subject_ar(&it.subject)),
    );
    if !it.year.is_empty() {
        caption.push_str(&format!(" | سنة {}", esc(&it.year)));
    }
    caption.push('\n');
    caption.push_str(if has_solution {
        "✅ الحل مرفق مع الملف."
    } else {
        "الحل غير متوفر لهذا الملف."
    });

    // حد تيليغرام: ملف أكبر من 49MB يُرسل كرابط لا كمستند
    let mut oversize = false;
    if let Some(p) = &pdf_path {
        if let Ok(meta) = std::fs::metadata(p) {
            if meta.len() > config::TELEGRAM_DOC_LIMIT {
                oversize = true;
            }
        }
        if oversize {
            let _ = std::fs::remove_file(p);
            pdf_path = None;
        }
    }

    if let Some(p) = pdf_path {
        let file = InputFile::file(p.clone()).file_name(pdf_filename(&detail_title));
        let _ = bot
            .send_document(chat, file)
            .caption(caption)
            .parse_mode(ParseMode::Html)
            .reply_markup(item_markup(&it, &key, true))
            .await;
        let _ = std::fs::remove_file(&p);
    } else {
        let kb = item_markup(&it, &key, true);
        let note = if oversize {
            "الملف كبير (أكثر من 49MB) — لا يمكن إرساله عبر تيليغرام.\nافتحه من زر الموقع بالأسفل."
        } else {
            "لم أستطع تنزيل ملف PDF مباشرة.\nافتحه من زر الموقع بالأسفل."
        };
        let _ = bot.send_message(chat, note).reply_markup(kb).await;
    }
    Ok(())
}

async fn err_edit(bot: &Bot, q: &CallbackQuery, e: &ContentError) {
    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::url(
            "🌐 فتح الموقع",
            Url::parse(&format!("{}/ar", config::BASE_URL)).expect("valid url"),
        ),
        InlineKeyboardButton::callback("🏠 القائمة", "MENU"),
    ]]);
    let _ = edit_text(bot, q, esc(&e.msg_ar), Some(kb)).await;
}

// ─── البحث المحلي ────────────────────────────────────────────────────────
async fn cmd_search(bot: &Bot, msg: &Message, state: &Arc<BotState>, args: &str) -> HandlerResult {
    let query = args.split_whitespace().collect::<Vec<_>>().join(" ");
    if query.is_empty() {
        bot.send_message(
            msg.chat.id,
            "للبحث، اكتب:\n/search كلمة البحث\n\nمثال: /search رياضيات فروض",
        )
        .await?;
        return Ok(());
    }
    let status = bot.send_message(msg.chat.id, "جاري البحث…").await?;
    let results = state
        .indexer
        .search(&query, None, CONFIG.search_results_limit);
    if results.is_empty() {
        let _ = bot
            .edit_message_text(msg.chat.id, status.id, "لا نتائج مطابقة. جرّب كلمات أخرى.")
            .await;
        return Ok(());
    }
    let mut st = SessionState {
        items: results,
        search: true,
        query: query.clone(),
        ..Default::default()
    };
    let key = state.sessions.lock().unwrap().put(st.clone());
    st.key = key;
    let text = format!(
        "🔎 نتائج البحث عن: {}\nعدد النتائج: {} — اضغط على أي نتيجة لعرضها.",
        esc(&query),
        st.items.len()
    );
    let _ = bot
        .edit_message_text(msg.chat.id, status.id, text)
        .parse_mode(ParseMode::Html)
        .link_preview_options(no_preview())
        .reply_markup(list_view_markup(&st, 0))
        .await;
    Ok(())
}

// ─── أوامر المشرفين ──────────────────────────────────────────────────────
fn is_admin(msg: &Message) -> bool {
    let uid = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    CONFIG.admin_ids.contains(&uid)
}

async fn admin_only(bot: &Bot, msg: &Message) -> bool {
    if !is_admin(msg) {
        let _ = bot
            .send_message(msg.chat.id, "هذا الأمر للمشرفين فقط.")
            .await;
        return false;
    }
    true
}

async fn cmd_ip(bot: &Bot, msg: &Message, state: &Arc<BotState>) -> HandlerResult {
    if !admin_only(bot, msg).await {
        return Ok(());
    }
    let desc = state.warp.describe().await;
    bot.send_message(
        msg.chat.id,
        format!("🌐 {desc}\n\n📊 {}", state.fetcher.stats_line()),
    )
    .await?;
    Ok(())
}

async fn cmd_status(bot: &Bot, msg: &Message, state: &Arc<BotState>) -> HandlerResult {
    if !admin_only(bot, msg).await {
        return Ok(());
    }
    bot.send_message(msg.chat.id, state.indexer.stats_text())
        .await?;
    Ok(())
}

async fn cmd_index(bot: &Bot, msg: &Message, state: &Arc<BotState>, args: &str) -> HandlerResult {
    if !admin_only(bot, msg).await {
        return Ok(());
    }
    let level = args.split_whitespace().next().unwrap_or("").to_lowercase();
    if !catalog::is_valid_level(&level) {
        let levels = catalog::LEVELS
            .iter()
            .map(|(l, _)| *l)
            .collect::<Vec<_>>()
            .join(", ");
        bot.send_message(
            msg.chat.id,
            format!("الاستخدام: /index <مستوى>\nالمستويات: {levels}"),
        )
        .await?;
        return Ok(());
    }
    if state.indexer.is_running() {
        bot.send_message(
            msg.chat.id,
            "توجد فهرسة جارية بالفعل. تفقّد /idxstatus أو أوقفها /idxstop",
        )
        .await?;
        return Ok(());
    }
    bot.send_message(msg.chat.id, format!("بدأت فهرسة مستوى {level} في الخلفية…"))
        .await?;
    spawn_index(state, bot, msg.chat.id, vec![level]);
    Ok(())
}

async fn cmd_indexall(bot: &Bot, msg: &Message, state: &Arc<BotState>) -> HandlerResult {
    if !admin_only(bot, msg).await {
        return Ok(());
    }
    if state.indexer.is_running() {
        bot.send_message(msg.chat.id, "توجد فهرسة جارية.").await?;
        return Ok(());
    }
    bot.send_message(
        msg.chat.id,
        "بدأت فهرسة كل المستويات — العملية طويلة وتجري في الخلفية.",
    )
    .await?;
    let levels: Vec<String> = catalog::LEVELS
        .iter()
        .map(|(l, _)| (*l).to_string())
        .collect();
    spawn_index(state, bot, msg.chat.id, levels);
    Ok(())
}

fn spawn_index(state: &Arc<BotState>, bot: &Bot, chat: ChatId, levels: Vec<String>) {
    let state2 = state.clone();
    let bot2 = bot.clone();
    let handle = tokio::spawn(async move {
        for lv in &levels {
            state2.indexer.index_level(lv, None).await;
        }
        state2.indexer.set_running(false);
        let text = format!("اكتملت الفهرسة.\n\n{}", state2.indexer.stats_text());
        let _ = bot2.send_message(chat, text).await;
    });
    *state.index_task.lock().unwrap() = Some(handle);
}

async fn cmd_idxstatus(bot: &Bot, msg: &Message, state: &Arc<BotState>) -> HandlerResult {
    if !admin_only(bot, msg).await {
        return Ok(());
    }
    bot.send_message(msg.chat.id, state.indexer.progress_text())
        .await?;
    Ok(())
}

async fn cmd_idxstop(bot: &Bot, msg: &Message, state: &Arc<BotState>) -> HandlerResult {
    if !admin_only(bot, msg).await {
        return Ok(());
    }
    let stopped = {
        // نُسقط القفل داخل نطاق محدود قبل أي .await
        let guard = state.index_task.lock().unwrap();
        match guard.as_ref() {
            Some(h) if !h.is_finished() => {
                h.abort();
                true
            }
            _ => false,
        }
    };
    if stopped {
        state.indexer.set_running(false);
    }
    let text = if stopped {
        "أوقفت الفهرسة."
    } else {
        "لا توجد فهرسة جارية."
    };
    bot.send_message(msg.chat.id, text).await?;
    Ok(())
}

// ─── الفهرسة التلقائية اليومية ────────────────────────────────────────────
async fn auto_index_loop(state: Arc<BotState>, bot: Bot) {
    if CONFIG.auto_index_levels.is_empty() {
        return;
    }
    loop {
        let now = chrono::Local::now();
        let today = now.date_naive();
        let target = match today.and_hms_opt(CONFIG.auto_index_hour, 0, 0) {
            Some(t) if t > now.naive_local() => t,
            _ => match today.succ_opt() {
                Some(d) => d
                    .and_hms_opt(CONFIG.auto_index_hour, 0, 0)
                    .unwrap_or(today.and_hms_opt(0, 0, 0).unwrap()),
                None => now.naive_local(),
            },
        };
        let dur = (target - now.naive_local())
            .to_std()
            .unwrap_or(Duration::from_secs(60));
        log::info!(
            "auto-index scheduled in {} min for {:?}",
            dur.as_secs() / 60,
            CONFIG.auto_index_levels
        );
        tokio::time::sleep(dur).await;

        if state.indexer.is_running() {
            continue;
        }
        for lv in &CONFIG.auto_index_levels {
            if catalog::is_valid_level(lv) {
                state.indexer.index_level(lv, None).await;
            }
        }
        let _ = bot
            .send_message(
                ChatId(CONFIG.admin_ids.iter().next().copied().unwrap_or(0)),
                "اكتملت الفهرسة التلقائية.".to_string(),
            )
            .await;
    }
}

// ─── الراوتر ──────────────────────────────────────────────────────────────
async fn on_message(bot: Bot, msg: Message, state: Arc<BotState>) -> ResponseResult<()> {
    let text = msg.text().unwrap_or("").to_string();
    match parse_command(&text) {
        Some((cmd, args)) => {
            let result: HandlerResult = match cmd.as_str() {
                "start" => {
                    bot.send_message(msg.chat.id, WELCOME)
                        .reply_markup(stages_markup())
                        .link_preview_options(no_preview())
                        .await?;
                    Ok(())
                }
                "search" => cmd_search(&bot, &msg, &state, &args).await,
                "index" => cmd_index(&bot, &msg, &state, &args).await,
                "indexall" => cmd_indexall(&bot, &msg, &state).await,
                "idxstatus" => cmd_idxstatus(&bot, &msg, &state).await,
                "idxstop" => cmd_idxstop(&bot, &msg, &state).await,
                "ip" => cmd_ip(&bot, &msg, &state).await,
                "status" => cmd_status(&bot, &msg, &state).await,
                _ => return Ok(()),
            };
            if let Err(e) = result {
                log::error!("command {cmd} failed: {e:#}");
                let _ = bot
                    .send_message(msg.chat.id, "حدث خطأ بسيط. حاول مجددًا.")
                    .await;
            }
        }
        None => {
            if !text.trim().is_empty() {
                let _ = bot
                    .send_message(msg.chat.id, "اكتب /start للبدء، أو اختر من القائمة.")
                    .await;
            }
        }
    }
    Ok(())
}

async fn on_callback(bot: Bot, q: CallbackQuery, state: Arc<BotState>) -> ResponseResult<()> {
    let data = q.data.clone().unwrap_or_default();
    if data == "NOOP" {
        answer(&bot, &q, None, false).await;
        return Ok(());
    }
    let route = data.split(':').next().unwrap_or("").to_string();
    let result: HandlerResult = match route.as_str() {
        "MENU" => cb_menu(&bot, &q).await,
        "SEARCH" => cb_search_help(&bot, &q).await,
        "STG" => cb_stage(&bot, &q).await,
        "L" => cb_level(&bot, &q).await,
        "STR" => cb_stream_select(&bot, &q).await,
        "SP" => cb_subjects_page(&bot, &q).await,
        "S" => cb_subject(&bot, &q, &state).await,
        "SEC" => cb_section(&bot, &q, &state).await,
        "ST" => cb_stream(&bot, &q, &state).await,
        "PG" => cb_page(&bot, &q, &state).await,
        "IT" => cb_item(&bot, &q, &state).await,
        "BK" => cb_back(&bot, &q, &state).await,
        _ => {
            answer(&bot, &q, None, false).await;
            return Ok(());
        }
    };
    if let Err(e) = result {
        log::error!("callback {route} failed: {e:#}");
        answer(&bot, &q, Some("حدث خطأ بسيط. حاول مجددًا."), true).await;
    }
    Ok(())
}

// ─── التشغيل ──────────────────────────────────────────────────────────────
pub async fn run() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    if CONFIG.bot_token.is_empty() {
        anyhow::bail!("❌ BOT_TOKEN غير مضبوط. ضعه في ملف .env أو متغيرات البيئة.");
    }
    std::fs::create_dir_all(&CONFIG.tmp_dir)?;

    let warp = Arc::new(WarpManager::new());
    let fetcher = Arc::new(Fetcher::new(warp.clone())?);
    let content = Arc::new(ContentService::new(fetcher.clone()));
    let indexer = Arc::new(Indexer::new(fetcher.clone(), None)?);

    let state = Arc::new(BotState {
        fetcher,
        content,
        indexer,
        warp,
        sessions: Mutex::new(SessionStore::new(400)),
        index_task: Mutex::new(None),
        last_dl: Mutex::new(HashMap::new()),
    });

    let bot = Bot::new(&CONFIG.bot_token);
    let _ = bot
        .set_my_commands(vec![
            BotCommand::new("start", "القائمة الرئيسية"),
            BotCommand::new("search", "بحث في المحتوى"),
        ])
        .await;

    tokio::spawn(auto_index_loop(state.clone(), bot.clone()));

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(on_message))
        .branch(Update::filter_callback_query().endpoint(on_callback));

    log::info!("Starting DzExams bot…");
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}
