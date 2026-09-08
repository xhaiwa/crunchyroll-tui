use anyhow::{Context, Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

const WIDEVINE_SCHEME_SUFFIX: &str = "edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename = "MPD")]
pub struct Manifest {
    #[serde(rename = "Period", default)]
    pub periods: Vec<Period>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Period {
    #[serde(rename = "AdaptationSet", default)]
    pub adaptation_sets: Vec<AdaptationSet>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AdaptationSet {
    #[serde(rename = "@contentType", default)]
    pub content_type: String,
    #[serde(rename = "@mimeType", default)]
    pub mime_type: String,
    #[serde(rename = "ContentProtection", default)]
    pub content_protections: Vec<ContentProtection>,
    #[serde(rename = "SegmentTemplate", default)]
    pub segment_template: Option<SegmentTemplate>,
    #[serde(rename = "Representation", default)]
    pub representations: Vec<Representation>,
}

impl AdaptationSet {
    pub fn is_video(&self) -> bool {
        self.content_type == "video"
            || self.mime_type.starts_with("video/")
            || self.representations.iter().any(|rep| rep.height.is_some())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContentProtection {
    #[serde(rename = "@schemeIdUri", default)]
    pub scheme_id_uri: String,
    #[serde(rename = "pssh", alias = "cenc:pssh", default)]
    pub pssh: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Representation {
    #[serde(rename = "@id", default)]
    pub id: String,
    #[serde(rename = "@bandwidth", default)]
    pub bandwidth: Option<u64>,
    #[serde(rename = "@height", default)]
    pub height: Option<u64>,
    #[serde(rename = "BaseURL", default)]
    pub base_url: String,
    #[serde(rename = "ContentProtection", default)]
    pub content_protections: Vec<ContentProtection>,
    #[serde(rename = "SegmentBase", default)]
    pub segment_base: Option<SegmentBase>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SegmentTemplate {
    #[serde(rename = "@initialization", default)]
    pub initialization: String,
    #[serde(rename = "@media", default)]
    pub media: String,
    #[serde(rename = "@startNumber", default)]
    pub start_number: Option<i64>,
    #[serde(rename = "SegmentTimeline", default)]
    pub timeline: SegmentTimeline,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SegmentTimeline {
    #[serde(rename = "S", default)]
    pub segments: Vec<TimelineSegment>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TimelineSegment {
    #[serde(rename = "@r", default)]
    pub repeat: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SegmentBase {
    #[serde(rename = "@indexRange", default)]
    pub index_range: String,
    #[serde(rename = "Initialization", default)]
    pub initialization: Initialization,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Initialization {
    #[serde(rename = "@range", default)]
    pub range: String,
}

pub fn parse_manifest(body: &[u8]) -> Result<Manifest> {
    quick_xml::de::from_reader(body).context("parse DASH manifest")
}

pub fn is_on_demand(manifest: &Manifest) -> bool {
    manifest
        .periods
        .first()
        .and_then(|period| period.adaptation_sets.first())
        .is_some_and(|set| set.segment_template.is_none())
}

/// Only a Widevine PSSH counts. A PlayReady or ClearKey box carries a payload no
/// Widevine CDM can read, and handing one to the license request buys nothing but an
/// unreadable error several layers down.
fn pssh_from_protections(protections: &[ContentProtection]) -> Option<String> {
    protections
        .iter()
        .find(|protection| {
            protection.pssh.is_some() && protection.scheme_id_uri.contains(WIDEVINE_SCHEME_SUFFIX)
        })
        .and_then(|protection| protection.pssh.clone())
}

pub fn get_pssh(manifest: &Manifest) -> Option<String> {
    let sets = &manifest.periods.first()?.adaptation_sets;
    for set in sets {
        if let Some(pssh) = pssh_from_protections(&set.content_protections) {
            return Some(pssh);
        }
        for representation in &set.representations {
            if let Some(pssh) = pssh_from_protections(&representation.content_protections) {
                return Some(pssh);
            }
        }
    }
    None
}

pub fn expand_timeline(timeline: &[TimelineSegment], start_number: i64) -> Vec<i64> {
    let mut number = start_number;
    let mut result = Vec::new();
    for segment in timeline {
        let count = segment.repeat.unwrap_or_default().max(0) + 1;
        for _ in 0..count {
            result.push(number);
            number += 1;
        }
    }
    result
}

/// `$Number$` and the `$Number%0Nd$` width variants DASH allows in their place.
static NUMBER_PLACEHOLDER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$Number(?:%0?(\d+)d)?\$").expect("valid $Number$ regex"));

pub fn build_url(base: &str, representation_id: &str, template: &str, part: Option<i64>) -> String {
    let mut file = template.replace("$RepresentationID$", representation_id);
    if let Some(part) = part {
        // ISO/IEC 23009-1 wants the bare integer for `$Number$`; only the explicit
        // `%0Nd` form is padded, and to whatever width it asks for. Padding `$Number$`
        // to five digits turns segment 1 into 00001 and earns a 404, and any width the
        // substitution does not know about would otherwise survive into the URL and
        // fail just as quietly.
        file = NUMBER_PLACEHOLDER
            .replace_all(&file, |captures: &regex::Captures<'_>| {
                match captures.get(1).map(|width| width.as_str().parse()) {
                    Some(Ok(width)) => format!("{part:0width$}"),
                    _ => part.to_string(),
                }
            })
            .into_owned();
    }
    format!("{base}{file}")
}

pub fn select_representation<'a>(
    set: &'a AdaptationSet,
    is_video: bool,
    quality: &str,
) -> Result<&'a Representation> {
    if set.representations.is_empty() {
        bail!("adaptation set has no representations");
    }
    let selected = if is_video {
        quality
            .trim_end_matches('p')
            .parse::<u64>()
            .ok()
            .and_then(|target| {
                set.representations
                    .iter()
                    .find(|representation| representation.height == Some(target))
            })
    } else if set
        .representations
        .iter()
        .any(|rep| rep.id.contains("audio/"))
    {
        set.representations
            .iter()
            .find(|representation| representation.id.contains(quality))
    } else {
        let target = quality.trim_end_matches('k').parse::<u64>().ok();
        target.and_then(|target| {
            let minimum = match target {
                192 => 192_000,
                128 => 128_000,
                96 => 96_000,
                other => other * 1_000,
            };
            set.representations
                .iter()
                .filter(|rep| rep.bandwidth.is_some_and(|bandwidth| bandwidth >= minimum))
                .min_by_key(|rep| rep.bandwidth.unwrap_or(u64::MAX))
        })
    };

    if let Some(selected) = selected {
        return Ok(selected);
    }
    let first = &set.representations[0];
    println!(
        "{} quality {} not found, deferring to {}",
        if is_video { "Video" } else { "Audio" },
        quality,
        first.id
    );
    Ok(first)
}

pub fn parse_byte_range(range: &str) -> Result<(u64, u64)> {
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("invalid byte range {range:?}"))?;
    if start.is_empty() || end.is_empty() {
        bail!("invalid byte range {range:?}");
    }
    Ok((
        start
            .parse()
            .with_context(|| format!("invalid byte range {range:?}"))?,
        end.parse()
            .with_context(|| format!("invalid byte range {range:?}"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON_DEMAND: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:cenc="urn:mpeg:cenc:2013">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <Representation id="720p" bandwidth="4386044" height="720">
        <BaseURL>https://example.com/video.mp4</BaseURL>
        <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"><cenc:pssh>d2lkZXZpbmU=</cenc:pssh></ContentProtection>
        <SegmentBase indexRange="1838-6129"><Initialization range="0-1837"/></SegmentBase>
      </Representation>
      <Representation id="1080p" bandwidth="16481538" height="1080">
        <BaseURL>https://example.com/video-1080.mp4</BaseURL>
        <SegmentBase indexRange="1839-6130"><Initialization range="0-1838"/></SegmentBase>
      </Representation>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4">
      <Representation id="192k" bandwidth="199094">
        <BaseURL>https://example.com/audio.mp4</BaseURL>
        <SegmentBase indexRange="1708-6011"><Initialization range="0-1707"/></SegmentBase>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn parses_on_demand_manifest() {
        let manifest = parse_manifest(ON_DEMAND.as_bytes()).unwrap();
        assert!(is_on_demand(&manifest));
        let sets = &manifest.periods[0].adaptation_sets;
        assert_eq!(sets.len(), 2);
        assert!(sets[0].is_video());
        assert_eq!(sets[0].representations[1].height, Some(1080));
        assert_eq!(
            sets[0].representations[1]
                .segment_base
                .as_ref()
                .unwrap()
                .initialization
                .range,
            "0-1838"
        );
        assert_eq!(get_pssh(&manifest).as_deref(), Some("d2lkZXZpbmU="));
        assert_eq!(
            select_representation(&sets[0], true, "1080p").unwrap().id,
            "1080p"
        );
        assert_eq!(
            select_representation(&sets[1], false, "192k").unwrap().id,
            "192k"
        );
    }

    #[test]
    fn parses_ranges() {
        assert_eq!(parse_byte_range("0-1837").unwrap(), (0, 1837));
        assert!(parse_byte_range("12345-").is_err());
        assert!(parse_byte_range("").is_err());
        assert!(parse_byte_range("abc-def").is_err());
    }

    const PLAYREADY_ONLY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:cenc="urn:mpeg:cenc:2013">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <ContentProtection schemeIdUri="urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95"><cenc:pssh>cGxheXJlYWR5</cenc:pssh></ContentProtection>
      <Representation id="720p" bandwidth="4386044" height="720">
        <BaseURL>https://example.com/video.mp4</BaseURL>
        <SegmentBase indexRange="1838-6129"><Initialization range="0-1837"/></SegmentBase>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn refuses_a_manifest_without_a_widevine_pssh() {
        let manifest = parse_manifest(PLAYREADY_ONLY.as_bytes()).unwrap();
        // Handing the PlayReady payload to the Widevine CDM would fail much later and
        // much less legibly than saying there is no Widevine PSSH here.
        assert_eq!(get_pssh(&manifest), None);
    }

    #[test]
    fn builds_segment_urls() {
        // The spec's plain `$Number$` is the raw integer: padding it to five digits was
        // a 404 for every segment.
        assert_eq!(
            build_url(
                "https://cdn/",
                "video/1080p",
                "$RepresentationID$-$Number$.m4s",
                Some(2)
            ),
            "https://cdn/video/1080p-2.m4s"
        );
        // Every width the manifest asks for, not just the one that used to be special
        // cased.
        for (template, expected) in [
            ("seg-$Number%05d$.m4s", "https://cdn/seg-00002.m4s"),
            ("seg-$Number%03d$.m4s", "https://cdn/seg-002.m4s"),
            ("seg-$Number%4d$.m4s", "https://cdn/seg-0002.m4s"),
        ] {
            assert_eq!(build_url("https://cdn/", "id", template, Some(2)), expected);
        }
        // A number wider than the requested padding keeps all of its digits.
        assert_eq!(
            build_url("https://cdn/", "id", "seg-$Number%03d$.m4s", Some(12345)),
            "https://cdn/seg-12345.m4s"
        );
        // Two placeholders in one template, and a template with none at all.
        assert_eq!(
            build_url("https://cdn/", "id", "$Number$/$Number%03d$.m4s", Some(7)),
            "https://cdn/7/007.m4s"
        );
        assert_eq!(
            build_url("https://cdn/", "id", "init.mp4", None),
            "https://cdn/init.mp4"
        );
    }
}
