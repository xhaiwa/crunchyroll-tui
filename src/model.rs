use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Subtitle {
    #[serde(default)]
    #[allow(dead_code)]
    pub language: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Episode {
    #[serde(default, rename = "url")]
    pub manifest_url: String,
    #[serde(default)]
    pub subtitles: HashMap<String, Subtitle>,
    #[serde(default)]
    pub captions: HashMap<String, Subtitle>,
    #[serde(default)]
    pub token: String,
    #[serde(default, deserialize_with = "deserialize_episode_error")]
    pub error: String,
    #[serde(default)]
    pub reason: String,
}

fn deserialize_episode_error<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => {
            String::new()
        }
        Some(serde_json::Value::Number(ref number)) if number.as_i64() == Some(0) => String::new(),
        Some(serde_json::Value::String(message)) => message,
        Some(other) => other.to_string(),
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DubVersion {
    #[serde(default)]
    pub audio_locale: String,
    #[serde(default)]
    pub guid: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EpisodeMetadata {
    #[serde(default)]
    pub audio_locale: String,
    #[serde(default)]
    pub episode_number: i32,
    #[serde(default)]
    pub season_number: i32,
    #[serde(default)]
    pub series_title: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub availability_starts: String,
    #[serde(default)]
    pub versions: Vec<DubVersion>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EpisodeInfo {
    #[serde(default)]
    pub episode_metadata: EpisodeMetadata,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Deserialize)]
pub struct EpisodeMetadataResponse {
    #[serde(default)]
    pub data: Vec<EpisodeInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeasonEpisode {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub versions: Vec<DubVersion>,
    #[serde(default)]
    pub season_number: i32,
    #[serde(default)]
    pub episode_number: i32,
    #[serde(default)]
    pub series_title: String,
    #[serde(default)]
    pub audio_locale: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub availability_starts: String,
}

#[derive(Debug, Deserialize)]
pub struct SeasonEpisodesResponse {
    #[serde(default)]
    pub data: Vec<SeasonEpisode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Season {
    pub id: String,
    #[serde(default)]
    pub season_number: i32,
}

#[derive(Debug, Deserialize)]
pub struct SeasonsResponse {
    #[serde(default)]
    pub data: Vec<Season>,
}

#[cfg(test)]
mod tests {
    use super::Episode;

    #[test]
    fn accepts_all_playback_error_shapes() {
        let cases = [
            (r#"{"error":"region locked"}"#, "region locked", ""),
            (r#"{"error":false}"#, "", ""),
            (r#"{"error":null}"#, "", ""),
            (r#"{"error":0}"#, "", ""),
            (r#"{"error":403}"#, "403", ""),
            (r#"{"error":true}"#, "true", ""),
            (r#"{}"#, "", ""),
            (
                r#"{"error":4294,"reason":"Too many requests"}"#,
                "4294",
                "Too many requests",
            ),
        ];
        for (json, error, reason) in cases {
            let episode: Episode = serde_json::from_str(json).unwrap();
            assert_eq!(episode.error, error);
            assert_eq!(episode.reason, reason);
        }
    }
}
