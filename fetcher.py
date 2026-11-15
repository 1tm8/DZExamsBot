"""
fetcher.py — عميل HTTP لتجاوز Cloudflare في dzexams.com

سلسلة الدفاع متعددة الطبقات:

  1) ترويسة مخصّصة: User-Agent = Mediapartners-Google
     (crawler AdSense مسموح كليًا في robots.txt، فتتجاوز WAF تحدي الجافاسكربت)

  2) بصمة الحجب: تمييز نوع الرفض من جسم الاستجابة
       - CF_CHALLENGE       : 403 + "Just a moment..." (تحدي Cloudflare)
       - BOT_VERIFICATION   : 403 + "Bot Verification" / lsrecap (LiteSpeed reCAPTCHA)
       - RATE_LIMIT         : 429
     الحجم التقريبي للتشخيص: تحدي CF ≈ 6KB، تحقق البوت ≈ 2.8KB

  3) إعادة المحاولة: تراجع أسي قصير (الرفض أحيانًا عابر)

  4) تدوير IP: عند تكرار الحجب (WARP_FAIL_THRESHOLD مرات متتالية)
     يُستدعى warp.rotate() لإحضار IP جديد ثم يعاد المحاولة تلقائيًا

  5) تخزين مؤقت TTL للصفحات لتقليل الضغط على الموقع
"""
import asyncio
import logging
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from enum import Enum
from typing import Optional

import aiohttp

import config
from warp import WarpManager

log = logging.getLogger("dzx.fetcher")


class BlockType(str, Enum):
    NONE = "none"
    CF_CHALLENGE = "cf_challenge"            # تحدي Cloudflare (Just a moment)
    BOT_VERIFICATION = "bot_verification"    # LiteSpeed reCAPTCHA (Bot Verification)
    RATE_LIMIT = "rate_limit"                # 429
    OTHER_403 = "other_403"
    NETWORK = "network"
    TIMEOUT = "timeout"
    NOT_FOUND = "not_found"


@dataclass
class FetchResult:
    ok: bool = False
    status: int = 0
    url: str = ""
    final_url: str = ""
    text: str = ""                            # للنصوص
    content: bytes = b""                      # للملفات الثنائية (PDF)
    block: BlockType = BlockType.NONE
    error: str = ""
    from_cache: bool = False
    elapsed: float = 0.0
    headers: dict = field(default_factory=dict)

    @property
    def blocked(self) -> bool:
        return self.block not in (BlockType.NONE, BlockType.NOT_FOUND)


class TTLCache:
    """ذاكرة مؤقتة بسيطة LRU + TTL."""

    def __init__(self, ttl: int, max_items: int):
        self.ttl = ttl
        self.max_items = max_items
        self._store: "OrderedDict[str, tuple[float, FetchResult]]" = OrderedDict()

    def get(self, key: str) -> Optional[FetchResult]:
        item = self._store.get(key)
        if not item:
            return None
        ts, res = item
        if time.monotonic() - ts > self.ttl:
            self._store.pop(key, None)
            return None
        self._store.move_to_end(key)
        return res

    def put(self, key: str, res: FetchResult):
        self._store[key] = (time.monotonic(), res)
        self._store.move_to_end(key)
        while len(self._store) > self.max_items:
            self._store.popitem(last=False)

    def clear(self):
        self._store.clear()


def fingerprint_block(status: int, body: bytes) -> BlockType:
    """تحديد نوع الحجب من رمز الحالة وجسم الاستجابة (بصمة معروفة)."""
    if status == 200:
        return BlockType.NONE
    if status == 404 or status == 410:
        return BlockType.NOT_FOUND
    if status == 429:
        return BlockType.RATE_LIMIT
    if status in (403, 503):
        low = body[:12000].lower()
        if b"just a moment" in low or b"cf-browser-verification" in low or b"turnstile" in low:
            return BlockType.CF_CHALLENGE
        if (
            b"bot verification" in low
            or b"lsrecap" in low
            or b"verifying that you are not a robot" in low
        ):
            return BlockType.BOT_VERIFICATION
        return BlockType.OTHER_403
    return BlockType.OTHER_403


