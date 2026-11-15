//! crawler.rs — زاحف محتوى مستقل: يفهرس dzexams في قاعدة بيانات البوت ثم يخرج.
//!
//! يُستعمل من:
//!   - الأمر `dzexams-bot --crawl [levels...]`
//!   - الثنائي المستقل `dzexams-crawler [levels...]`
//!
//! مناسب لـ cron أو التهيئة الأولى، بمعزل عن تشغيل البوت. العناصر الجديدة
//! تُعلَّم `published = 0` فينشرها البوت لاحقًا تلقائيًا.
use std::sync::Arc;

use crate::catalog;
use crate::config::CONFIG;
use crate::fetcher::Fetcher;
use crate::indexer::Indexer;
use crate::warp::WarpManager;

/// يحدّد الأطوار المطلوب فهرستها: المعطاة، أو `AUTO_INDEX_LEVELS`، أو كل الأطوار.
fn resolve_levels(levels: Option<Vec<String>>) -> Vec<String> {
    match levels {
        Some(l) => l
            .into_iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| catalog::is_valid_level(s))
            .collect(),
        None if !CONFIG.auto_index_levels.is_empty() => CONFIG.auto_index_levels.clone(),
        None => catalog::LEVELS
            .iter()
            .map(|(l, _)| (*l).to_string())
            .collect(),
    }
}

/// يشغّل جولة فهرسة واحدة ثم يطبع تقريرًا. يعيد عدد العناصر الجديدة.
pub async fn run(levels: Option<Vec<String>>) -> anyhow::Result<usize> {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    let levels = resolve_levels(levels);
    if levels.is_empty() {
        anyhow::bail!(
            "لا توجد أطوار صالحة للفهرسة.\nمرّر أسماء أطوار صحيحة (مثل: 1as 2as 3as bac) \
             أو اضبط AUTO_INDEX_LEVELS في البيئة."
        );
    }
    std::fs::create_dir_all(&CONFIG.tmp_dir)?;
    log::info!("بدء الزحف للأطوار: {}", levels.join(", "));

    let warp = Arc::new(WarpManager::new());
    let fetcher = Arc::new(Fetcher::new(warp)?);
    let indexer = Arc::new(Indexer::new(fetcher, None)?);

    let mut added = 0usize;
    for lv in &levels {
        added += indexer.index_level(lv, None).await;
    }

    println!("{}", indexer.stats_text());
    println!("➕ عناصر أُضيفت/حُدِّثت هذه الجولة: {added}");
    println!(
        "⏳ بانتظار النشر التلقائي: {}",
        indexer.pending_publish_count()
    );
    Ok(added)
}
