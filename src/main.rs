//! نقطة تشغيل بوت DzExams.
//!
//! الاستعمال:
//!   dzexams-bot               تشغيل البوت (polling)
//!   dzexams-bot --crawl [levels...]  زحف/فهرسة لمرة واحدة ثم الخروج
//!   dzexams-bot --healthcheck فحص اتصال الشبكة بخدمة تيليغرام (لـ Docker HEALTHCHECK)
//!   dzexams-bot --version     طباعة الإصدار

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("dzexams-bot {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.iter().any(|a| a == "--healthcheck") {
        return healthcheck().await;
    }

    // وضع الزحف المستقل: `--crawl` ثم أسماء الأطوار (مفصولة بفراغ أو فاصلة).
    if let Some(pos) = args.iter().position(|a| a == "--crawl") {
        let levels: Vec<String> = args[pos + 1..]
            .iter()
            .flat_map(|a| a.split(','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.starts_with('-'))
            .collect();
        let levels = if levels.is_empty() {
            None
        } else {
            Some(levels)
        };
        return dzexams_bot::crawler::run(levels).await.map(|_| ());
    }

    dzexams_bot::bot::run().await
}

/// يتحقّق من إمكانية الوصول إلى `api.telegram.org:443`.
async fn healthcheck() -> anyhow::Result<()> {
    let timeout = std::time::Duration::from_secs(5);
    match tokio::time::timeout(
        timeout,
        tokio::net::TcpStream::connect("api.telegram.org:443"),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(anyhow::anyhow!("فشل الاتصال بخدمة تيليغرام: {e}")),
        Err(_) => Err(anyhow::anyhow!("انتهت مهلة الاتصال بخدمة تيليغرام")),
    }
}
