//! publisher.rs — نشر المحتوى الجديد تلقائيًا إلى قناة و/أو المشتركين.
//!
//! يعتمد على علم `published` في قاعدة البيانات: كل عنصر يُفهرس حديثًا يبدأ
//! بقيمة 0، وبعد نشره يُعلَّم بـ 1. بذلك يستطيع البوت (أو الزاحف المستقل)
//! أن يجد العناصر غير المنشورة وينشرها دون تكرار.
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, InlineKeyboardButton, InlineKeyboardMarkup, LinkPreviewOptions, ParseMode,
};
use url::Url;

use crate::catalog;
use crate::scraper::Item;

fn esc(s: &str) -> String {
    html_escape::encode_text(s).into_owned()
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

/// نص منشور عنصر واحد بصيغة HTML.
pub fn item_post(it: &Item) -> String {
    let mut text = format!(
        "🆕 <b>{}</b>\n🎓 {} — {}",
        esc(&it.title),
        esc(catalog::level_name(&it.level)),
        esc(catalog::subject_ar(&it.subject)),
    );
    if !it.year.is_empty() {
        text.push_str(&format!("\n📅 سنة {}", esc(&it.year)));
    }
    if it.solution {
        text.push_str("\n✅ الحل مرفق");
    }
    text
}

/// زر فتح الملف في الموقع.
fn item_markup(it: &Item) -> Option<InlineKeyboardMarkup> {
    let u = Url::parse(&it.site_url()).ok()?;
    Some(InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::url("📥 فتح الملف", u),
    ]]))
}

/// ينشر كل عنصر كمنشور مستقل (مناسب للقنوات).
pub async fn publish_posts(bot: &Bot, chat: ChatId, items: &[Item]) {
    for it in items {
        let mut req = bot
            .send_message(chat, item_post(it))
            .parse_mode(ParseMode::Html)
            .link_preview_options(no_preview());
        if let Some(kb) = item_markup(it) {
            req = req.reply_markup(kb);
        }
        if let Err(e) = req.await {
            log::warn!("publish post to {chat}: {e}");
        }
    }
}

/// ملخّص واحد يجمع العناصر الجديدة (مناسب للمشتركين).
pub fn digest(items: &[Item]) -> String {
    let mut text = format!("🆕 <b>محتوى جديد ({})</b>\n\n", items.len());
    for it in items {
        text.push_str(&format!(
            "• <a href=\"{}\">{}</a> — {} ({})\n",
            it.site_url(),
            esc(&it.title),
            esc(catalog::subject_ar(&it.subject)),
            esc(catalog::level_name(&it.level)),
        ));
    }
    text
}

/// يرسل الملخّص إلى محادثة واحدة.
pub async fn publish_digest(bot: &Bot, chat: ChatId, text: &str) {
    if let Err(e) = bot
        .send_message(chat, text)
        .parse_mode(ParseMode::Html)
        .link_preview_options(no_preview())
        .await
    {
        log::debug!("publish digest to {chat}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scraper::Item;

    fn item() -> Item {
        Item {
            title: "درس <الرياضيات>".into(),
            url: "/ar/1as/math/x".into(),
            level: "1as".into(),
            subject: "math".into(),
            year: "2024".into(),
            solution: true,
            ..Default::default()
        }
    }

    #[test]
    fn post_escapes_html_and_keeps_meta() {
        let t = item_post(&item());
        assert!(t.contains("&lt;الرياضيات&gt;"));
        assert!(!t.contains("<الرياضيات>"));
        assert!(t.contains("2024"));
        assert!(t.contains("✅"));
    }

    #[test]
    fn digest_lists_item_links() {
        let d = digest(&[item()]);
        assert!(d.contains("محتوى جديد"));
        assert!(d.contains("/ar/1as/math/x"));
    }
}
