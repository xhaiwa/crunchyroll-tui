use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

/// Crunchyroll sends `null` for a field it has no value for - the episode number of a
/// special, the description of a season - rather than leaving it out, and
/// `#[serde(default)]` only covers a field that is missing entirely.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// One rendition of a piece of artwork. Crunchyroll publishes every poster and every
/// thumbnail at half a dozen widths, so the one that suits the panel can be asked for
/// rather than the largest being fetched and then thrown away.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Artwork {
    #[serde(default, deserialize_with = "null_default")]
    pub width: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub source: String,
}

/// The artwork hanging off a catalogue entry or an episode.
///
/// Each set arrives as a list of lists - one inner list of renditions per image - so both
/// levels are flattened before anything is picked out of them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Images {
    #[serde(default, deserialize_with = "null_default")]
    pub poster_tall: Vec<Vec<Artwork>>,
    #[serde(default, deserialize_with = "null_default")]
    pub thumbnail: Vec<Vec<Artwork>>,
}

impl Images {
    /// The series poster, portrait, roughly two by three.
    pub fn poster(&self, at_least: u32) -> Option<&str> {
        widest_under(&self.poster_tall, at_least)
    }

    /// The still from the episode, sixteen by nine.
    pub fn thumbnail(&self, at_least: u32) -> Option<&str> {
        widest_under(&self.thumbnail, at_least)
    }
}

/// The narrowest rendition that still covers `at_least` pixels: nothing is upscaled, and
/// no more is pulled over the wire than the panel can show. A set that stops short of the
/// asked-for width gives up its largest instead of nothing.
fn widest_under(sets: &[Vec<Artwork>], at_least: u32) -> Option<&str> {
    let mut renditions: Vec<&Artwork> = sets
        .iter()
        .flatten()
        .filter(|artwork| !artwork.source.is_empty())
        .collect();
    renditions.sort_by_key(|artwork| artwork.width);
    renditions
        .iter()
        .find(|artwork| artwork.width >= at_least)
        .or_else(|| renditions.last())
        .map(|artwork| artwork.source.as_str())
}

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
    #[serde(default, deserialize_with = "null_default")]
    pub episode_number: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub season_number: i32,
    #[serde(default)]
    pub series_id: String,
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
    #[serde(default, deserialize_with = "null_default")]
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
    #[serde(default, deserialize_with = "null_default")]
    pub season_number: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub episode_number: i32,
    /// What Crunchyroll prints on the episode: usually the number, but "SP" or "1.5"
    /// for the specials that `episode_number` has nothing to say about.
    #[serde(default, deserialize_with = "null_default")]
    pub episode: String,
    #[serde(default)]
    pub series_title: String,
    #[serde(default)]
    pub audio_locale: String,
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_default")]
    pub duration_ms: u64,
    #[serde(default, deserialize_with = "null_default")]
    pub availability_starts: String,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Images,
}

#[derive(Debug, Deserialize)]
pub struct SeasonEpisodesResponse {
    #[serde(default)]
    pub data: Vec<SeasonEpisode>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Season {
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub season_number: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub number_of_episodes: i32,
    #[serde(default)]
    pub audio_locales: Vec<String>,
    #[serde(default)]
    pub subtitle_locales: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SeasonsResponse {
    #[serde(default)]
    pub data: Vec<Season>,
}

/// What the catalogue knows about a series before any season has been fetched.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeriesMetadata {
    #[serde(default, deserialize_with = "null_default")]
    pub episode_count: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub season_count: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub series_launch_year: i32,
    #[serde(default)]
    pub audio_locales: Vec<String>,
    #[serde(default)]
    pub subtitle_locales: Vec<String>,
    #[serde(default)]
    pub maturity_ratings: Vec<String>,
    #[serde(default)]
    pub is_dubbed: bool,
    #[serde(default)]
    pub is_simulcast: bool,
}

/// One entry of the catalogue, as returned by browse and by search.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogItem {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub description: String,
    #[serde(default)]
    pub series_metadata: SeriesMetadata,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Images,
}

/// One entry of the account's own list.
///
/// The list is kept as the episode each series was last left at rather than as the
/// series themselves, so the entry wraps an episode panel and what the catalogue
/// column wants - the series - is the id hanging off its metadata. A panel that is a
/// series already is taken as it stands, and anything else (a film, an entry whose
/// panel Crunchyroll left out) carries no series id and is passed over.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WatchlistEntry {
    #[serde(default, deserialize_with = "null_default")]
    pub panel: WatchlistPanel,
}

