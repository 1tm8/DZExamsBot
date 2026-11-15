//! dzenc.rs — فك تشويش معرّف الروابط (data-id) في موقع dzexams.com
//!
//! الموقع يخفي روابط الموارد خلف وسم data-id مشوّش:
//!     1) فك ترميز Base64
//!     2) طرح 8 من كل بايت
//! الناتج هو رمز (token) يُستخدم في مسار المحتوى:
//!     /ar/sujets/{token} أو /ar/documents/{token}
//!     أو /ar/articles/{token} أو /ar/annales/{token}
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

/// فك تشويش data-id وإرجاع رمز الرابط الجاهز للاستخدام في المسار.
pub fn dz_decode(data_id: &str) -> String {
    if data_id.is_empty() {
        return String::new();
    }
    let raw = match STANDARD.decode(data_id) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    raw.iter().map(|b| b.wrapping_sub(8) as char).collect()
}

/// تحويل صنف العنصر (btn-item-xxx) إلى مسار كامل.
///
/// الخريطة مأخوذة من web.js الرسمي للموقع:
///     .btn-item-sujet    -> /{lang}/sujets/
///     .btn-item-document -> /{lang}/documents/
///     .btn-item-article  -> /{lang}/articles/
///     .btn-item-annale   -> /{lang}/annales/
pub fn content_path(item_class: &str, token: &str, lang: &str) -> String {
    let kind = item_class.replace("btn-item-", "");
    let kind = kind.trim();
    let plural = match kind {
        "sujet" => "sujets",
        "document" => "documents",
        "article" => "articles",
        "annale" => "annales",
        _ => return String::new(),
    };
    format!("/{lang}/{plural}/{token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_known_values() {
        // قيم حقيقية من الموقع
        let tests = [
            (
                "XDtaWV06OThWTE5MXjlJPVw5bDhWgl5xbWBSOWKCOEE=",
                "T3RQU210NDFDV1A5T1d0NzVieXJ1Zz09",
            ),
            (
                "VlxeTWFdOV5sdV2AanU9fVk4XnVhgkmBXnRvf2KCOEE=",
                "NTVEYU1VdmUxbm5uQ0VmYzAyVlgwZz09",
            ),
        ];
        for (data_id, expected) in tests {
            assert_eq!(dz_decode(data_id), expected);
        }
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(dz_decode(""), "");
        assert_eq!(dz_decode("!!not base64!!"), "");
    }

    #[test]
    fn paths() {
        assert_eq!(
            content_path("btn-item-sujet", "abc", "ar"),
            "/ar/sujets/abc"
        );
        assert_eq!(
            content_path("btn-item-annale", "xyz", "fr"),
            "/fr/annales/xyz"
        );
        assert_eq!(content_path("btn-item-unknown", "abc", "ar"), "");
    }
}
