"""
stealth_browser.py — حلّال اختياري لتحقق LiteSpeed "Bot Verification"

متى يُستخدم؟
  بعض المقاطع (مثل /ar/library وصفحات معيّنة أحيانًا) تُظهر صفحة
  "Bot Verification" فيها reCAPTCHA خفية (invisible) تُنفّذ وتُرسل تلقائيًا
  في المتصفح الحقيقي خلال ثوانٍ. هذا الملف يفتح الصفحة في Playwright،
  ينتظر اكتمال التحقق، ثم يُصدّر الكوكيز لجلسة fetcher.

التفعيل:
    PLAYWRIGHT_ENABLED=1
    pip install playwright && playwright install chromium

ملاحظة مهمة: كوكيز LiteSpeed غير مرتبطة ببصمة TLS بعكس cf_clearance،
لذا نقلها إلى aiohttp يعمل عمليًا في معظم الحالات.
"""
import asyncio
import logging
from typing import Optional

import config

log = logging.getLogger("dzx.stealth")

try:
    from playwright.async_api import async_playwright
    PLAYWRIGHT_AVAILABLE = True
except ImportError:
    PLAYWRIGHT_AVAILABLE = False


async def solve_and_get_cookies(url: str, max_wait: int = 25) -> Optional[dict]:
    """
    فتح الرابط في متصفح حقيقي، انتظار تجاوز صفحة التحقق، وإرجاع الكوكيز.
    يعيد dict {name: value} أو None عند الفشل.
    """
    if not config.PLAYWRIGHT_ENABLED:
        return None
    if not PLAYWRIGHT_AVAILABLE:
        log.warning("PLAYWRIGHT_ENABLED=1 but playwright is not installed")
        return None

    try:
        async with async_playwright() as pw:
            browser = await pw.chromium.launch(
                headless=config.PLAYWRIGHT_HEADLESS,
                args=["--no-sandbox", "--disable-blink-features=AutomationControlled"],
            )
            ctx = await browser.new_context(
                user_agent=config.BROWSER_UA,
                locale="ar-DZ",
                viewport={"width": 1280, "height": 800},
            )
            page = await ctx.new_page()
            await page.goto(url, wait_until="domcontentloaded", timeout=45000)

            title = await page.title()
            log.info("browser opened %s title=%r", url, title[:60])

            # صفحة LiteSpeed تُنفّذ grecaptcha.execute() وتُرسل النموذج تلقائيًا؛
            # ننتظر تحوّل العنوان/العنوان. إن ظهر مربع تحقق مرئي ننقره.
            for _ in range(max_wait):
                await asyncio.sleep(1)
                title = (await page.title() or "").lower()
                if "bot verification" not in title and "just a moment" not in title:
                    break

            if "bot verification" in title or "just a moment" in title:
                # محاولة النقر على مربع reCAPTCHA إن كان مرئيًا
                try:
                    for frame in page.frames:
                        cb = await frame.query_selector(".recaptcha-checkbox-border")
                        if cb:
                            await cb.click(timeout=3000)
                            break
                except Exception:
                    pass
                await asyncio.sleep(6)

            title = (await page.title() or "").lower()
            if "bot verification" in title or "just a moment" in title:
                await browser.close()
                log.warning("verification not solved for %s", url)
                return None

            cookies = {}
            for c in await ctx.cookies():
                if "dzexams" in c.get("domain", ""):
                    cookies[c["name"]] = c["value"]
            await browser.close()
            log.info("verification solved, got %d cookies", len(cookies))
            return cookies or None
    except Exception as e:
        log.exception("stealth browser error: %s", e)
        return None


async def inject_cookies(fetcher, cookies: dict):
    """إدخال الكوكيز في جلسة aiohttp الخاصة بـ fetcher."""
    session = await fetcher._ensure_session()
    from yarl import URL
    jar = session.cookie_jar
    for name, value in cookies.items():
        jar.update_cookies({name: value}, URL(config.BASE_URL))


async def fetch_via_browser(fetcher, url: str) -> Optional[str]:
    """
    المسار الأخير: جلب الصفحة كاملة عبر المتصفح نفسه إن فشل نقل الكوكيز.
    """
    if not config.PLAYWRIGHT_ENABLED or not PLAYWRIGHT_AVAILABLE:
        return None
    try:
        async with async_playwright() as pw:
            browser = await pw.chromium.launch(
                headless=config.PLAYWRIGHT_HEADLESS,
                args=["--no-sandbox"],
            )
            ctx = await browser.new_context(user_agent=config.BROWSER_UA, locale="ar-DZ")
            page = await ctx.new_page()
            await page.goto(url, wait_until="domcontentloaded", timeout=45000)
            for _ in range(25):
                await asyncio.sleep(1)
                t = (await page.title() or "").lower()
                if "bot verification" not in t and "just a moment" not in t:
                    break
            html = await page.content()
            await browser.close()
            return html if html else None
    except Exception as e:
        log.exception("fetch_via_browser error: %s", e)
        return None