impl WatchlistEntry {
    /// The series this entry is about, or an empty string if it is not about one.
    pub fn series_id(&self) -> &str {
        if self.panel.kind == "series" {
            &self.panel.id
        } else {
            &self.panel.episode_metadata.series_id
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WatchlistPanel {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, deserialize_with = "null_default")]
    pub episode_metadata: EpisodeMetadata,
}

#[derive(Debug, Deserialize)]
pub struct WatchlistResponse {
    #[serde(default)]
    pub data: Vec<WatchlistEntry>,
}

/// Catalogue entries fetched by id rather than found by browsing.
#[derive(Debug, Deserialize)]
pub struct ObjectsResponse {
    #[serde(default)]
    pub data: Vec<CatalogItem>,
}

#[derive(Debug, Deserialize)]
pub struct BrowseResponse {
    #[serde(default)]
    pub data: Vec<CatalogItem>,
    #[serde(default)]
    #[allow(dead_code)]
    pub total: i64,
}

/// Search answers with one group per result type rather than a flat list.
#[derive(Debug, Deserialize)]
pub struct SearchGroup {
    #[serde(default)]
    pub items: Vec<CatalogItem>,
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub data: Vec<SearchGroup>,
}

#[cfg(test)]
mod tests {
    use super::{Episode, SeasonEpisode, SeasonEpisodesResponse, WatchlistResponse};

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

    /// The artwork arrives wrapped in a list of lists, and the rendition worth fetching
    /// is the smallest one that still covers the panel.
    #[test]
    fn picks_a_rendition_that_covers_the_panel() {
        let json = r#"{"data":[{"id":"G1","images":{
            "thumbnail":[[
                {"width":320,"height":180,"source":"small.jpg"},
                {"width":640,"height":360,"source":"medium.jpg"},
                {"width":1920,"height":1080,"source":"huge.jpg"}
            ]]
        }}]}"#;
        let response: SeasonEpisodesResponse = serde_json::from_str(json).unwrap();
        let images = &response.data[0].images;
        assert_eq!(images.thumbnail(300), Some("small.jpg"));
        // Exactly wide enough is wide enough.
        assert_eq!(images.thumbnail(320), Some("small.jpg"));
        assert_eq!(images.thumbnail(500), Some("medium.jpg"));
        // Nothing is big enough, so the biggest there is beats showing nothing.
        assert_eq!(images.thumbnail(4000), Some("huge.jpg"));
        // And a set that is not there at all is not an error.
        assert_eq!(images.poster(300), None);
    }

    /// Crunchyroll sends `"images": null` for an episode it has no still for, and a
    /// series listing may leave the field out altogether.
    #[test]
    fn takes_an_episode_with_no_artwork() {
        for json in [
            r#"{"data":[{"id":"G1","images":null}]}"#,
            r#"{"data":[{"id":"G1"}]}"#,
        ] {
            let response: SeasonEpisodesResponse = serde_json::from_str(json).unwrap();
            assert_eq!(response.data[0].images.thumbnail(320), None, "{json}");
        }
    }

    /// The account's list arrives as the episode each series was left at, so the id
    /// worth keeping is the series the panel belongs to rather than the panel's own.
    /// An entry about anything else is passed over rather than becoming an empty row.
    #[test]
    fn finds_the_series_behind_a_watchlist_entry() {
        let json = r#"{"total":3,"data":[
            {"panel":{"id":"GQJUGQG3J","type":"episode","title":"The Time Has Come",
                "episode_metadata":{"series_id":"G4PH0WEKE","series_title":"BLUE LOCK",
                "season_number":1,"episode_number":24}},"playhead":0,"never_watched":false},
            {"panel":{"id":"GRMG8ZQZR","type":"series","title":"One Piece"}},
            {"panel":{"id":"G1","type":"movie_listing","title":"A film"}}
        ]}"#;
        let response: WatchlistResponse = serde_json::from_str(json).unwrap();
        let entries = response.data;
        assert_eq!(entries[0].series_id(), "G4PH0WEKE");
        assert_eq!(
            entries[0].panel.episode_metadata.series_title, "BLUE LOCK",
            "the series title comes along for a listing that wants it"
        );
        assert_eq!(
            entries[1].series_id(),
            "GRMG8ZQZR",
            "a panel that is a series already is the series"
        );
        assert_eq!(entries[2].series_id(), "", "a film is about no series");
    }

    #[test]
    fn reads_a_special_with_null_numbers() {
        // A special carries no episode number, and a season that has not aired yet
        // carries no title: both arrive as null rather than as a missing field.
        let json = r#"{"data":[
            {"id":"G1","episode":"SP","episode_number":null,"season_number":1,"title":null},
            {"id":"G2","episode":"2","episode_number":2,"title":"Second","duration_ms":1461000}
        ]}"#;
        let response: SeasonEpisodesResponse = serde_json::from_str(json).unwrap();
        let episodes: Vec<SeasonEpisode> = response.data;
        assert_eq!(episodes[0].episode, "SP");
        assert_eq!(episodes[0].episode_number, 0);
        assert_eq!(episodes[0].title, "");
        assert_eq!(episodes[1].duration_ms, 1_461_000);
    }
}
