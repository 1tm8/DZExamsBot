"""
scraper.py — استخراج البيانات من صفحات dzexams.com

يستخرج:
  - روابط المواد من صفحة المستوى
  - الأقسام (cours/d1..e3/evaluations) من صفحة المادة
  - الشعب (se/m/mt/ges/lit) من صفحات البكالوريا
  - العناصر (data-id + الصنف + العنوان) من صفحات الأقسام/الشهادات
  - رابط PDF ورابط المستند من صفحة العنصر
  - العنوان وسنة الموضوع
"""
import logging
import re
from dataclasses import dataclass, field
from typing import List, Optional

import config
from dzenc import dz_decode, content_path

log = logging.getLogger("dzx.scraper")

RE_LEVEL_SUBJECT_LINKS = re.compile(
    r'href="https?://www\.dzexams\.com/ar/({level})/([a-z0-9\-]+)"'.format(level=r"{lvl}")
)
RE_SECTION_LINKS = re.compile(
    r'href="https?://www\.dzexams\.com/ar/({lvl})/({sub})/(cours|d[123]|e[123]|evaluations)"'
)
RE_STREAM_LINKS = re.compile(
    r'href="https?://www\.dzexams\.com/ar/(bac)/([a-z0-9\-]+)/(se|m|mt|ges|lit)"'
)
# عنصر: <a href="#" data-id="..." class="item-center btn-item-xxx">
RE_ITEM_ANCHOR = re.compile(
    r'<a[^>]*data-id="([^"]+)"[^>]*class="[^"]*item-center[^"]*btn-item-(sujet|document|article|annale)[^"]*"'
    r'|<a[^>]*class="[^"]*item-center[^"]*btn-item-(sujet|document|article|annale)[^"]*"[^>]*data-id="([^"]+)"'
)
RE_PDF = re.compile(r'href="(https?://[^"\s]+/uploads/[^"\s]+\.pdf[^"\s]*)"')
RE_TITLE = re.compile(r"<title>\s*(.*?)\s*</title>", re.S)
RE_YEAR = re.compile(r"(20\d\d)")
# السنة في صفحة القائمة: 📅 ثم وسوم/فراغات ثم السنة (مثبت حيًا 2026-11-15)
RE_YEAR_ITEM = re.compile(r"📅(?:<[^>]+>|\s)*(\d{4})")
RE_DESCRIPTION = re.compile(r'<meta name="description" content="([^"]*)"')
RE_DATA_SRC_FRAME = re.compile(r'data-src="(https?://[^"]+)"')

_WS = re.compile(r"\s+")


def clean_text(s: str) -> str:
    s = re.sub(r"<[^>]+>", " ", s or "")
    return _WS.sub(" ", s).strip()


@dataclass
class Item:
    kind: str          # sujets / documents / articles / annales
    token: str         # رمز الرابط بعد فك dzEnc
    title: str = ""
    url: str = ""      # رابط الصفحة الكامل
    pdf_url: str = ""
    level: str = ""
    subject: str = ""
    section: str = ""
    stream: str = ""
    year: str = ""
    solution: bool = False   # الموقع يوثّق حلًا لهذا الملف (data-solution="1")
    tags: list = field(default_factory=list)  # الشعب المرتبطة (data-tags)

    def site_url(self) -> str:
        return f"{config.BASE_URL}{self.url}"


@dataclass
class SectionPage:
    items: List[Item] = field(default_factory=list)
    title: str = ""
    description: str = ""


# ─────────────────────────── المستويات والمواد ─────────────────────────────
def parse_subjects(level: str, html: str) -> List[str]:
    """استخراج مواد المستوى من صفحة المستوى."""
    subs = set(
        re.findall(
            r'href="https?://www\.dzexams\.com/ar/%s/([a-z0-9\-]+)"' % re.escape(level),
            html,
        )
    )
    bad_prefixes = (
        "sujets", "documents", "articles", "annales", "cours", "evaluations",
        "e1", "e2", "e3", "d1", "d2", "d3", "forum", "upload", "profile",
        "university", "teachers", "stories", "islam", "advice", "enigmes",
        "faqs", "olympiades", "filieres",
    )
    out = []
    for s in sorted(subs):
        if s.startswith(bad_prefixes) or s in ("se", "m", "mt", "ges", "lit"):
            continue
        out.append(s)
    return out


def parse_sections(level: str, subject: str, html: str) -> List[str]:
    """أقسام المادة (للمستويات الدراسية فقط)."""
    secs = re.findall(
        r'href="https?://www\.dzexams\.com/ar/%s/%s/(cours|d[123]|e[123]|evaluations)"'
        % (re.escape(level), re.escape(subject)),
        html,
    )
    return sorted(set(secs))


def parse_streams(level: str, subject: str, html: str) -> List[str]:
    """شعب البكالوريا للمادة."""
    streams = re.findall(
        r'href="https?://www\.dzexams\.com/ar/bac/%s/(se|m|mt|ges|lit)"' % re.escape(subject),
        html,
    )
    return sorted(set(streams))


# ─────────────────────────── العناصر ──────────────────────────────────────
def _extract_items_raw(html: str):
    """يستخرج أزواج (data-id, kind) بالترتيب من الصفحة."""
    pairs = []
    for m in re.finditer(
        r'<a\b([^>]*)>', html
    ):
        attrs = m.group(1)
        did = re.search(r'data-id="([^"]+)"', attrs)
        kind = re.search(r'btn-item-(sujet|document|article|annale)', attrs)
        if did and kind:
            pairs.append((did.group(1), kind.group(1)))
    return pairs


