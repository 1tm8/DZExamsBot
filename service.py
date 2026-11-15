"""
service.py — الطبقة العليا: جلب محتوى الموقع وتحويله لبيانات جاهزة للبوت
"""
import logging
import os
from typing import List, Optional

import config
import catalog
from fetcher import Fetcher, BlockType
from scraper import (
    Item, SectionPage, parse_items, parse_sections, parse_streams,
    parse_item_page,
)

log = logging.getLogger("dzx.service")


class ContentError(Exception):
    def __init__(self, msg_ar: str, block: BlockType = BlockType.NONE, url: str = ""):
        super().__init__(msg_ar)
        self.msg_ar = msg_ar
        self.block = block
        self.url = url


BLOCK_MESSAGES = {
    BlockType.BOT_VERIFICATION: (
        "الموقع يطلب تحققًا إضافيًا في الوقت الحالي.\n"
        "جرّب بعد قليل."
    ),
    BlockType.CF_CHALLENGE: (
        "تعذّر الوصول إلى هذا القسم حاليًا.\n"
        "جرّب بعد قليل."
    ),
    BlockType.RATE_LIMIT: "الطلبات كثيرة حاليًا. انتظر قليلًا ثم جرّب.",
    BlockType.NETWORK: "تعذّر الوصول إلى الموقع حاليًا. حاول مجددًا بعد قليل.",
    BlockType.TIMEOUT: "تأخّر الرد من الموقع. حاول مجددًا.",
    BlockType.NOT_FOUND: "هذا المحتوى غير موجود.",
}


def block_error(res) -> ContentError:
    msg = BLOCK_MESSAGES.get(res.block, "تعذّر إتمام الطلب حاليًا. حاول مجددًا.")
    return ContentError(msg, res.block, res.url)


class ContentService:
    def __init__(self, fetcher: Fetcher):
        self.fetcher = fetcher

    # ─── صفحة المادة ─────────────────────────────────────────────────
    async def subject_page(self, level: str, subject: str) -> dict:
        """
        يعيد dict:
          {"type": "school"|"exam"|"streams", "sections": [...], "streams": [...],
           "items": SectionPage|None}
        """
        url = f"{config.BASE_URL}/ar/{level}/{subject}"
        res = await self.fetcher.get(url)
        if not res.ok:
            raise block_error(res)

        if level in catalog.EXAM_LEVELS:
            page = parse_items(res.text, level=level, subject=subject)
            return {"type": "exam", "items": page, "sections": [], "streams": []}

        if level in catalog.STREAM_LEVELS:
            streams = parse_streams(level, subject, res.text)
            if streams:
                return {"type": "streams", "streams": streams,
                        "sections": [], "items": None}
            page = parse_items(res.text, level=level, subject=subject)
            return {"type": "exam", "items": page, "sections": [], "streams": []}

        sections = parse_sections(level, subject, res.text)
        if sections:
            return {"type": "school", "sections": sections,
                    "streams": [], "items": None}
        # بعض المواد بلا أقسام: عناصر مباشرة
        page = parse_items(res.text, level=level, subject=subject)
        return {"type": "exam", "items": page, "sections": [], "streams": []}

    # ─── صفحة القسم ──────────────────────────────────────────────────
    async def section_items(self, level: str, subject: str, section: str,
                            stream: str = "") -> SectionPage:
        if stream:
            path = f"/ar/bac/{subject}/{stream}"
        elif section:
            path = f"/ar/{level}/{subject}/{section}"
        else:
            path = f"/ar/{level}/{subject}"
        res = await self.fetcher.get(f"{config.BASE_URL}{path}")
        if not res.ok:
            raise block_error(res)
        page = parse_items(res.text, level=level, subject=subject,
                           section=section, stream=stream)
        return page

    # ─── صفحة العنصر ─────────────────────────────────────────────────
    async def item_detail(self, item: Item) -> dict:
        res = await self.fetcher.get(item.site_url())
        if not res.ok:
            raise block_error(res)
        data = parse_item_page(res.text)
        data.setdefault("title", item.title)
        return data

    # ─── تنزيل PDF ───────────────────────────────────────────────────
    async def download_item_pdf(self, item: Item) -> Optional[str]:
        detail = await self.item_detail(item)
        pdf_url = detail.get("pdf_url")
        if not pdf_url:
            return None
        os.makedirs(config.TMP_DIR, exist_ok=True)
        safe = "".join(c for c in item.token if c.isalnum())[:40] or "doc"
        fname = f"dzx_{safe}.pdf"
        dest = os.path.join(config.TMP_DIR, fname)
        got = await self.fetcher.download_pdf(pdf_url, dest)
        return got

    # ─── عناصر صفحة العرض ────────────────────────────────────────────
    @staticmethod
    def items_to_dicts(items: List[Item]) -> List[dict]:
        return [
            {
                "kind": it.kind, "token": it.token, "title": it.title,
                "url": it.url, "level": it.level, "subject": it.subject,
                "section": it.section, "stream": it.stream, "year": it.year,
                "solution": bool(it.solution), "tags": list(it.tags or []),
            }
            for it in items
        ]
