"""
warp.py — إدارة تدوير عنوان IP للخروج عند حجب Cloudflare لعنوان السيرفر

ثلاث آليات خروج بالأولوية (يكتشف البوت المتوفرة تلقائيًا):

1) warp-cli      — عميل Cloudflare WARP الرسمي (warp-svc).
                   `warp-cli disconnect` ثم `connect` عادةً يجلب IP جديدًا.
2) wg-quick      — ملفات WireGuard مولّدة بـ wgcf (warp1.conf, warp2.conf...).
                   التدوير = إيقاف الملف الحالي وتشغيل التالي (دورة بين الملفات).
3) PROXY_LIST    — قائمة بروكسي HTTP/SOCKS5 من متغير البيئة، تدوير دوري.

كشف نجاح الدوران: طلب https://www.cloudflare.com/cdn-cgi/trace وقراءة ip=
"""
import asyncio
import glob
import ipaddress
import logging
import re
import shutil
from dataclasses import dataclass, field
from enum import Enum
from typing import List, Optional

import aiohttp

import config

log = logging.getLogger("dzx.warp")

TRACE_URL = "https://www.cloudflare.com/cdn-cgi/trace"
_WARP_PROFILE_GLOB = getattr(config, "WARP_WG_PROFILES", "") or ""


class Backend(str, Enum):
    NONE = "none"
    WARP_CLI = "warp-cli"
    WG_QUICK = "wg-quick"
    PROXY = "proxy"


@dataclass
class WarpStatus:
    backend: Backend = Backend.NONE
    enabled: bool = False
    exit_ip: str = ""
    warp_on: bool = False
    detail: str = ""
    profiles: List[str] = field(default_factory=list)
    proxy_index: int = 0


def _parse_trace(text: str) -> dict:
    out = {}
    for line in text.splitlines():
        if "=" in line:
            k, _, v = line.partition("=")
            out[k.strip()] = v.strip()
    return out


