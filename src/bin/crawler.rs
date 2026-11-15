//! ثنائي مستقل: زاحف محتوى DzExams.
//!
//! يجلب محتوى الموقع ويفهرسه في قاعدة بيانات البوت ثم يخرج — دون تشغيل البوت.
//! الاستعمال:
//!   dzexams-crawler                فهرسة كل الأطوار (أو AUTO_INDEX_LEVELS)
//!   dzexams-crawler bac 3as        فهرسة أطوار محدّدة
//!   dzexams-crawler bac,3as        الصيغة نفسها بفواصل
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let levels: Vec<String> = std::env::args()
        .skip(1)
        .flat_map(|a| {
            a.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
        .collect();

    let levels = if levels.is_empty() {
        None
    } else {
        Some(levels)
    };
    dzexams_bot::crawler::run(levels).await.map(|_| ())
}
