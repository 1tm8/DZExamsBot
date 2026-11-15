//! scraper.rs — استخراج البيانات من صفحات dzexams.com
//!
//! يستخرج:
//!   - روابط المواد من صفحة المستوى
//!   - الأقسام (cours/d1..e3/evaluations) من صفحة المادة
//!   - الشعب (se/m/mt/ges/lit) من صفحات البكالوريا
//!   - العناصر (data-id + الصنف + العنوان) من صفحات الأقسام/الشهادات
//!   - رابط PDF ورابط المستند من صفحة العنصر
//!   - العنوان وسنة الموضوع
use std::collections::BTreeSet;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::config::BASE_URL;
use crate::dzenc::{content_path, dz_decode};

static RE_TITLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)<title>\s*(.*?)\s*</title>").expect("regex"));
static RE_YEAR: Lazy<Regex> = Lazy::new(|| Regex::new(r"(20\d\d)").expect("regex"));
static RE_YEAR_ITEM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"📅(?:<[^>]+>|\s)*(\d{4})").expect("regex"));
static RE_DESCRIPTION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"<meta name="description" content="([^"]*)""#).expect("regex"));
static RE_DATA_SRC_FRAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"data-src="(https?://[^"]+)""#).expect("regex"));
static RE_PDF: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"href="(https?://[^"\s]+/uploads/[^"\s]+\.pdf[^"\s]*)"#).expect("regex")
});
static RE_DATA_TAGS: Lazy<Regex> = Lazy::new(|| Regex::new(r"data-tags='([^']*)'").expect("regex"));

static RE_TAG_STRIP: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").expect("regex"));
static RE_WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").expect("regex"));
static RE_ANCHOR: Lazy<Regex> = Lazy::new(|| Regex::new(r"<a\b([^>]*)>").expect("regex"));
static RE_ANCHOR_POS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"<a\b[^>]*data-id="[^"]+"[^>]*>"#).expect("regex"));
static RE_TITLE_BLOCK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)<div class="(?:sujet-title|document-title|item-title|annale-title)"[^>]*>(.*?)</div>"#)
        .expect("regex")
});
static RE_ATTR_DATA_ID: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"data-id="([^"]+)""#).expect("regex"));
static RE_ATTR_KIND: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"btn-item-(sujet|document|article|annale)").expect("regex"));

/// إزالة وسوم HTML وضغط الفراغات.
pub fn clean_text(s: &str) -> String {
    let no_tags = RE_TAG_STRIP.replace_all(s, " ");
    RE_WS.replace_all(&no_tags, " ").trim().to_string()
}

fn safe_slice(s: &str, start: usize, end: usize) -> &str {
    let mut st = start.min(s.len());
    let mut en = end.min(s.len());
    while en > st && !s.is_char_boundary(en) {
        en -= 1;
    }
    while st < en && !s.is_char_boundary(st) {
        st += 1;
    }
    if st >= en {
        ""
    } else {
        &s[st..en]
    }
}

#[derive(Debug, Clone, Default)]
pub struct Item {
    /// sujets / documents / articles / annales
    pub kind: String,
    /// رمز الرابط بعد فك dzEnc
    pub token: String,
    pub title: String,
    /// رابط الصفحة الكامل (بدون النطاق)
    pub url: String,
    pub pdf_url: String,
    pub level: String,
    pub subject: String,
    pub section: String,
    pub stream: String,
    pub year: String,
    /// الموقع يوثّق حلًا لهذا الملف (data-solution="1")
    pub solution: bool,
    /// الشعب المرتبطة (data-tags)
    pub tags: Vec<String>,
}

