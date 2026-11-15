//! catalog.rs — دليل المستويات والمواد والأقسام (ثابت + أسماء عربية)
//!
//! البنية المكتشفة في الموقع:
//! - مستويات التعليم (0ap..5ap, 1am..4am, 1as..3as): `/ar/{level}/{subject}/{section}`
//!   الأقسام: cours, d1, d2, d3, e1, e2, e3, evaluations
//! - شهادتان (bem, bep): `/ar/{level}/{subject}` (العناصر مضمّنة — annales)
//! - البكالوريا (bac): `/ar/bac/{subject}` (صفحة اختيار الشعب)،
//!   و `/ar/bac/{subject}/{stream}` (se, m, mt, ges, lit — عناصر annales مضمّنة)
use std::collections::HashMap;

use once_cell::sync::Lazy;

// ─── المستويات ────────────────────────────────────────────────────────────
pub const LEVELS: &[(&str, &str)] = &[
    ("1ap", "السنة الأولى ابتدائي"),
    ("2ap", "السنة الثانية ابتدائي"),
    ("3ap", "السنة الثالثة ابتدائي"),
    ("4ap", "السنة الرابعة ابتدائي"),
    ("5ap", "السنة الخامسة ابتدائي"),
    ("1am", "السنة الأولى متوسط"),
    ("2am", "السنة الثانية متوسط"),
    ("3am", "السنة الثالثة متوسط"),
    ("4am", "السنة الرابعة متوسط"),
    ("1as", "السنة الأولى ثانوي"),
    ("2as", "السنة الثانية ثانوي"),
    ("3as", "السنة الثالثة ثانوي"),
    ("bem", "شهادة التعليم المتوسط BEM"),
    ("bep", "شهادة التكوين المهني BEP"),
    ("bac", "شهادة البكالوريا BAC"),
];

pub const EXAM_LEVELS: &[&str] = &["bem", "bep"];
pub const STREAM_LEVELS: &[&str] = &["bac"];

// ─── الأطوار ─────────────────────────────────────────────────────────────
pub const STAGES: &[(&str, &str, &[&str])] = &[
    (
        "primary",
        "الطور الابتدائي",
        &["1ap", "2ap", "3ap", "4ap", "5ap"],
    ),
    ("middle", "الطور المتوسط", &["1am", "2am", "3am", "4am"]),
    ("secondary", "الطور الثانوي", &["1as", "2as", "3as"]),
    ("exams", "الشهادات", &["bem", "bep", "bac"]),
];

// ─── الشعب (البكالوريا فقط) ──────────────────────────────────────────────
pub const STREAMS: &[(&str, &str)] = &[
    ("se", "شعبة علوم تجريبية"),
    ("m", "شعبة رياضيات"),
    ("mt", "شعبة تقني رياضي"),
    ("ges", "شعبة تسيير واقتصاد"),
    ("lit", "شعبة آداب وفلسفة"),
];

// ─── شعب الطور الثانوي (من data-tags في صفحات الموقع) ────────────────────
// الأولى ثانوي جذع مشترك؛ الثانية/الثالثة ثانوي خمس شعب برموز مختلفة عن
// رموز البكالوريا في الروابط (tm بدل mt، و al/ge بدل lit/ges).
pub static SCHOOL_STREAMS: Lazy<HashMap<&'static str, &'static [(&'static str, &'static str)]>> =
    Lazy::new(|| {
        HashMap::from([
            (
                "1as",
                &[
                    ("tcst", "جذع مشترك علوم وتكنولوجيا"),
                    ("tcl", "جذع مشترك آداب"),
                ][..],
            ),
            (
                "2as",
                &[
                    ("se", "علوم تجريبية"),
                    ("m", "رياضيات"),
                    ("tm", "تقني رياضي"),
                    ("al", "آداب ولغات أجنبية"),
                    ("ge", "تسيير واقتصاد"),
                ][..],
            ),
            (
                "3as",
                &[
                    ("se", "علوم تجريبية"),
                    ("m", "رياضيات"),
                    ("tm", "تقني رياضي"),
                    ("al", "آداب ولغات أجنبية"),
                    ("ge", "تسيير واقتصاد"),
                ][..],
            ),
        ])
    });