class Fetcher:
    """عميل HTTP موحّد للبوت كله (تقييد الإيقاع + تخزين مؤقت + تجاوز CF)."""

    def __init__(self, warp: Optional[WarpManager] = None):
        self.warp = warp or WarpManager()
        self._sem = asyncio.Semaphore(config.MAX_CONCURRENT_REQUESTS)
        self._last_ts = 0.0
        self._session: Optional[aiohttp.ClientSession] = None
        self.cache = TTLCache(config.CACHE_TTL, config.CACHE_MAX)
        # عدّاد الحجب المتتالي (يُصفَّر عند أي نجاح)
        self._consecutive_blocked = 0
        self._rotate_lock = asyncio.Lock()
        self._ua_idx = 0  # موقع UA الحالي في UA_CHAIN
        self.total_requests = 0
        self.total_blocked = 0
        self.total_rotations = 0

    # ─────────────────────────── session ─────────────────────────────
    async def _ensure_session(self):
        if self._session is None or self._session.closed:
            jar = aiohttp.CookieJar(unsafe=True)
            self._session = aiohttp.ClientSession(
                cookie_jar=jar,
                timeout=aiohttp.ClientTimeout(total=config.REQ_TIMEOUT),
                headers={
                    "User-Agent": config.BYPASS_UA,
                    "Accept-Language": config.ACCEPT_LANGUAGE,
                    "Accept": "text/html,application/xhtml+xml,application/pdf,*/*;q=0.8",
                },
            )
        return self._session

    async def close(self):
        if self._session and not self._session.closed:
            await self._session.close()

    async def clear_cookies(self):
        if self._session and not self._session.closed:
            self._session.cookie_jar.clear()

    # ─────────────────────────── rate limit ──────────────────────────
    async def _pace(self):
        gap = config.REQ_MIN_INTERVAL
        now = time.monotonic()
        wait = self._last_ts + gap - now
        if wait > 0:
            await asyncio.sleep(wait)
        self._last_ts = time.monotonic()

    # ─────────────────────────── core request ────────────────────────
    async def _do_get(self, url: str, *, as_bytes: bool, timeout: int,
                      proxy: Optional[str]) -> FetchResult:
        session = await self._ensure_session()
        t0 = time.monotonic()
        to = aiohttp.ClientTimeout(total=timeout)
        try:
            ua = config.UA_CHAIN[self._ua_idx % len(config.UA_CHAIN)]
            async with session.get(url, timeout=to, proxy=proxy,
                                  headers={"User-Agent": ua}) as r:
                body = await r.read()
                res = FetchResult(
                    status=r.status,
                    url=url,
                    final_url=str(r.url),
                    headers=dict(r.headers),
                    elapsed=time.monotonic() - t0,
                )
                if as_bytes:
                    res.content = body
                    res.ok = r.status == 200 and len(body) > 0
                    if r.status != 200:
                        res.block = fingerprint_block(r.status, body[:8000])
                else:
                    # الموقع يخدم النصوص بترميز utf-8؛ صفحات الحجب windows-1252
                    charset = r.charset or "utf-8"
                    try:
                        res.text = body.decode(charset, errors="replace")
                    except LookupError:
                        res.text = body.decode("utf-8", errors="replace")
                    res.ok = r.status == 200
                    res.block = fingerprint_block(r.status, body[:8000])
                return res
        except asyncio.TimeoutError:
            return FetchResult(url=url, block=BlockType.TIMEOUT,
                               error="timeout", elapsed=time.monotonic() - t0)
        except aiohttp.ClientError as e:
            return FetchResult(url=url, block=BlockType.NETWORK,
                               error=str(e), elapsed=time.monotonic() - t0)
        except Exception as e:
            return FetchResult(url=url, block=BlockType.NETWORK,
                               error=repr(e), elapsed=time.monotonic() - t0)

    # ─────────────────────────── public get ──────────────────────────
    async def get(self, url: str, *, as_bytes: bool = False,
                  timeout: Optional[int] = None, use_cache: bool = True,
                  allow_rotate: bool = True) -> FetchResult:
        """
        الجلب الذكي: تخزين مؤقت ← محاولتان بتراجع ← UA احتياطي ← تدوير IP عند تكرار الحجب.
        """
        if timeout is None:
            timeout = config.PDF_TIMEOUT if as_bytes else config.REQ_TIMEOUT

        cache_key = url if not as_bytes else "bin:" + url
        if use_cache and not as_bytes:
            cached = self.cache.get(cache_key)
            if cached:
                r = FetchResult(
                    ok=cached.ok, status=cached.status, url=cached.url,
                    final_url=cached.final_url, text=cached.text,
                    block=cached.block, from_cache=True, headers=cached.headers,
                )
                return r

        proxy = self.warp.current_proxy()

        async with self._sem:
            self.total_requests += 1
            # محاولتان سريعتان
            for attempt in range(2):
                await self._pace()
                res = await self._do_get(url, as_bytes=as_bytes,
                                         timeout=timeout, proxy=proxy)
                if res.ok:
                    self._consecutive_blocked = 0
                    if use_cache and not as_bytes:
                        self.cache.put(cache_key, res)
                    return res
                if res.block == BlockType.NOT_FOUND:
                    return res  # غير موجود لا يستحق إعادة
                log.info(
                    "get fail #%d %s -> status=%s block=%s err=%s",
                    attempt + 1, url, res.status, res.block.value, res.error,
                )
                await asyncio.sleep(2 * (attempt + 1))

            # فشلت المحاولتان: جرّب UA التالي في السلسلة قبل تدوير IP
            if (res.block != BlockType.NOT_FOUND
                    and self._ua_idx + 1 < len(config.UA_CHAIN)):
                self._ua_idx += 1
                log.warning("UA blocked, switching to %s",
                            config.UA_CHAIN[self._ua_idx])
                await self._pace()
                self.total_requests += 1
                res2 = await self._do_get(url, as_bytes=as_bytes,
                                          timeout=timeout, proxy=proxy)
                if res2.ok:
                    self._consecutive_blocked = 0
                    if use_cache and not as_bytes:
                        self.cache.put(cache_key, res2)
                    return res2
                res = res2

            # فشلت المحاولتان: تحديث عدّاد الحجب
            if res.blocked or res.block in (BlockType.NETWORK, BlockType.TIMEOUT):
                self.total_blocked += 1
                self._consecutive_blocked += 1

        # تدوير IP إذا بلغ العدّاد العتبة
        if allow_rotate and self._consecutive_blocked >= config.WARP_FAIL_THRESHOLD:
            rotated = False
            async with self._rotate_lock:
                if self._consecutive_blocked >= config.WARP_FAIL_THRESHOLD:
                    log.warning(
                        "%d consecutive blocked -> rotating exit IP",
                        self._consecutive_blocked,
                    )
                    self._consecutive_blocked = 0
                    rotated = await self.warp.rotate()
                    self.total_rotations += 1
            if rotated:
                proxy = self.warp.current_proxy()
                # محاولة أخيرة بعد التدوير
                await self._pace()
                async with self._sem:
                    self.total_requests += 1
                    res2 = await self._do_get(url, as_bytes=as_bytes,
                                              timeout=timeout, proxy=proxy)
                    if res2.ok:
                        self._consecutive_blocked = 0
                        if use_cache and not as_bytes:
                            self.cache.put(cache_key, res2)
                        return res2
                    res = res2

        return res

    # ─────────────────────────── PDF download ────────────────────────
    async def download_pdf(self, url: str, dest_path: str) -> Optional[str]:
        """تنزيل PDF إلى ملف محلي؛ يعيد المسار عند النجاح."""
        res = await self.get(url, as_bytes=True, use_cache=False)
        if res.ok and res.content[:5] in (b"%PDF-", b"%PDF"):
            with open(dest_path, "wb") as f:
                f.write(res.content)
            return dest_path
        log.warning("PDF download failed %s status=%s block=%s len=%d",
                    url, res.status, res.block.value, len(res.content))
        return None

    # ─────────────────────────── stats ───────────────────────────────
    def stats_line(self) -> str:
        return (
            f"طلبات: {self.total_requests} | محجوبة: {self.total_blocked} "
            f"| تدوير IP: {self.total_rotations} | الكاش: {len(self.cache._store)}"
        )


# ─── اختبار مباشر: python fetcher.py ─────────────────────────────────────
if __name__ == "__main__":
    import sys

    async def _test():
        logging.basicConfig(level=logging.INFO, stream=sys.stdout)
        f = Fetcher()
        r = await f.get(f"{config.BASE_URL}/ar/bem")
        print("status:", r.status, "ok:", r.ok, "len:", len(r.text),
              "block:", r.block.value, "cache:", r.from_cache)
        r2 = await f.get(f"{config.BASE_URL}/ar/bem")
        print("second call from_cache:", r2.from_cache)
        await f.close()

    asyncio.run(_test())