def parse_items(html: str, *, level: str = "", subject: str = "",
                section: str = "", stream: str = "") -> SectionPage:
    """
    استخراج عناصر القائمة من صفحة قسم/شهادة.
    عنوان كل عنصر: الكتلة div.*-title التالية للرابط.
    """
    page = SectionPage()
    t = RE_TITLE.search(html)
    page.title = clean_text(t.group(1)).replace("| DzExams", "").strip() if t else ""
    d = RE_DESCRIPTION.search(html)
    page.description = clean_text(d.group(1)) if d else ""

    pairs = _extract_items_raw(html)
    if not pairs:
        return page

    # تقسيم الصفحة على مواضع الروابط لالتقاط العنوان التالي لكل رابط
    positions = []
    for m in re.finditer(r'<a\b[^>]*data-id="[^"]+"[^>]*>', html):
        positions.append(m.end())
    # عناوين الكتل
    title_blocks = list(
        re.finditer(
            r'<div class="(?:sujet-title|document-title|item-title|annale-title)"[^>]*>(.*?)</div>',
            html,
            re.S,
        )
    )

    # الربط: لكل رابط، أقرب كتلة عنوان بعده (خلال 2500 حرف)
    used = set()
    for i, (did, kind) in enumerate(pairs):
        token = dz_decode(did)
        if not token:
            continue
        path = content_path(f"btn-item-{kind}", token)
        if not path:
            continue
        start = positions[i] if i < len(positions) else 0
        title = ""
        for j, tb in enumerate(title_blocks):
            if j in used or tb.start() < start - 200:
                continue
            if tb.start() - start > 2500:
                break
            title = clean_text(tb.group(1))
            used.add(j)
            break
        if not title:
            title = page.title or "مستند"
        # تنظيف أسماء مكررة من رأس الصفحة
        title = _strip_page_title(title, page.title)
        # السنة: من المقطع التالي للرابط (📅 ثم السنة)، وإلا من العنوان
        seg_end = positions[i + 1] if i + 1 < len(positions) else start + 2500
        segment = html[start:seg_end]
        year = ""
        ym = RE_YEAR_ITEM.search(segment)
        if ym:
            year = ym.group(1)
        else:
            ym = RE_YEAR.search(title)
            if ym:
                year = ym.group(1)
        # الحل والشعب: من حاوية العنصر التي تسبق الرابط مباشرة
        solution, tags = _container_flags(html, start)
        it = Item(
            kind={"sujet": "sujets", "document": "documents",
                  "article": "articles", "annale": "annales"}[kind],
            token=token,
            title=title or "مستند",
            url=path,
            level=level, subject=subject, section=section, stream=stream,
            year=year,
            solution=solution,
            tags=tags,
        )
        page.items.append(it)
    return page


def _container_flags(html: str, anchor_end: int):
    """الحل والشعب من حاوية العنصر <div class="... sujet-filter-item"
    data-solution="1" data-tags='["se"]'> التي تسبق الرابط مباشرة.

    يعيد (has_solution: bool, tags: list[str]). غياب السمة = لا حل.
    """
    import json as _json
    div_start = html.rfind("<div", 0, anchor_end)
    if div_start < 0:
        return False, []
    tag_end = html.find(">", div_start)
    if tag_end < 0 or tag_end > anchor_end:
        return False, []
    tag = html[div_start:tag_end + 1]
    if "sujet-filter-item" not in tag:
        return False, []
    solution = 'data-solution="1"' in tag
    tags: list = []
    tm = re.search(r"data-tags='([^']*)'", tag)
    if tm:
        raw = tm.group(1).replace("&quot;", '"')
        try:
            parsed = _json.loads(raw)
            if isinstance(parsed, list):
                tags = [str(t) for t in parsed]
        except Exception:
            tags = []
    return solution, tags


def _strip_page_title(item_title: str, page_title: str) -> str:
    """إزالة تكرار اسم المستوى/المادة الطويل من عنوان العنصر."""
    if not item_title:
        return item_title
    # العناوين كثيرًا ما تكون: "موضوع الرياضيات - شهادة التعليم المتوسط 2025"
    # نُبقيها كما هي لكن ننظّف الفراغات المتكررة (تم في clean_text).
    return item_title[:120]


# ─────────────────────────── صفحة العنصر ──────────────────────────────────
def parse_item_page(html: str) -> dict:
    """
    استخراج بيانات صفحة العنصر: العنوان، الوصف، رابط PDF، رابط العارض.
    """
    out = {}
    t = RE_TITLE.search(html)
    if t:
        out["title"] = clean_text(t.group(1)).replace("| DzExams", "").strip()
    d = RE_DESCRIPTION.search(html)
    if d:
        out["description"] = clean_text(d.group(1))
    pdfs = RE_PDF.findall(html)
    if pdfs:
        out["pdf_url"] = pdfs[0]
        out["all_pdfs"] = pdfs
    frames = RE_DATA_SRC_FRAME.findall(html)
    if frames:
        out["viewer_url"] = frames[0]
    return out


def make_pdf_guess(item: "Item", pdf_token: Optional[str] = None) -> str:
    """
    صيغة رابط الرفع المعروفة:
        /uploads/{kind Singular}/dzexams-{kind-singular}-{token}.pdf
    لكن الرمز داخل رابط الرفع مختلف عن رمز الصفحة، لذا نعتمد دائمًا
    على استخراج الرابط الحقيقي من صفحة العنصر. هذه الدالة احتياطية فقط.
    """
    singular = {"sujets": "sujet", "documents": "document",
                "articles": "article", "annales": "annale"}.get(item.kind, "")
    if not singular:
        return ""
    tok = pdf_token or item.token
    return f"{config.BASE_URL}/uploads/{singular}/dzexams-{singular}-{tok}.pdf"
