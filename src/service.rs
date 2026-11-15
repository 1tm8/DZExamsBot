//! service.rs — الطبقة العليا: جلب محتوى الموقع وتحويله لبيانات جاهزة للبوت
use std::path::PathBuf;
use std::sync::Arc;

use crate::catalog;
use crate::config::{BASE_URL, CONFIG};
use crate::fetcher::{BlockType, FetchResult, Fetcher, GetOpts};
use crate::scraper::{
    parse_item_page, parse_items, parse_sections, parse_streams, Item, ItemDetail, SectionPage,
};

#[derive(Debug, Clone)]
pub struct ContentError {
    pub msg_ar: String,
    pub block: BlockType,
    pub url: String,
}

impl ContentError {
    pub fn new(msg_ar: impl Into<String>) -> Self {
        ContentError {
            msg_ar: msg_ar.into(),
            block: BlockType::None,
            url: String::new(),
        }
    }
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg_ar)
    }
}

impl std::error::Error for ContentError {}

fn block_message(block: BlockType) -> &'static str {
    match block {
        BlockType::BotVerification => "الموقع يطلب تحققًا إضافيًا في الوقت الحالي.\nجرّب بعد قليل.",
        BlockType::CfChallenge => "تعذّر الوصول إلى هذا القسم حاليًا.\nجرّب بعد قليل.",
        BlockType::RateLimit => "الطلبات كثيرة حاليًا. انتظر قليلًا ثم جرّب.",
        BlockType::Network => "تعذّر الوصول إلى الموقع حاليًا. حاول مجددًا بعد قليل.",
        BlockType::Timeout => "تأخّر الرد من الموقع. حاول مجددًا.",
        BlockType::NotFound => "هذا المحتوى غير موجود.",
        _ => "تعذّر إتمام الطلب حاليًا. حاول مجددًا.",
    }
}

pub fn block_error(res: &FetchResult) -> ContentError {
    ContentError {
        msg_ar: block_message(res.block).to_string(),
        block: res.block,
        url: res.url.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubjectKind {
    School,
    #[default]
    Exam,
    Streams,
}

/// نتيجة تحميل صفحة المادة.
#[derive(Debug, Clone, Default)]
pub struct SubjectInfo {
    pub kind: SubjectKind,
    pub sections: Vec<String>,
    pub streams: Vec<String>,
    pub items: Option<SectionPage>,
}

pub struct ContentService {
    pub fetcher: Arc<Fetcher>,
}

impl ContentService {
    pub fn new(fetcher: Arc<Fetcher>) -> Self {
        ContentService { fetcher }
    }

    // ─── صفحة المادة ─────────────────────────────────────────────────
    pub async fn subject_page(
        &self,
        level: &str,
        subject: &str,
    ) -> Result<SubjectInfo, ContentError> {
        let url = format!("{BASE_URL}/ar/{level}/{subject}");
        let res = self.fetcher.get(&url, GetOpts::default()).await;
        if !res.ok {
            return Err(block_error(&res));
        }

        if catalog::EXAM_LEVELS.contains(&level) {
            let page = parse_items(&res.text, level, subject, "", "");
            return Ok(SubjectInfo {
                kind: SubjectKind::Exam,
                items: Some(page),
                ..Default::default()
            });
        }

        if catalog::STREAM_LEVELS.contains(&level) {
            let streams = parse_streams(level, subject, &res.text);
            if !streams.is_empty() {
                return Ok(SubjectInfo {
                    kind: SubjectKind::Streams,
                    streams,
                    ..Default::default()
                });
            }
            let page = parse_items(&res.text, level, subject, "", "");
            return Ok(SubjectInfo {
                kind: SubjectKind::Exam,
                items: Some(page),
                ..Default::default()
            });
        }

        let sections = parse_sections(level, subject, &res.text);
        if !sections.is_empty() {
            return Ok(SubjectInfo {
                kind: SubjectKind::School,
                sections,
                ..Default::default()
            });
        }
        // بعض المواد بلا أقسام: عناصر مباشرة
        let page = parse_items(&res.text, level, subject, "", "");
        Ok(SubjectInfo {
            kind: SubjectKind::Exam,
            items: Some(page),
            ..Default::default()
        })
    }

    // ─── صفحة القسم ──────────────────────────────────────────────────
    pub async fn section_items(
        &self,
        level: &str,
        subject: &str,
        section: &str,
        stream: &str,
    ) -> Result<SectionPage, ContentError> {
        let path = if !stream.is_empty() {
            format!("/ar/bac/{subject}/{stream}")
        } else if !section.is_empty() {
            format!("/ar/{level}/{subject}/{section}")
        } else {
            format!("/ar/{level}/{subject}")
        };
        let res = self
            .fetcher
            .get(&format!("{BASE_URL}{path}"), GetOpts::default())
            .await;
        if !res.ok {
            return Err(block_error(&res));
        }
        Ok(parse_items(&res.text, level, subject, section, stream))
    }

    // ─── صفحة العنصر ─────────────────────────────────────────────────
    pub async fn item_detail(&self, item: &Item) -> Result<ItemDetail, ContentError> {
        let res = self.fetcher.get(&item.site_url(), GetOpts::default()).await;
        if !res.ok {
            return Err(block_error(&res));
        }
        let mut data = parse_item_page(&res.text);
        if data.title.is_none() {
            data.title = Some(item.title.clone());
        }
        Ok(data)
    }

    // ─── تنزيل PDF ───────────────────────────────────────────────────
    /// مسار مؤقّت آمن لملف PDF الخاص بالعنصر.
    pub fn pdf_dest(item: &Item) -> PathBuf {
        let safe: String = item
            .token
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(40)
            .collect();
        let safe = if safe.is_empty() {
            "doc".to_string()
        } else {
            safe
        };
        PathBuf::from(&CONFIG.tmp_dir).join(format!("dzx_{safe}.pdf"))
    }

    /// تنزيل PDF بالاعتماد على رابط جاهز (بعد جلب تفاصيل العنصر).
    pub async fn download_pdf_from_url(&self, item: &Item, pdf_url: &str) -> Option<PathBuf> {
        let dest = Self::pdf_dest(item);
        self.fetcher.download_pdf(pdf_url, &dest).await
    }

    /// جلب تفاصيل العنصر ثم تنزيل ملف PDF الخاص به.
    pub async fn download_item_pdf(&self, item: &Item) -> Result<Option<PathBuf>, ContentError> {
        let detail = self.item_detail(item).await?;
        let pdf_url = match detail.pdf_url {
            Some(u) => u,
            None => return Ok(None),
        };
        Ok(self.download_pdf_from_url(item, &pdf_url).await)
    }
}
