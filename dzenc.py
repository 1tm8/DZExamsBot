"""
dzenc.py — دالة فك تشويش معرّف الروابط (data-id) في موقع dzexams.com

الموقع يخفي روابط الموارد خلف وسم data-id مشوّش:
    1) فك ترميز Base64
    2) طرح 8 من كل بايت
الناتج هو رمز (token) يُستخدم في مسار المحتوى:
    /ar/sujets/{token} أو /ar/documents/{token}
    أو /ar/articles/{token} أو /ar/annales/{token}
"""
import base64


def dz_decode(data_id: str) -> str:
    """فك تشويش data-id وإرجاع رمز الرابط الجاهز للاستخدام في المسار."""
    if not data_id:
        return ""
    try:
        raw = base64.b64decode(data_id)
    except Exception:
        return ""
    return "".join(chr((b - 8) & 0xFF) for b in raw)


def content_path(item_class: str, token: str, lang: str = "ar") -> str:
    """
    تحويل صنف العنصر (btn-item-xxx) إلى مسار كامل.
    الخريطة مأخوذة من web.js الرسمي للموقع:
        .btn-item-sujet    -> /{lang}/sujets/
        .btn-item-document -> /{lang}/documents/
        .btn-item-article  -> /{lang}/articles/
        .btn-item-annale   -> /{lang}/annales/
    """
    kind = (item_class or "").replace("btn-item-", "").strip()
    plural = {
        "sujet": "sujets",
        "document": "documents",
        "article": "articles",
        "annale": "annales",
    }.get(kind)
    if not plural:
        return ""
    return f"/{lang}/{plural}/{token}"


if __name__ == "__main__":
    # اختبار سريع (قيم حقيقية من الموقع)
    tests = [
        ("XDtaWV06OThWTE5MXjlJPVw5bDhWgl5xbWBSOWKCOEE=", "T3RQU210NDFDV1A5T1d0NzVieXJ1Zz09"),
        ("VlxeTWFdOV5sdV2AanU9fVk4XnVhgkmBXnRvf2KCOEE=", "NTVEYU1VdmUxbm5uQ0VmYzAyVlgwZz09"),
    ]
    for data_id, expected in tests:
        got = dz_decode(data_id)
        assert got == expected, f"{got!r} != {expected!r}"
        print("OK:", got)
    print("dz_decode: all tests passed")