/// شعب المستوى إن كان له شعب تُختار قبل المادة، وإلا قائمة فارغة.
pub fn streams_for_level(level: &str) -> &'static [(&'static str, &'static str)] {
    SCHOOL_STREAMS.get(level).copied().unwrap_or(&[])
}

// ─── الأقسام داخل كل مادة ────────────────────────────────────────────────
pub const SECTIONS: &[(&str, &str)] = &[
    ("cours", "دروس وتمارين"),
    ("d1", "فروض الفصل الأول"),
    ("d2", "فروض الفصل الثاني"),
    ("d3", "فروض الفصل الثالث"),
    ("e1", "اختبارات الفصل الأول"),
    ("e2", "اختبارات الفصل الثاني"),
    ("e3", "اختبارات الفصل الثالث"),
    ("evaluations", "اختبارات وتقييمات"),
];

// ─── المواد حسب المستوى (مستخرجة من الموقع مباشرة) ───────────────────────
pub static SUBJECTS: Lazy<HashMap<&'static str, &'static [&'static str]>> = Lazy::new(|| {
    HashMap::from([
        (
            "0ap",
            &[
                "arabe",
                "dessin",
                "ecriture",
                "mathematiques",
                "tarbia-islamia",
                "tarbia-madania",
                "technologie",
            ][..],
        ),
        (
            "1ap",
            &[
                "activites",
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "mathematiques",
                "musique",
                "tarbia-islamia",
                "tarbia-madania",
                "technologie",
            ][..],
        ),
        (
            "2ap",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "mathematiques",
                "musique",
                "tarbia-islamia",
                "tarbia-madania",
                "technologie",
            ][..],
        ),
        (
            "3ap",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "mathematiques",
                "musique",
                "tarbia-islamia",
                "tarbia-madania",
                "technologie",
            ][..],
        ),
        (
            "4ap",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "mathematiques",
                "musique",
                "tamazight",
                "tarbia-islamia",
                "tarbia-madania",
                "technologie",
            ][..],
        ),
        (
            "5ap",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "mathematiques",
                "musique",
                "tamazight",
                "tarbia-islamia",
                "tarbia-madania",
                "technologie",
            ][..],
        ),
        (
            "1am",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "informatique",
                "mathematiques",
                "musique",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
                "tarbia-madania",
            ][..],
        ),
        (
            "2am",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "informatique",
                "mathematiques",
                "musique",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
                "tarbia-madania",
            ][..],
        ),
        (
            "3am",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "informatique",
                "mathematiques",
                "musique",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
                "tarbia-madania",
            ][..],
        ),
        (
            "4am",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "informatique",
                "mathematiques",
                "musique",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
                "tarbia-madania",
            ][..],
        ),
        (
            "1as",
            &[
                "anglais",
                "arabe",
                "dessin",
                "francais",
                "histoire-geographie",
                "informatique",
                "mathematiques",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
                "technologie-st",
            ][..],
        ),
        (
            "2as",
            &[
                "allemand",
                "anglais",
                "arabe",
                "comptabilite",
                "dessin",
                "droit",
                "economie-management",
                "espagnol",
                "francais",
                "genie-civil",
                "genie-electrique",
                "genie-mecanique",
                "genie-procedes",
                "histoire-geographie",
                "italien",
                "mathematiques",
                "philosophie",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
            ][..],
        ),
        (
            "3as",
            &[
                "allemand",
                "anglais",
                "arabe",
                "comptabilite",
                "droit",
                "economie-management",
                "espagnol",
                "francais",
                "genie-civil",
                "genie-electrique",
                "genie-mecanique",
                "genie-procedes",
                "histoire-geographie",
                "italien",
                "mathematiques",
                "philosophie",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
            ][..],
        ),
        (
            "bem",
            &[
                "anglais",
                "arabe",
                "francais",
                "histoire-geographie",
                "mathematiques",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
                "tarbia-madania",
            ][..],
        ),
        ("bep", &["arabe", "francais", "mathematiques"][..]),
        (
            "bac",
            &[
                "allemand",
                "anglais",
                "arabe",
                "comptabilite",
                "droit",
                "economie-management",
                "espagnol",
                "francais",
                "genie-civil",
                "genie-electrique",
                "genie-mecanique",
                "genie-procedes",
                "histoire-geographie",
                "italien",
                "mathematiques",
                "philosophie",
                "physique",
                "sciences-naturelles",
                "tamazight",
                "tarbia-islamia",
            ][..],
        ),
    ])
});

