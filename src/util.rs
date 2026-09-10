use once_cell::sync::Lazy;
use regex::Regex;

static ILLEGAL_FILENAME_CHARS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"[\\/:*?"<>|'’`“”]"#).expect("valid filename regex"));
static UNDERSCORE_RUNS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"_+").expect("valid underscore regex"));

pub fn parse_langs(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn parse_url(url: &str) -> Option<(String, String)> {
    let parts: Vec<_> = url.trim_end_matches('/').split('/').collect();
    parts.windows(2).find_map(|pair| {
        matches!(pair[0], "watch" | "series")
            .then(|| (pair[0].to_owned(), pair[1].to_owned()))
            .filter(|(_, id)| !id.is_empty())
    })
}

pub fn sanitize_filename(name: &str) -> String {
    if name.is_empty() {
        return "Unknown".to_owned();
    }
    let replaced = ILLEGAL_FILENAME_CHARS.replace_all(name, "_");
    UNDERSCORE_RUNS
        .replace_all(&replaced, "_")
        .trim_end_matches([' ', '.'])
        .to_owned()
}

/// Every locale Crunchyroll publishes, and the name it gives it. One table, because the
/// interface offers the same list it looks names up in.
pub const LANGUAGES: [(&str, &str); 26] = [
    ("ja-JP", "日本語"),
    ("en-US", "English"),
    ("en-IN", "English (India)"),
    ("id-ID", "Bahasa Indonesia"),
    ("ms-MY", "Bahasa Melayu"),
    ("ca-ES", "Català"),
    ("de-DE", "Deutsch"),
    ("es-419", "Español (América Latina)"),
    ("es-ES", "Español (España)"),
    ("fr-FR", "Français"),
    ("it-IT", "Italiano"),
    ("pl-PL", "Polski"),
    ("pt-BR", "Português (Brasil)"),
    ("pt-PT", "Português (Portugal)"),
    ("vi-VN", "Tiếng Việt"),
    ("tr-TR", "Türkçe"),
    ("ru-RU", "Русский"),
    ("ar-SA", "العربية"),
    ("hi-IN", "हिंदी"),
    ("ta-IN", "தமிழ்"),
    ("te-IN", "తెలుగు"),
    ("zh-CN", "中文 (普通话)"),
    ("zh-HK", "中文 (粵語)"),
    ("zh-TW", "中文 (國語)"),
    ("ko-KR", "한국어"),
    ("th-TH", "ไทย"),
];

pub fn language_name(locale: &str) -> &str {
    LANGUAGES
        .iter()
        .find(|(code, _)| *code == locale)
        .map_or(locale, |(_, name)| *name)
}

pub fn language_code(locale: &str) -> &str {
    match locale {
        "ja-JP" => "jpn",
        "en-US" | "en-IN" => "eng",
        "id-ID" => "ind",
        "ms-MY" => "msa",
        "ca-ES" => "cat",
        "de-DE" => "deu",
        "es-419" | "es-ES" => "spa",
        "fr-FR" => "fra",
        "it-IT" => "ita",
        "pl-PL" => "pol",
        "pt-BR" | "pt-PT" => "por",
        "vi-VN" => "vie",
        "tr-TR" => "tur",
        "ru-RU" => "rus",
        "ar-SA" => "ara",
        "hi-IN" => "hin",
        "ta-IN" => "tam",
        "te-IN" => "tel",
        "zh-CN" | "zh-HK" | "zh-TW" => "zho",
        "ko-KR" => "kor",
        "th-TH" => "tha",
        _ => locale,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_langs, parse_url, sanitize_filename};

    #[test]
    fn parses_localized_urls() {
        assert_eq!(
            parse_url("https://www.crunchyroll.com/fr/series/G0XHWM0D3/title"),
            Some(("series".into(), "G0XHWM0D3".into()))
        );
        assert_eq!(
            parse_url("https://www.crunchyroll.com/watch/GE00198973JAJP/episode/"),
            Some(("watch".into(), "GE00198973JAJP".into()))
        );
        assert_eq!(parse_url("https://www.crunchyroll.com/"), None);
        assert_eq!(parse_url("https://www.crunchyroll.com/series"), None);
    }

    #[test]
    fn parses_language_lists() {
        assert_eq!(parse_langs("ja-JP, en-US,,"), ["ja-JP", "en-US"]);
    }

    #[test]
    fn sanitizes_file_names() {
        let cases = [
            ("", "Unknown"),
            (
                "Frieren Beyond Journey's End",
                "Frieren Beyond Journey_s End",
            ),
            ("a/b\\c", "a_b_c"),
            (r#"a:b*c?d"e<f>g|h"#, "a_b_c_d_e_f_g_h"),
            ("“Hello” ‘s ’t", "_Hello_ ‘s _t"),
            ("a`b", "a_b"),
            ("a///b", "a_b"),
            ("a___b", "a_b"),
            ("Episode 1. . ", "Episode 1"),
            (" Episode", " Episode"),
            ("進撃の巨人", "進撃の巨人"),
            ("///", "_"),
        ];
        for (input, expected) in cases {
            assert_eq!(sanitize_filename(input), expected, "input: {input:?}");
        }
    }
}
