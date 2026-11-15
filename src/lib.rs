//! dzexams_bot — بوت تيليغرام لتصفح موارد dzexams.com (إعادة كتابة بلغة Rust).
//!
//! الوحدات:
//! - [`config`]: الإعدادات والمتغيّرات البيئية والثوابت.
//! - [`catalog`]: بيانات الأطوار والمواد والمسارات.
//! - [`dzenc`]: فكّ ترميز مسارات الموقع.
//! - [`scraper`]: تحليل صفحات HTML إلى بيانات.
//! - [`warp`]: تدوير عناوين IP (WARP).
//! - [`fetcher`]: عميل HTTP مع تجاوز الحماية.
//! - [`service`]: طبقة الجلب العليا.
//! - [`indexer`]: الفهرسة المحلية والبحث (SQLite + FTS5).
//! - [`bot`]: واجهة تيليغرام.

pub mod bot;
pub mod catalog;
pub mod config;
pub mod dzenc;
pub mod fetcher;
pub mod indexer;
pub mod scraper;
pub mod service;
pub mod warp;
