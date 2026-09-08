use anyhow::{Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;

static ILLEGAL_FILENAME_CHARS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"[\\/:*?"<>|'’`“”]"#).expect("valid filename regex"));
static UNDERSCORE_RUNS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"_+").expect("valid underscore regex"));
static UUID: Lazy<Regex> = Lazy::new(|| {
    Regex::new("^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$").expect("valid UUID regex")
});

/// The etp_rt cookie is a single UUID. A wrong one only shows up as an opaque 403 from
/// the token endpoint, so name the likely mistake before the request goes out.
pub fn check_etp_rt(value: &str) -> Result<()> {
    if UUID.is_match(value) {
        return Ok(());
    }
    // Double-clicking the cookie value in dev tools selects it twice often enough to be
    // worth calling out by name. This one is provably wrong, so it is worth refusing.
    if let Some(half) = value
        .get(..36)
        .filter(|half| UUID.is_match(half) && value.len() == 72 && value.ends_with(half))
    {
        bail!("--etp-rt looks like the cookie pasted twice; pass it once, as {half}");
    }
    // Any other shape is only a guess on our part, so say so and carry on rather than
    // blocking on an assumption about a format Crunchyroll is free to change.
    eprintln!(
        "! --etp-rt does not look like an etp_rt cookie (expected a 36-character UUID, got {} characters). Trying it anyway.",
        value.chars().count()
    );
    Ok(())
}

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

pub fn language_name(locale: &str) -> &str {
    match locale {
        "ja-JP" => "日本語",
        "en-US" => "English",
        "en-IN" => "English (India)",
        "id-ID" => "Bahasa Indonesia",
        "ms-MY" => "Bahasa Melayu",
        "ca-ES" => "Català",
        "de-DE" => "Deutsch",
        "es-419" => "Español (América Latina)",
        "es-ES" => "Español (España)",
        "fr-FR" => "Français",
        "it-IT" => "Italiano",
        "pl-PL" => "Polski",
        "pt-BR" => "Português (Brasil)",
        "pt-PT" => "Português (Portugal)",
        "vi-VN" => "Tiếng Việt",
        "tr-TR" => "Türkçe",
        "ru-RU" => "Русский",
        "ar-SA" => "العربية",
        "hi-IN" => "हिंदी",
        "ta-IN" => "தமிழ்",
        "te-IN" => "తెలుగు",
        "zh-CN" => "中文 (普通话)",
        "zh-HK" => "中文 (粵語)",
        "zh-TW" => "中文 (國語)",
        "ko-KR" => "한국어",
        "th-TH" => "ไทย",
        _ => locale,
    }
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
    use super::{check_etp_rt, parse_langs, parse_url, sanitize_filename};

    #[test]
    fn refuses_only_a_doubled_etp_rt() {
        let cookie = "49053edb-abd4-5da2-bacf-57b78b845946";
        assert!(check_etp_rt(cookie).is_ok());

        let doubled = check_etp_rt(&format!("{cookie}{cookie}")).unwrap_err();
        assert!(format!("{doubled:#}").contains("pasted twice"));
        assert!(format!("{doubled:#}").contains(cookie));

        // Two different UUIDs are not a doubled paste, and an unknown shape is only a
        // guess, so neither blocks the run.
        assert!(check_etp_rt(&format!("{cookie}00000000-0000-0000-0000-000000000000")).is_ok());
        assert!(check_etp_rt("some-other-format").is_ok());
    }

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
