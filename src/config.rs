//! config.rs — إعدادات البيئة
//!
//! جميع الإعدادات قابلة للتعديل عبر متغيرات البيئة أو ملف `.env`.
//! تُقرأ مرّة واحدة عند أول استعمال عبر `CONFIG`.
use std::collections::HashSet;
use std::path::PathBuf;

use once_cell::sync::Lazy;
use regex::Regex;

// ─── ثوابت الموقع (تجاوز Cloudflare) ─────────────────────────────────────
pub const BASE_URL: &str = "https://www.dzexams.com";

/// الـ User-Agent المثبت الذي يسمح به robots.txt (زاحف AdSense) فيمرّ عبر WAF.
pub const BYPASS_UA: &str = "Mediapartners-Google";

/// سلسلة User-Agent للتجاوز: الأساسي أولًا، والباقي احتياط عند حجب الأساسي.
pub const UA_CHAIN: &[&str] = &[
    "Mediapartners-Google",
    "AdsBot-Google-Mobile",
    "Googlebot/2.1 (+http://www.google.com/bot.html)",
];

/// User-Agent للمتصفح (مرجع فقط).
pub const ACCEPT_LANGUAGE: &str = "ar,en;q=0.8,fr;q=0.6";

/// حد تيليغرام لإرسال الملفات: ما فوقه يُرسل كرابط فقط.
pub const TELEGRAM_DOC_LIMIT: u64 = 49 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    // ─── Telegram ───────────────────────────────────────────────────────
    pub bot_token: String,
    pub admin_ids: HashSet<i64>,

    // ─── Rate limiting / politeness ─────────────────────────────────────
    pub max_concurrent_requests: usize,
    pub req_min_interval: f64,
    pub req_timeout: u64,
    pub pdf_timeout: u64,

    // ─── Caching ────────────────────────────────────────────────────────
    pub cache_ttl: u64,
    pub cache_max: usize,

    // ─── WARP / IP rotation ─────────────────────────────────────────────
    pub warp_fail_threshold: usize,
    pub warp_cli_path: String,
    pub warp_wg_profiles: String,
    pub proxy_list: Vec<String>,

    // ─── Storage ────────────────────────────────────────────────────────
    pub base_dir: PathBuf,
    pub db_path: String,
    pub tmp_dir: String,

    // ─── Background indexing ────────────────────────────────────────────
    pub auto_index_levels: Vec<String>,
    pub auto_index_hour: u32,
    pub index_section_delay: f64,

    // ─── UI ─────────────────────────────────────────────────────────────
    pub items_per_page: usize,
    pub subjects_per_page: usize,
    pub search_results_limit: usize,

    /// مهلة بين تحميلين لنفس المستخدم (ثوانٍ) — حماية من إغراق الموقع.
    pub user_dl_cooldown: u64,
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Config {
    fn from_env() -> Self {
        // حمّل .env من مجلد المشروع (أو الآباء) قبل قراءة المتغيّرات.
        let _ = dotenvy::dotenv();

        let base_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let admin_raw = std::env::var("ADMIN_IDS").unwrap_or_default();
        let digits = Regex::new(r"\d+").expect("valid regex");
        let admin_ids: HashSet<i64> = digits
            .find_iter(&admin_raw)
            .filter_map(|m| m.as_str().parse().ok())
            .collect();

        let db_path = std::env::var("DB_PATH")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| base_dir.join("dzexams.db").to_string_lossy().into_owned());

        Config {
            bot_token: env_str("BOT_TOKEN", ""),
            admin_ids,

            max_concurrent_requests: env_usize("MAX_CONCURRENT_REQUESTS", 1).max(1),
            req_min_interval: env_f64("REQ_MIN_INTERVAL", 1.2),
            req_timeout: env_u64("REQ_TIMEOUT", 25),
            pdf_timeout: env_u64("PDF_TIMEOUT", 90),

            cache_ttl: env_u64("CACHE_TTL", 1800),
            cache_max: env_usize("CACHE_MAX", 400),

            warp_fail_threshold: env_usize("WARP_FAIL_THRESHOLD", 3).max(1),
            warp_cli_path: env_str("WARP_CLI_PATH", ""),
            warp_wg_profiles: env_str("WARP_WG_PROFILES", "/etc/wireguard/warp*.conf"),
            proxy_list: split_list(&std::env::var("PROXY_LIST").unwrap_or_default()),

            base_dir,
            db_path,
            tmp_dir: env_str("TMP_DIR", "/tmp/dzexams_bot"),

            auto_index_levels: split_list(&std::env::var("AUTO_INDEX_LEVELS").unwrap_or_default()),
            auto_index_hour: env_u64("AUTO_INDEX_HOUR", 3) as u32,
            index_section_delay: env_f64("INDEX_SECTION_DELAY", 1.5),

            items_per_page: env_usize("ITEMS_PER_PAGE", 8).max(1),
            subjects_per_page: env_usize("SUBJECTS_PER_PAGE", 20).max(1),
            search_results_limit: env_usize("SEARCH_RESULTS_LIMIT", 10).max(1),

            user_dl_cooldown: env_u64("USER_DL_COOLDOWN", 10),
        }
    }
}

/// الإعدادات العامة، تُحمَّل عند أول استعمال.
pub static CONFIG: Lazy<Config> = Lazy::new(Config::from_env);
