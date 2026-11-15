//! warp.rs — إدارة تدوير عنوان IP للخروج عند حجب Cloudflare لعنوان السيرفر
//!
//! ثلاث آليات خروج بالأولوية (يكتشف البوت المتوفرة تلقائيًا):
//!
//! 1) warp-cli      — عميل Cloudflare WARP الرسمي (warp-svc).
//!    `warp-cli disconnect` ثم `connect` عادةً يجلب IP جديدًا.
//! 2) wg-quick      — ملفات WireGuard مولّدة بـ wgcf (warp1.conf, warp2.conf...).
//!    التدوير = إيقاف الملف الحالي وتشغيل التالي (دورة بين الملفات).
//! 3) PROXY_LIST    — قائمة بروكسي HTTP/SOCKS5 من متغير البيئة، تدوير دوري.
//!
//! كشف نجاح الدوران: طلب https://www.cloudflare.com/cdn-cgi/trace وقراءة ip=
use std::sync::Mutex;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::config::CONFIG;

pub const TRACE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Backend {
    #[default]
    None,
    WarpCli,
    WgQuick,
    Proxy,
}

impl Backend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Backend::None => "none",
            Backend::WarpCli => "warp-cli",
            Backend::WgQuick => "wg-quick",
            Backend::Proxy => "proxy",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WarpStatus {
    pub backend: Backend,
    pub enabled: bool,
    pub exit_ip: String,
    pub warp_on: bool,
    pub detail: String,
    pub profiles: Vec<String>,
    pub proxy_index: usize,
}