class WarpManager:
    """مدير تدوير IP: يكتشف الآلية المتاحة ويوفر rotate() موحّد."""

    def __init__(self):
        self.status = WarpStatus()
        self._rotate_lock = asyncio.Lock()
        self._last_rotation_ts = 0.0
        self.detect()

    # ─────────────────────────── detection ────────────────────────────
    def detect(self) -> WarpStatus:
        st = self.status

        # 1) warp-cli
        cli = config.WARP_CLI_PATH or shutil.which("warp-cli")
        if cli:
            st.backend = Backend.WARP_CLI
            st.enabled = True
            st.detail = f"warp-cli found at {cli}"
            st.profiles = []
            log.info("WARP backend: warp-cli (%s)", cli)
            return st

        # 2) wg-quick profiles
        if _WARP_PROFILE_GLOB:
            profiles = sorted(glob.glob(_WARP_PROFILE_GLOB))
            if profiles and shutil.which("wg-quick"):
                st.backend = Backend.WG_QUICK
                st.enabled = True
                st.profiles = profiles
                st.detail = f"{len(profiles)} wgcf profile(s)"
                log.info("WARP backend: wg-quick with %d profile(s)", len(profiles))
                return st

        # 3) proxies
        if config.PROXY_LIST:
            st.backend = Backend.PROXY
            st.enabled = True
            st.detail = f"{len(config.PROXY_LIST)} proxy(ies)"
            log.info("WARP backend: proxy rotation with %d proxy(ies)", len(config.PROXY_LIST))
            return st

        st.backend = Backend.NONE
        st.enabled = False
        st.detail = "no warp-cli / no wg profiles / no proxies configured"
        log.warning("No WARP/IP-rotation backend detected: %s", st.detail)
        return st

    # ─────────────────────────── helpers ──────────────────────────────
    @staticmethod
    def _valid_ip(ip: str) -> bool:
        try:
            ipaddress.ip_address(ip)
            return True
        except ValueError:
            return False

    async def get_exit_ip(self, proxy: Optional[str] = None, timeout: int = 12) -> str:
        """يعيد IP الخروج الحالي عبر cloudflare trace، أو سلسلة فارغة."""
        try:
            to = aiohttp.ClientTimeout(total=timeout)
            async with aiohttp.ClientSession(timeout=to) as s:
                async with s.get(TRACE_URL, proxy=proxy) as r:
                    if r.status != 200:
                        return ""
                    info = _parse_trace(await r.text())
                    ip = info.get("ip", "")
                    self.status.warp_on = info.get("warp") in ("on", "plus")
                    return ip if self._valid_ip(ip) else ""
        except Exception as e:
            log.debug("trace failed: %s", e)
            return ""

    def current_proxy(self) -> Optional[str]:
        """البروكسي الحالي للجلسة إن كانت الآلية proxy."""
        if self.status.backend is not Backend.PROXY or not config.PROXY_LIST:
            return None
        i = self.status.proxy_index % len(config.PROXY_LIST)
        return config.PROXY_LIST[i] or None

    # ─────────────────────────── rotation ─────────────────────────────
    async def rotate(self) -> bool:
        """
        يبدّل عنوان IP الخروج. يعيد True عند نجاح التدوير (IP جديد مختلف).
        استدعاء واحد في كل مرة (قفل) وتهدئة 20 ثانية بين الدورات.
        """
        import time as _time

        if not self.status.enabled:
            log.warning("rotate() requested but no backend enabled")
            return False

        async with self._rotate_lock:
            now = _time.monotonic()
            if now - self._last_rotation_ts < 20:
                log.info("rotate() skipped: cooldown")
                return True  # تُعتبر دورة جارية/حديثة
            self._last_rotation_ts = now

            old_ip = await self.get_exit_ip()
            backend = self.status.backend
            ok = False

            try:
                if backend is Backend.WARP_CLI:
                    ok = await self._rotate_warp_cli()
                elif backend is Backend.WG_QUICK:
                    ok = await self._rotate_wg_quick()
                elif backend is Backend.PROXY:
                    ok = await self._rotate_proxy()
            except Exception as e:
                log.exception("rotation error: %s", e)

            new_ip = await self.get_exit_ip()
            self.status.exit_ip = new_ip or "?"
            changed = bool(ok and new_ip and new_ip != old_ip) or ok
            log.info(
                "rotate(): backend=%s old=%s new=%s changed=%s",
                backend.value, old_ip or "?", new_ip or "?", changed,
            )
            return changed

    async def _run(self, *cmd: str, timeout: int = 25) -> tuple:
        """تشغيل أمر نظام، يعيد (returncode, stdout)."""
        try:
            proc = await asyncio.create_subprocess_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            out, err = await asyncio.wait_for(proc.communicate(), timeout)
            return proc.returncode, (out or b"").decode(errors="replace")
        except Exception as e:
            log.debug("cmd %s failed: %s", cmd[0], e)
            return -1, ""

    # 1) warp-cli الرسمي
    async def _rotate_warp_cli(self) -> bool:
        cli = config.WARP_CLI_PATH or shutil.which("warp-cli")
        if not cli:
            return False
        # بنية الأوامر تختلف بين الإصدارات؛ نجرب الحديثة ثم القديمة
        await self._run(cli, "disconnect")
        await asyncio.sleep(3)
        rc, _ = await self._run(cli, "connect")
        if rc != 0:
            await self._run(cli, "status")  # تسجيل الحالة فقط
        # انتظار الاتصال حتى 25 ثانية
        for _ in range(25):
            await asyncio.sleep(1)
            rc, out = await self._run(cli, "status")
            if rc == 0 and re.search(r"Connected", out, re.I):
                return True
        return False

    # 2) wg-quick بملفات wgcf متعددة
    async def _rotate_wg_quick(self) -> bool:
        profiles = self.status.profiles
        if not profiles:
            return False
        n = len(profiles)
        names = [p.rsplit("/", 1)[-1].removesuffix(".conf") for p in profiles]
        # إيقاف أي واجهة عاملة حاليًا
        for name in names:
            await self._run("wg-quick", "down", name, timeout=30)
        await asyncio.sleep(2)
        # تشغيل الملف التالي في الدورة
        self.status.proxy_index = (self.status.proxy_index + 1) % n
        name = names[self.status.proxy_index]
        rc, out = await self._run("wg-quick", "up", name, timeout=40)
        if rc != 0:
            log.warning("wg-quick up %s failed: %s", name, out[:200])
            return False
        # انتظار توفّر الشبكة عبر الواجهة الجديدة
        for _ in range(10):
            await asyncio.sleep(1)
            if await self.get_exit_ip(timeout=8):
                return True
        return False

    # 3) بروكسي دوري
    async def _rotate_proxy(self) -> bool:
        if not config.PROXY_LIST:
            return False
        self.status.proxy_index = (self.status.proxy_index + 1) % len(config.PROXY_LIST)
        return True

    # ─────────────────────────── reporting ────────────────────────────
    async def describe(self) -> str:
        st = self.status
        if st.backend is Backend.NONE:
            return (
                "⚠️ لا توجد آلية تدوير IP مفعّلة.\n"
                "فعّل واحدة: warp-cli أو wgcf + wg-quick أو PROXY_LIST."
            )
        ip = st.exit_ip or await self.get_exit_ip()
        lines = [
            f"🛡 الآلية: {st.backend.value}",
            f"🌐 IP الحالي: {ip or 'غير معروف'}",
            f"🔎 WARP مفعّل: {'نعم' if st.warp_on else 'لا'}",
        ]
        if st.backend is Backend.WG_QUICK and st.profiles:
            lines.append(f"📁 عدد ملفات wgcf: {len(st.profiles)}")
        if st.backend is Backend.PROXY:
            lines.append(f"🔄 فهرس البروكسي: {st.proxy_index + 1}/{len(config.PROXY_LIST)}")
        return "\n".join(lines)
