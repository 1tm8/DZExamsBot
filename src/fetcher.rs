//! fetcher.rs — عميل HTTP لتجاوز Cloudflare في dzexams.com
//!
//! سلسلة الدفاع متعددة الطبقات:
//!
//!   1) ترويسة مخصّصة: User-Agent = Mediapartners-Google
//!   2) بصمة الحجب: تمييز نوع الرفض من جسم الاستجابة
//!   3) إعادة المحاولة: تراجع أسي قصير
//!   4) تدوير IP: عند تكرار الحجب يُستدعى warp.rotate() ثم يعاد المحاولة
//!   5) تخزين مؤقت TTL للصفحات لتقليل الضغط على الموقع
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, USER_AGENT};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use crate::config::{self, CONFIG};
use crate::warp::WarpManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockType {
    #[default]
    None,
    /// تحدي Cloudflare (Just a moment)
    CfChallenge,
    /// LiteSpeed reCAPTCHA (Bot Verification)
    BotVerification,
    /// 429
    RateLimit,
    Other403,
    Network,
    Timeout,
    NotFound,
}

impl BlockType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockType::None => "none",
            BlockType::CfChallenge => "cf_challenge",
            BlockType::BotVerification => "bot_verification",
            BlockType::RateLimit => "rate_limit",
            BlockType::Other403 => "other_403",
            BlockType::Network => "network",
            BlockType::Timeout => "timeout",
            BlockType::NotFound => "not_found",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FetchResult {
    pub ok: bool,
    pub status: u16,
    pub url: String,
    pub final_url: String,
    /// للنصوص
    pub text: String,
    /// للملفات الثنائية (PDF)
    pub content: Vec<u8>,
    pub block: BlockType,
    pub error: String,
    pub from_cache: bool,
    pub elapsed: f64,
    pub headers: HashMap<String, String>,
}

impl FetchResult {
    pub fn blocked(&self) -> bool {
        !matches!(self.block, BlockType::None | BlockType::NotFound)
    }
}

/// ذاكرة مؤقتة بسيطة LRU + TTL.
struct TtlCache {
    ttl: Duration,
    max_items: usize,
    store: IndexMap<String, (Instant, FetchResult)>,
}