fn parse_trace(text: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn valid_ip(ip: &str) -> bool {
    ip.parse::<std::net::IpAddr>().is_ok()
}

/// مدير تدوير IP: يكتشف الآلية المتاحة ويوفر `rotate()` موحّد.
pub struct WarpManager {
    status: Mutex<WarpStatus>,
    rotate_lock: tokio::sync::Mutex<()>,
    last_rotation_ts: Mutex<Option<Instant>>,
}

impl WarpManager {
    pub fn new() -> Self {
        let mgr = WarpManager {
            status: Mutex::new(WarpStatus::default()),
            rotate_lock: tokio::sync::Mutex::new(()),
            last_rotation_ts: Mutex::new(None),
        };
        mgr.detect();
        mgr
    }

    // ─────────────────────────── detection ────────────────────────────
    fn detect(&self) {
        let mut st = self.status.lock().unwrap();

        // 1) warp-cli
        let cli = if !CONFIG.warp_cli_path.is_empty() {
            Some(std::path::PathBuf::from(&CONFIG.warp_cli_path))
        } else {
            which::which("warp-cli").ok()
        };
        if let Some(path) = cli {
            st.backend = Backend::WarpCli;
            st.enabled = true;
            st.detail = format!("warp-cli found at {}", path.display());
            st.profiles = Vec::new();
            log::info!("WARP backend: warp-cli ({})", path.display());
            return;
        }

        // 2) wg-quick profiles
        if !CONFIG.warp_wg_profiles.is_empty() {
            let mut profiles: Vec<String> = glob::glob(&CONFIG.warp_wg_profiles)
                .map(|paths| {
                    paths
                        .filter_map(Result::ok)
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            profiles.sort();
            if !profiles.is_empty() && which::which("wg-quick").is_ok() {
                st.backend = Backend::WgQuick;
                st.enabled = true;
                st.detail = format!("{} wgcf profile(s)", profiles.len());
                st.profiles = profiles;
                log::info!(
                    "WARP backend: wg-quick with {} profile(s)",
                    st.profiles.len()
                );
                return;
            }
        }

        // 3) proxies
        if !CONFIG.proxy_list.is_empty() {
            st.backend = Backend::Proxy;
            st.enabled = true;
            st.detail = format!("{} proxy(ies)", CONFIG.proxy_list.len());
            log::info!(
                "WARP backend: proxy rotation with {} proxy(ies)",
                CONFIG.proxy_list.len()
            );
            return;
        }

        st.backend = Backend::None;
        st.enabled = false;
        st.detail = "no warp-cli / no wg profiles / no proxies configured".to_string();
        log::warn!("No WARP/IP-rotation backend detected: {}", st.detail);
    }

    pub fn status(&self) -> WarpStatus {
        self.status.lock().unwrap().clone()
    }

    // ─────────────────────────── helpers ──────────────────────────────
    /// يعيد IP الخروج الحالي عبر cloudflare trace، أو سلسلة فارغة.
    pub async fn get_exit_ip(&self, proxy: Option<&str>, timeout: u64) -> String {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(timeout));
        if let Some(p) = proxy {
            if let Ok(pr) = reqwest::Proxy::all(p) {
                builder = builder.proxy(pr);
            }
        }
        let client = match builder.build() {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        let req = client.get(TRACE_URL);
        match req.send().await {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(t) => {
                    let info = parse_trace(&t);
                    let ip = info.get("ip").cloned().unwrap_or_default();
                    let warp = matches!(
                        info.get("warp").map(String::as_str),
                        Some("on") | Some("plus")
                    );
                    self.status.lock().unwrap().warp_on = warp;
                    if valid_ip(&ip) {
                        ip
                    } else {
                        String::new()
                    }
                }
                Err(_) => String::new(),
            },
            _ => String::new(),
        }
    }

    /// البروكسي الحالي للجلسة إن كانت الآلية proxy.
    pub fn current_proxy(&self) -> Option<String> {
        let st = self.status.lock().unwrap();
        if st.backend != Backend::Proxy || CONFIG.proxy_list.is_empty() {
            return None;
        }
        let i = st.proxy_index % CONFIG.proxy_list.len();
        let p = CONFIG.proxy_list[i].clone();
        if p.is_empty() {
            None
        } else {
            Some(p)
        }
    }

    // ─────────────────────────── rotation ─────────────────────────────
    /// يبدّل عنوان IP الخروج. يعيد `true` عند نجاح التدوير.
    pub async fn rotate(&self) -> bool {
        if !self.status.lock().unwrap().enabled {
            log::warn!("rotate() requested but no backend enabled");
            return false;
        }

        let _guard = self.rotate_lock.lock().await;
        {
            let mut last = self.last_rotation_ts.lock().unwrap();
            let now = Instant::now();
            if let Some(t) = *last {
                if now.duration_since(t) < Duration::from_secs(20) {
                    log::info!("rotate() skipped: cooldown");
                    return true; // تُعتبر دورة جارية/حديثة
                }
            }
            *last = Some(now);
        }

        let old_ip = self.get_exit_ip(None, 12).await;
        let backend = self.status.lock().unwrap().backend;
        let ok = match backend {
            Backend::WarpCli => self.rotate_warp_cli().await,
            Backend::WgQuick => self.rotate_wg_quick().await,
            Backend::Proxy => self.rotate_proxy(),
            Backend::None => false,
        };

        let new_ip = self.get_exit_ip(None, 12).await;
        {
            let mut st = self.status.lock().unwrap();
            st.exit_ip = if new_ip.is_empty() {
                "?".to_string()
            } else {
                new_ip.clone()
            };
        }
        log::info!(
            "rotate(): backend={} old={} new={} ok={}",
            backend.as_str(),
            if old_ip.is_empty() { "?" } else { &old_ip },
            if new_ip.is_empty() { "?" } else { &new_ip },
            ok
        );
        ok
    }

    /// تشغيل أمر نظام، يعيد (returncode, stdout).
    async fn run_cmd(cmd: &[&str], timeout: u64) -> (i32, String) {
        use tokio::process::Command;
        let fut = Command::new(cmd[0]).args(&cmd[1..]).output();
        match tokio::time::timeout(Duration::from_secs(timeout), fut).await {
            Ok(Ok(out)) => {
                let code = out.status.code().unwrap_or(-1);
                (code, String::from_utf8_lossy(&out.stdout).into_owned())
            }
            _ => (-1, String::new()),
        }
    }

    // 1) warp-cli الرسمي
    async fn rotate_warp_cli(&self) -> bool {
        let cli_path = if !CONFIG.warp_cli_path.is_empty() {
            Some(CONFIG.warp_cli_path.clone())
        } else {
            which::which("warp-cli")
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        };
        let cli = match cli_path {
            Some(c) => c,
            None => return false,
        };
        Self::run_cmd(&[&cli, "disconnect"], 25).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let (rc, _) = Self::run_cmd(&[&cli, "connect"], 25).await;
        if rc != 0 {
            Self::run_cmd(&[&cli, "status"], 25).await;
        }
        let re = Regex::new(r"(?i)Connected").expect("regex");
        for _ in 0..25 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let (rc, out) = Self::run_cmd(&[&cli, "status"], 25).await;
            if rc == 0 && re.is_match(&out) {
                return true;
            }
        }
        false
    }

    // 2) wg-quick بملفات wgcf متعددة
    async fn rotate_wg_quick(&self) -> bool {
        let profiles = self.status.lock().unwrap().profiles.clone();
        if profiles.is_empty() {
            return false;
        }
        let n = profiles.len();
        let names: Vec<String> = profiles
            .iter()
            .map(|p| {
                let base = p.rsplit('/').next().unwrap_or(p);
                base.strip_suffix(".conf").unwrap_or(base).to_string()
            })
            .collect();
        for name in &names {
            Self::run_cmd(&["wg-quick", "down", name], 30).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let idx = {
            let mut st = self.status.lock().unwrap();
            st.proxy_index = (st.proxy_index + 1) % n;
            st.proxy_index
        };
        let name = &names[idx];
        let (rc, out) = Self::run_cmd(&["wg-quick", "up", name], 40).await;
        if rc != 0 {
            log::warn!(
                "wg-quick up {} failed: {}",
                name,
                out.chars().take(200).collect::<String>()
            );
            return false;
        }
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if !self.get_exit_ip(None, 8).await.is_empty() {
                return true;
            }
        }
        false
    }

    // 3) بروكسي دوري
    fn rotate_proxy(&self) -> bool {
        if CONFIG.proxy_list.is_empty() {
            return false;
        }
        let mut st = self.status.lock().unwrap();
        st.proxy_index = (st.proxy_index + 1) % CONFIG.proxy_list.len();
        true
    }

    // ─────────────────────────── reporting ────────────────────────────
    pub async fn describe(&self) -> String {
        let st = self.status();
        if st.backend == Backend::None {
            return "⚠️ لا توجد آلية تدوير IP مفعّلة.\n\
                    فعّل واحدة: warp-cli أو wgcf + wg-quick أو PROXY_LIST."
                .to_string();
        }
        let ip = if st.exit_ip.is_empty() {
            self.get_exit_ip(None, 12).await
        } else {
            st.exit_ip.clone()
        };
        let mut lines = vec![
            format!("🛡 الآلية: {}", st.backend.as_str()),
            format!(
                "🌐 IP الحالي: {}",
                if ip.is_empty() {
                    "غير معروف".to_string()
                } else {
                    ip
                }
            ),
            format!("🔎 WARP مفعّل: {}", if st.warp_on { "نعم" } else { "لا" }),
        ];
        if st.backend == Backend::WgQuick && !st.profiles.is_empty() {
            lines.push(format!("📁 عدد ملفات wgcf: {}", st.profiles.len()));
        }
        if st.backend == Backend::Proxy {
            lines.push(format!(
                "🔄 فهرس البروكسي: {}/{}",
                st.proxy_index + 1,
                CONFIG.proxy_list.len()
            ));
        }
        lines.join("\n")
    }
}

impl Default for WarpManager {
    fn default() -> Self {
        Self::new()
    }
}