// أسماء المواد بالعربية
pub static SUBJECT_NAMES: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    HashMap::from([
        ("activites", "أنشطة علمية"),
        ("allemand", "الألمانية"),
        ("anglais", "الإنجليزية"),
        ("arabe", "اللغة العربية"),
        ("comptabilite", "المحاسبة والمالية"),
        ("dessin", "الرسم"),
        ("droit", "القانون"),
        ("economie-management", "الاقتصاد والتسيير"),
        ("ecriture", "الكتابة"),
        ("espagnol", "الإسبانية"),
        ("francais", "اللغة الفرنسية"),
        ("genie-civil", "الهندسة المدنية"),
        ("genie-electrique", "الهندسة الكهربائية"),
        ("genie-mecanique", "الهندسة الميكانيكية"),
        ("genie-procedes", "هندسة الطرائق"),
        ("histoire-geographie", "التاريخ والجغرافيا"),
        ("informatique", "المعلوماتية"),
        ("italien", "الإيطالية"),
        ("mathematiques", "الرياضيات"),
        ("musique", "الموسيقى"),
        ("philosophie", "الفلسفة"),
        ("physique", "الفيزياء"),
        ("sciences-naturelles", "علوم الطبيعة والحياة"),
        ("tamazight", "الأمازيغية"),
        ("tarbia-islamia", "التربية الإسلامية"),
        ("tarbia-madania", "التربية المدنية"),
        ("technologie", "التكنولوجيا"),
        ("technologie-st", "التكنولوجيا (علوم وتكنولوجيا)"),
    ])
});

static LEVEL_NAMES: Lazy<HashMap<&'static str, &'static str>> =
    Lazy::new(|| LEVELS.iter().copied().collect());

static SECTION_NAMES: Lazy<HashMap<&'static str, &'static str>> =
    Lazy::new(|| SECTIONS.iter().copied().collect());

// ─── دوال مساعدة ─────────────────────────────────────────────────────────

pub fn level_name(level: &str) -> &str {
    LEVEL_NAMES.get(level).copied().unwrap_or(level)
}

pub fn subject_ar(slug: &str) -> &str {
    SUBJECT_NAMES.get(slug).copied().unwrap_or(slug)
}

pub fn section_ar(slug: &str) -> &str {
    SECTION_NAMES.get(slug).copied().unwrap_or(slug)
}

pub fn subjects_for(level: &str) -> &'static [&'static str] {
    SUBJECTS.get(level).copied().unwrap_or(&[])
}

pub fn is_valid_level(level: &str) -> bool {
    LEVEL_NAMES.contains_key(level)
}

/// الطور الذي ينتمي إليه المستوى (افتراضيًا "exams").
pub fn stage_of_level(level: &str) -> &'static str {
    for (stage, _name, levels) in STAGES {
        if levels.contains(&level) {
            return stage;
        }
    }
    "exams"
}

pub fn stage_name(stage: &str) -> &str {
    STAGES
        .iter()
        .find(|(s, _, _)| *s == stage)
        .map(|(_, n, _)| *n)
        .unwrap_or(stage)
}

/// اسم الشعبة بالعربية حسب المستوى (رموز الثانوي تختلف عن البكالوريا).
pub fn stream_ar<'a>(level: &str, code: &'a str) -> &'a str {
    if let Some(map) = SCHOOL_STREAMS.get(level) {
        if let Some((_, name)) = map.iter().find(|(c, _)| *c == code) {
            return name;
        }
    }
    STREAMS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| *n)
        .unwrap_or(code)
}

/// اسم شعبة البكالوريا (يُستعمل في صفحة اختيار الشعب).
pub fn bac_stream_name(code: &str) -> &str {
    STREAMS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| *n)
        .unwrap_or(code)
}

pub fn subject_url(level: &str, subject: &str) -> String {
    format!("/ar/{level}/{subject}")
}

pub fn section_url(level: &str, subject: &str, section: &str) -> String {
    format!("/ar/{level}/{subject}/{section}")
}

pub fn stream_url(_level: &str, subject: &str, stream: &str) -> String {
    format!("/ar/bac/{subject}/{stream}")
}