impl Item {
    pub fn site_url(&self) -> String {
        format!("{BASE_URL}{}", self.url)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SectionPage {
    pub items: Vec<Item>,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub struct ItemDetail {
    pub title: Option<String>,
    pub description: Option<String>,
    pub pdf_url: Option<String>,
    pub all_pdfs: Vec<String>,
    pub viewer_url: Option<String>,
}

// ─────────────────────────── المستويات والمواد ─────────────────────────────
/// استخراج مواد المستوى من صفحة المستوى.
pub fn parse_subjects(level: &str, html: &str) -> Vec<String> {
    let pat = format!(
        r#"href="https?://www\.dzexams\.com/ar{}/([a-z0-9\-]+)""#,
        level_re(level)
    );
    let re = match Regex::new(&pat) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    const BAD_PREFIXES: &[&str] = &[
        "sujets",
        "documents",
        "articles",
        "annales",
        "cours",
        "evaluations",
        "e1",
        "e2",
        "e3",
        "d1",
        "d2",
        "d3",
        "forum",
        "upload",
        "profile",
        "university",
        "teachers",
        "stories",
        "islam",
        "advice",
        "enigmes",
        "faqs",
        "olympiades",
        "filieres",
    ];
    let mut out = BTreeSet::new();
    for c in re.captures_iter(html) {
        let s = &c[1];
        if BAD_PREFIXES.iter().any(|p| s.starts_with(p))
            || matches!(s, "se" | "m" | "mt" | "ges" | "lit")
        {
            continue;
        }
        out.insert(s.to_string());
    }
    out.into_iter().collect()
}

/// أقسام المادة (للمستويات الدراسية فقط).
pub fn parse_sections(level: &str, subject: &str, html: &str) -> Vec<String> {
    let pat = format!(
        r#"href="https?://www\.dzexams\.com/ar{}/{}/(cours|d[123]|e[123]|evaluations)""#,
        level_re(level),
        level_re(subject)
    );
    let re = match Regex::new(&pat) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut set = BTreeSet::new();
    for c in re.captures_iter(html) {
        set.insert(c[1].to_string());
    }
    set.into_iter().collect()
}

/// شعب البكالوريا للمادة.
pub fn parse_streams(_level: &str, subject: &str, html: &str) -> Vec<String> {
    let pat = format!(
        r#"href="https?://www\.dzexams\.com/ar/bac{}/(se|m|mt|ges|lit)""#,
        level_re(subject)
    );
    let re = match Regex::new(&pat) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut set = BTreeSet::new();
    for c in re.captures_iter(html) {
        set.insert(c[1].to_string());
    }
    set.into_iter().collect()
}

/// هروب مسار يُدرج داخل تعبير نمطي (مكافئ `re.escape`).
fn level_re(level: &str) -> String {
    regex::escape(level)
}

// ─────────────────────────── العناصر ──────────────────────────────────────
/// يستخرج أزواج (data-id, kind) بالترتيب من الصفحة.
fn extract_items_raw(html: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for m in RE_ANCHOR.captures_iter(html) {
        let attrs = &m[1];
        let did = RE_ATTR_DATA_ID.captures(attrs);
        let kind = RE_ATTR_KIND.captures(attrs);
        if let (Some(d), Some(k)) = (did, kind) {
            pairs.push((d[1].to_string(), k[1].to_string()));
        }
    }
    pairs
}

/// استخراج عناصر القائمة من صفحة قسم/شهادة.
pub fn parse_items(
    html: &str,
    level: &str,
    subject: &str,
    section: &str,
    stream: &str,
) -> SectionPage {
    let mut page = SectionPage::default();
    if let Some(t) = RE_TITLE.captures(html) {
        page.title = clean_text(&t[1]).replace("| DzExams", "");
        page.title = page.title.trim().to_string();
    }
    if let Some(d) = RE_DESCRIPTION.captures(html) {
        page.description = clean_text(&d[1]);
    }

    let pairs = extract_items_raw(html);
    if pairs.is_empty() {
        return page;
    }

    // مواضع نهايات روابط العناصر
    let positions: Vec<usize> = RE_ANCHOR_POS.find_iter(html).map(|m| m.end()).collect();
    // كتل العناوين
    let title_blocks: Vec<_> = RE_TITLE_BLOCK.captures_iter(html).collect();

    let mut used: BTreeSet<usize> = BTreeSet::new();
    for (i, (did, kind)) in pairs.iter().enumerate() {
        let token = dz_decode(did);
        if token.is_empty() {
            continue;
        }
        let path = content_path(&format!("btn-item-{kind}"), &token, "ar");
        if path.is_empty() {
            continue;
        }
        let start = positions.get(i).copied().unwrap_or(0);
        let mut title = String::new();
        for (j, tb) in title_blocks.iter().enumerate() {
            if used.contains(&j) {
                continue;
            }
            let tb_start = tb.get(0).map(|m| m.start()).unwrap_or(0);
            if tb_start + 200 < start {
                continue;
            }
            if tb_start > start + 2500 {
                break;
            }
            title = clean_text(tb.get(1).map(|m| m.as_str()).unwrap_or(""));
            used.insert(j);
            break;
        }
        if title.is_empty() {
            title = if page.title.is_empty() {
                "مستند".to_string()
            } else {
                page.title.clone()
            };
        }
        // تنظيف أسماء مكررة من رأس الصفحة (اقتصاص الطول)
        title = title.chars().take(120).collect();

        // السنة: من المقطع التالي للرابط (📅 ثم السنة)، وإلا من العنوان
        let seg_end = positions.get(i + 1).copied().unwrap_or(start + 2500);
        let segment = safe_slice(html, start, seg_end);
        let year = if let Some(ym) = RE_YEAR_ITEM.captures(segment) {
            ym[1].to_string()
        } else if let Some(ym) = RE_YEAR.captures(&title) {
            ym[1].to_string()
        } else {
            String::new()
        };

        let (solution, tags) = container_flags(html, start);
        let plural = match kind.as_str() {
            "sujet" => "sujets",
            "document" => "documents",
            "article" => "articles",
            "annale" => "annales",
            _ => "sujets",
        };
        page.items.push(Item {
            kind: plural.to_string(),
            token,
            title: if title.is_empty() {
                "مستند".to_string()
            } else {
                title
            },
            url: path,
            pdf_url: String::new(),
            level: level.to_string(),
            subject: subject.to_string(),
            section: section.to_string(),
            stream: stream.to_string(),
            year,
            solution,
            tags,
        });
    }
    page
}

/// الحل والشعب من حاوية العنصر `<div class="... sujet-filter-item"
/// data-solution="1" data-tags='["se"]'>` التي تسبق الرابط مباشرة.
fn container_flags(html: &str, anchor_end: usize) -> (bool, Vec<String>) {
    let anchor_end = anchor_end.min(html.len());
    let before = &html[..anchor_end];
    let div_start = match before.rfind("<div") {
        Some(i) => i,
        None => return (false, Vec::new()),
    };
    let tag_end = match html[div_start..].find('>') {
        Some(i) => div_start + i,
        None => return (false, Vec::new()),
    };
    if tag_end > anchor_end {
        return (false, Vec::new());
    }
    let tag = &html[div_start..=tag_end];
    if !tag.contains("sujet-filter-item") {
        return (false, Vec::new());
    }
    let solution = tag.contains("data-solution=\"1\"");
    let mut tags: Vec<String> = Vec::new();
    if let Some(tm) = RE_DATA_TAGS.captures(tag) {
        let raw = tm[1].replace("&quot;", "\"");
        if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(&raw) {
            tags = arr
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
        }
    }
    (solution, tags)
}

// ─────────────────────────── صفحة العنصر ──────────────────────────────────
/// استخراج بيانات صفحة العنصر: العنوان، الوصف، رابط PDF، رابط العارض.
pub fn parse_item_page(html: &str) -> ItemDetail {
    let mut out = ItemDetail::default();
    if let Some(t) = RE_TITLE.captures(html) {
        out.title = Some(
            clean_text(&t[1])
                .replace("| DzExams", "")
                .trim()
                .to_string(),
        );
    }
    if let Some(d) = RE_DESCRIPTION.captures(html) {
        out.description = Some(clean_text(&d[1]));
    }
    let pdfs: Vec<String> = RE_PDF
        .captures_iter(html)
        .map(|c| c[1].to_string())
        .collect();
    if let Some(first) = pdfs.first() {
        out.pdf_url = Some(first.clone());
        out.all_pdfs = pdfs;
    }
    if let Some(f) = RE_DATA_SRC_FRAME.captures(html) {
        out.viewer_url = Some(f[1].to_string());
    }
    out
}

/// صيغة رابط الرفع المعروفة (احتياطية فقط — الرمز الحقيقي يُستخرج من صفحة العنصر).
pub fn make_pdf_guess(item: &Item, pdf_token: Option<&str>) -> String {
    let singular = match item.kind.as_str() {
        "sujets" => "sujet",
        "documents" => "document",
        "articles" => "article",
        "annales" => "annale",
        _ => return String::new(),
    };
    let tok = pdf_token.unwrap_or(&item.token);
    format!("{BASE_URL}/uploads/{singular}/dzexams-{singular}-{tok}.pdf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_removes_tags() {
        assert_eq!(clean_text("<b>مرحبا</b>   <i>عالم</i>"), "مرحبا عالم");
    }

    #[test]
    fn parse_item_page_pdf() {
        let html = r#"<title>موضوع | DzExams</title>
        <meta name="description" content="وصف">
        <a href="https://www.dzexams.com/uploads/sujet/dzexams-sujet-abc.pdf">x</a>"#;
        let d = parse_item_page(html);
        assert_eq!(d.title.as_deref(), Some("موضوع"));
        assert_eq!(
            d.pdf_url.as_deref(),
            Some("https://www.dzexams.com/uploads/sujet/dzexams-sujet-abc.pdf")
        );
    }
}