impl TtlCache {
    fn new(ttl_secs: u64, max_items: usize) -> Self {
        TtlCache {
            ttl: Duration::from_secs(ttl_secs),
            max_items,
            store: IndexMap::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<FetchResult> {
        let expired = match self.store.get(key) {
            Some((ts, _)) => ts.elapsed() > self.ttl,
            None => return None,
        };
        if expired {
            self.store.shift_remove(key);
            return None;
        }
        // انقل إلى النهاية (MRU)
        if let Some(v) = self.store.shift_remove(key) {
            let res = v.1.clone();
            self.store.insert(key.to_string(), v);
            return Some(res);
        }
        None
    }

    fn put(&mut self, key: String, res: FetchResult) {
        self.store.shift_remove(&key);
        self.store.insert(key, (Instant::now(), res));
        while self.store.len() > self.max_items {
            self.store.shift_remove_index(0);
        }
    }

    fn len(&self) -> usize {
        self.store.len()
    }
}

/// تحديد نوع الحجب من رمز الحالة وجسم الاستجابة (بصمة معروفة).
pub fn fingerprint_block(status: u16, body: &[u8]) -> BlockType {
    if status == 200 {
        return BlockType::None;
    }
    if status == 404 || status == 410 {
        return BlockType::NotFound;
    }
    if status == 429 {
        return BlockType::RateLimit;
    }
    if status == 403 || status == 503 {
        let low = String::from_utf8_lossy(&body[..body.len().min(12000)]).to_lowercase();
        if low.contains("just a moment")
            || low.contains("cf-browser-verification")
            || low.contains("turnstile")
        {
            return BlockType::CfChallenge;
        }
        if low.contains("bot verification")
            || low.contains("lsrecap")
            || low.contains("verifying that you are not a robot")
        {
            return BlockType::BotVerification;
        }
        return BlockType::Other403;
    }
    BlockType::Other403
}

/// خيارات الجلب.
#[derive(Debug, Clone)]
pub struct GetOpts {
    pub as_bytes: bool,
    pub timeout: Option<u64>,
    pub use_cache: bool,
    pub allow_rotate: bool,
}

impl Default for GetOpts {
    fn default() -> Self {
        GetOpts {
            as_bytes: false,
            timeout: None,
            use_cache: true,
            allow_rotate: true,
        }
    }
}

/// عميل HTTP موحّد للبوت كله (تقييد الإيقاع + تخزين مؤقت + تجاوز CF).
pub struct Fetcher {
    client: reqwest::Client,
    warp: Arc<WarpManager>,
    sem: Semaphore,
    last_ts: AsyncMutex<Option<Instant>>,
    cache: Mutex<TtlCache>,
    /// عميل لكل بروكسي (reqwest لا يدعم تعيين بروكسي لكل طلب، بل عند بناء العميل).
    proxy_clients: Mutex<HashMap<String, reqwest::Client>>,
    consecutive_blocked: AtomicUsize,
    rotate_lock: AsyncMutex<()>,
    ua_idx: AtomicUsize,
    total_requests: AtomicU64,
    total_blocked: AtomicU64,
    total_rotations: AtomicU64,
}

/// يبني عميل reqwest بترويسات البوت، مع بروكسي اختياري.
fn build_client(proxy: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static(config::ACCEPT_LANGUAGE),
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,application/pdf,*/*;q=0.8"),
    );
    let mut builder = reqwest::Client::builder()
        .cookie_store(true)
        .timeout(Duration::from_secs(CONFIG.req_timeout))
        .user_agent(config::BYPASS_UA)
        .default_headers(headers);
    if let Some(p) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(p)?);
    }
    Ok(builder.build()?)
}

impl Fetcher {
    pub fn new(warp: Arc<WarpManager>) -> anyhow::Result<Self> {
        let client = build_client(None)?;
        Ok(Fetcher {
            client,
            warp,
            sem: Semaphore::new(CONFIG.max_concurrent_requests),
            last_ts: AsyncMutex::new(None),
            cache: Mutex::new(TtlCache::new(CONFIG.cache_ttl, CONFIG.cache_max)),
            proxy_clients: Mutex::new(HashMap::new()),
            consecutive_blocked: AtomicUsize::new(0),
            rotate_lock: AsyncMutex::new(()),
            ua_idx: AtomicUsize::new(0),
            total_requests: AtomicU64::new(0),
            total_blocked: AtomicU64::new(0),
            total_rotations: AtomicU64::new(0),
        })
    }

    /// عميل مناسب للبروكسي المطلوب (مع تخزين مؤقت لتفادي إعادة البناء).
    fn client_for(&self, proxy: Option<&str>) -> reqwest::Client {
        let p = match proxy {
            Some(p) if !p.is_empty() => p,
            _ => return self.client.clone(),
        };
        if let Ok(map) = self.proxy_clients.lock() {
            if let Some(c) = map.get(p) {
                return c.clone();
            }
        }
        match build_client(Some(p)) {
            Ok(c) => {
                if let Ok(mut map) = self.proxy_clients.lock() {
                    map.insert(p.to_string(), c.clone());
                }
                c
            }
            Err(e) => {
                log::warn!("proxy client build failed for {p}: {e}");
                self.client.clone()
            }
        }
    }

    /// إغلاق العميل (reqwest لا يحتاج إغلاقًا صريحًا — متوافق مع الواجهة القديمة).
    pub async fn close(&self) {}

    // ─────────────────────────── rate limit ──────────────────────────
    async fn pace(&self) {
        let mut last = self.last_ts.lock().await;
        let now = Instant::now();
        if let Some(prev) = *last {
            let gap = Duration::from_secs_f64(CONFIG.req_min_interval);
            let elapsed = now.duration_since(prev);
            if elapsed < gap {
                tokio::time::sleep(gap - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    // ─────────────────────────── core request ────────────────────────
    async fn do_get(
        &self,
        url: &str,
        as_bytes: bool,
        timeout: u64,
        proxy: Option<&str>,
    ) -> FetchResult {
        let t0 = Instant::now();
        let ua = config::UA_CHAIN[self.ua_idx.load(Ordering::Relaxed) % config::UA_CHAIN.len()];
        let client = self.client_for(proxy);
        let req = client
            .get(url)
            .timeout(Duration::from_secs(timeout))
            .header(USER_AGENT, ua);

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let block = if e.is_timeout() {
                    BlockType::Timeout
                } else {
                    BlockType::Network
                };
                return FetchResult {
                    url: url.to_string(),
                    block,
                    error: e.to_string(),
                    elapsed: t0.elapsed().as_secs_f64(),
                    ..Default::default()
                };
            }
        };

        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let headers: HashMap<String, String> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        let body = match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                return FetchResult {
                    url: url.to_string(),
                    final_url,
                    status,
                    headers,
                    block: BlockType::Network,
                    error: e.to_string(),
                    elapsed: t0.elapsed().as_secs_f64(),
                    ..Default::default()
                };
            }
        };

        let mut r = FetchResult {
            status,
            url: url.to_string(),
            final_url,
            headers,
            elapsed: t0.elapsed().as_secs_f64(),
            ..Default::default()
        };
        if as_bytes {
            r.ok = status == 200 && !body.is_empty();
            if status != 200 {
                r.block = fingerprint_block(status, &body);
            }
            r.content = body;
        } else {
            r.ok = status == 200;
            r.block = fingerprint_block(status, &body);
            r.text = String::from_utf8_lossy(&body).into_owned();
        }
        r
    }

    // ─────────────────────────── public get ──────────────────────────
    /// الجلب الذكي: تخزين مؤقت ← محاولتان بتراجع ← UA احتياطي ← تدوير IP.
    pub async fn get(&self, url: &str, opts: GetOpts) -> FetchResult {
        let timeout = opts.timeout.unwrap_or(if opts.as_bytes {
            CONFIG.pdf_timeout
        } else {
            CONFIG.req_timeout
        });
        let cache_key = if opts.as_bytes {
            format!("bin:{url}")
        } else {
            url.to_string()
        };

        if opts.use_cache && !opts.as_bytes {
            if let Some(c) = self.cache.lock().unwrap().get(&cache_key) {
                let mut r = c;
                r.from_cache = true;
                return r;
            }
        }

        let mut proxy = self.warp.current_proxy();
        let mut res: FetchResult;

        {
            let _permit = self.sem.acquire().await.expect("semaphore");
            self.total_requests.fetch_add(1, Ordering::Relaxed);

            // محاولتان سريعتان
            res = FetchResult::default();
            for attempt in 0..2 {
                self.pace().await;
                res = self
                    .do_get(url, opts.as_bytes, timeout, proxy.as_deref())
                    .await;
                if res.ok {
                    self.consecutive_blocked.store(0, Ordering::Relaxed);
                    if opts.use_cache && !opts.as_bytes {
                        self.cache
                            .lock()
                            .unwrap()
                            .put(cache_key.clone(), res.clone());
                    }
                    return res;
                }
                if res.block == BlockType::NotFound {
                    return res;
                }
                log::info!(
                    "get fail #{} {} -> status={} block={} err={}",
                    attempt + 1,
                    url,
                    res.status,
                    res.block.as_str(),
                    res.error
                );
                tokio::time::sleep(Duration::from_secs(2 * (attempt + 1))).await;
            }

            // فشلت المحاولتان: جرّب UA التالي في السلسلة قبل تدوير IP
            let idx = self.ua_idx.load(Ordering::Relaxed);
            if res.block != BlockType::NotFound && idx + 1 < config::UA_CHAIN.len() {
                self.ua_idx.fetch_add(1, Ordering::Relaxed);
                log::warn!("UA blocked, switching to {}", config::UA_CHAIN[idx + 1]);
                self.pace().await;
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                let res2 = self
                    .do_get(url, opts.as_bytes, timeout, proxy.as_deref())
                    .await;
                if res2.ok {
                    self.consecutive_blocked.store(0, Ordering::Relaxed);
                    if opts.use_cache && !opts.as_bytes {
                        self.cache
                            .lock()
                            .unwrap()
                            .put(cache_key.clone(), res2.clone());
                    }
                    return res2;
                }
                res = res2;
            }

            // فشلت المحاولات: تحديث عدّاد الحجب
            if res.blocked() || matches!(res.block, BlockType::Network | BlockType::Timeout) {
                self.total_blocked.fetch_add(1, Ordering::Relaxed);
                self.consecutive_blocked.fetch_add(1, Ordering::Relaxed);
            }
        }

        // تدوير IP إذا بلغ العدّاد العتبة
        if opts.allow_rotate
            && self.consecutive_blocked.load(Ordering::Relaxed) >= CONFIG.warp_fail_threshold
        {
            let mut rotated = false;
            {
                let _g = self.rotate_lock.lock().await;
                if self.consecutive_blocked.load(Ordering::Relaxed) >= CONFIG.warp_fail_threshold {
                    log::warn!(
                        "{} consecutive blocked -> rotating exit IP",
                        self.consecutive_blocked.load(Ordering::Relaxed)
                    );
                    self.consecutive_blocked.store(0, Ordering::Relaxed);
                    rotated = self.warp.rotate().await;
                    self.total_rotations.fetch_add(1, Ordering::Relaxed);
                }
            }
            if rotated {
                proxy = self.warp.current_proxy();
                self.pace().await;
                let _permit = self.sem.acquire().await.expect("semaphore");
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                let res2 = self
                    .do_get(url, opts.as_bytes, timeout, proxy.as_deref())
                    .await;
                if res2.ok {
                    self.consecutive_blocked.store(0, Ordering::Relaxed);
                    if opts.use_cache && !opts.as_bytes {
                        self.cache
                            .lock()
                            .unwrap()
                            .put(cache_key.clone(), res2.clone());
                    }
                    return res2;
                }
                res = res2;
            }
        }

        res
    }

    // ─────────────────────────── PDF download ────────────────────────
    /// تنزيل PDF إلى ملف محلي؛ يعيد المسار عند النجاح.
    pub async fn download_pdf(&self, url: &str, dest: &Path) -> Option<PathBuf> {
        let res = self
            .get(
                url,
                GetOpts {
                    as_bytes: true,
                    use_cache: false,
                    ..Default::default()
                },
            )
            .await;
        if res.ok && res.content.starts_with(b"%PDF") {
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(dest, &res.content).is_ok() {
                return Some(dest.to_path_buf());
            }
        }
        log::warn!(
            "PDF download failed {} status={} block={} len={}",
            url,
            res.status,
            res.block.as_str(),
            res.content.len()
        );
        None
    }

    // ─────────────────────────── stats ───────────────────────────────
    pub fn stats_line(&self) -> String {
        format!(
            "طلبات: {} | محجوبة: {} | تدوير IP: {} | الكاش: {}",
            self.total_requests.load(Ordering::Relaxed),
            self.total_blocked.load(Ordering::Relaxed),
            self.total_rotations.load(Ordering::Relaxed),
            self.cache.lock().unwrap().len()
        )
    }
}
