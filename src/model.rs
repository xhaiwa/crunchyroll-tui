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
    /// Which series the episode is an episode of. The history is what wants it: its
    /// entries are episodes, the catalogue column holds series, and this is where the
    /// series hides on an entry whose `parent_id` was left out.
    #[serde(default, deserialize_with = "null_default")]
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

#[derive(Debug, Deserialize)]
pub struct BrowseResponse {
    #[serde(default)]
    pub data: Vec<CatalogItem>,
    #[serde(default)]
    #[allow(dead_code)]
    pub total: i64,
}

/// One row of the account's watchlist.
///
/// Crunchyroll answers this endpoint in two shapes and both turn up in the wild. Either
/// the row wraps the series in a `panel`, with the bookkeeping the watchlist keeps about
/// it - whether it is new, whether it is a favourite - sitting outside the wrapper, or
/// the row simply *is* the series, with that same bookkeeping beside its title. Taking
/// the panel where there is one and the row itself where there is not covers both,
/// without having to know in advance which shape an account will be sent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WatchlistEntry {
    #[serde(default)]
    pub panel: Option<CatalogItem>,
    /// The other shape: the series' own fields, straight on the row. Harmless where a
    /// panel is present, since everything it would collect is inside the wrapper and the
    /// row keeps only its own id.
    #[serde(flatten)]
    pub item: CatalogItem,
}

impl WatchlistEntry {
    /// The series the row stands for, whichever of the two shapes it arrived in.
    pub fn into_item(self) -> CatalogItem {
        self.panel.unwrap_or(self.item)
    }
}

/// The panel an entry of the history carries: the episode as the catalogue would have
/// shown it, with the metadata that says which series it came from.
///
/// Only the metadata is read. The panel describes an episode, and the column the history
/// ends up in holds series, so the episode itself is of no use here beyond what it says
/// about its parent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryPanel {
    #[serde(default, deserialize_with = "null_default")]
    pub episode_metadata: EpisodeMetadata,
}

/// One episode the account has watched, newest first.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryEntry {
    #[serde(default, deserialize_with = "null_default")]
    pub parent_id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub panel: HistoryPanel,
}

impl HistoryEntry {
    /// The series this episode belongs to.
    ///
    /// `parent_id` is the entry's own answer and the one to trust, but it is not always
    /// filled in, and the panel hanging off the entry says the same thing a second time.
    /// An entry with neither knows of no series at all, which the caller reads as an
    /// empty string and drops.
    pub fn series_id(&self) -> &str {
        if self.parent_id.is_empty() {
            &self.panel.episode_metadata.series_id
        } else {
            &self.parent_id
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WatchlistResponse {
    #[serde(default)]
    pub data: Vec<WatchlistEntry>,
    #[serde(default)]
    #[allow(dead_code)]
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct HistoryResponse {
    #[serde(default)]
    pub data: Vec<HistoryEntry>,
    #[serde(default)]
    #[allow(dead_code)]
    pub total: i64,
}

/// What `objects` answers with: the catalogue entries for the ids it was given, in
/// whatever order it felt like putting them in.
#[derive(Debug, Deserialize)]
pub struct ObjectsResponse {
    #[serde(default)]
    pub data: Vec<CatalogItem>,
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
    use super::{
        Episode, HistoryResponse, SeasonEpisode, SeasonEpisodesResponse, WatchlistResponse,
    };

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

    /// The watchlist arrives in one of two shapes depending on the account, and neither
    /// of them is worth guessing wrong: a row read the wrong way round is a column of
    /// blank titles that nothing can be opened from.
    #[test]
    fn reads_a_watchlist_row_in_either_shape() {
        // Wrapped: the watchlist's own bookkeeping outside, the series in `panel`.
        let wrapped = r#"{"total":1,"data":[
            {"id":"GY8VEQ95Y","new":false,"is_favorite":false,
             "panel":{"id":"GY8VEQ95Y","type":"series","title":"Frieren",
                      "description":"An elf outlives her party.",
                      "images":{"poster_tall":[[{"width":240,"source":"a.jpg"}]]},
                      "series_metadata":{"season_count":1,"is_dubbed":true}}}
        ]}"#;
        // Flat: the row is the series, with the same bookkeeping beside its title.
        let flat = r#"{"total":1,"data":[
            {"id":"GY8VEQ95Y","new":false,"is_favorite":false,"type":"series",
             "title":"Frieren","description":"An elf outlives her party.",
             "images":{"poster_tall":[[{"width":240,"source":"a.jpg"}]]},
             "series_metadata":{"season_count":1,"is_dubbed":true}}
        ]}"#;
        for json in [wrapped, flat] {
            let response: WatchlistResponse = serde_json::from_str(json).unwrap();
            let item = response.data.into_iter().next().unwrap().into_item();
            assert_eq!(item.id, "GY8VEQ95Y", "{json}");
            assert_eq!(item.kind, "series", "{json}");
            assert_eq!(item.title, "Frieren", "{json}");
            assert_eq!(item.description, "An elf outlives her party.", "{json}");
            assert_eq!(item.series_metadata.season_count, 1, "{json}");
            assert!(item.series_metadata.is_dubbed, "{json}");
            assert_eq!(item.images.poster(240), Some("a.jpg"), "{json}");
        }
    }

    /// A series with no poster yet is sent either as `"images": null` or with the field
    /// left out, and in both shapes of row. None of the four is a reason to throw the
    /// whole watchlist away - the column draws a placeholder and carries on.
    #[test]
    fn takes_a_watchlist_row_with_no_artwork() {
        for json in [
            r#"{"data":[{"id":"G1","panel":{"id":"G1","type":"series","images":null}}]}"#,
            r#"{"data":[{"id":"G1","panel":{"id":"G1","type":"series"}}]}"#,
            r#"{"data":[{"id":"G1","type":"series","images":null}]}"#,
            r#"{"data":[{"id":"G1","type":"series"}]}"#,
        ] {
            let response: WatchlistResponse = serde_json::from_str(json).unwrap();
            let item = response.data.into_iter().next().unwrap().into_item();
            assert_eq!(item.id, "G1", "{json}");
            assert_eq!(item.images.poster(240), None, "{json}");
        }
    }

    /// The history as Crunchyroll actually sends it. It is a list of episodes, and the
    /// only thing read off each one is which series it belongs to, so that is what this
    /// pins down: the whole shape goes in, and the series comes out.
    #[test]
    fn reads_the_history_an_episode_at_a_time() {
        let json = r#"{"total":120,"data":[
            {"id":"GZ7UV8KWZ","playhead":842,"fully_watched":false,
             "date_played":"2026-05-01T10:11:12Z",
             "parent_id":"GY8VEQ95Y","parent_type":"series",
             "panel":{"id":"GZ7UV8KWZ","type":"episode","title":"The Land Where Souls Rest",
                      "episode_metadata":{"series_id":"GY8VEQ95Y","series_title":"Frieren",
                                          "season_number":1,"episode_number":4}}}
        ]}"#;
        let response: HistoryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.total, 120);
        let entry = &response.data[0];
        assert_eq!(entry.series_id(), "GY8VEQ95Y");
        assert_eq!(entry.panel.episode_metadata.series_title, "Frieren");
        assert_eq!(entry.panel.episode_metadata.episode_number, 4);
    }

    /// `parent_id` is the entry's own word for which series it came from, and it is not
    /// always there. The panel says the same thing a second time, so an entry without one
    /// is still worth keeping - and an entry with neither names no series at all, which
    /// the caller has to be able to tell apart from one that does.
    #[test]
    fn finds_the_series_wherever_the_entry_keeps_it() {
        let json = r#"{"data":[
            {"parent_id":"","panel":{"episode_metadata":{"series_id":"GY8VEQ95Y"}}},
            {"panel":{"episode_metadata":{"series_id":"GRMG8ZQZR"}}},
            {"parent_id":"GY5P48XEY","panel":{"episode_metadata":{"series_id":"G0LDEN"}}},
            {"panel":{"episode_metadata":{"series_id":null}}},
            {"id":"GZ7UV8KWZ"}
        ]}"#;
        let entries = serde_json::from_str::<HistoryResponse>(json).unwrap().data;
        assert_eq!(entries[0].series_id(), "GY8VEQ95Y");
        assert_eq!(entries[1].series_id(), "GRMG8ZQZR");
        assert_eq!(
            entries[2].series_id(),
            "GY5P48XEY",
            "the entry's own parent beats the panel's copy of it"
        );
        assert_eq!(entries[3].series_id(), "");
        assert_eq!(entries[4].series_id(), "");
    }

    /// An entry whose panel Crunchyroll has nothing to say about - a deleted episode, or
    /// one the account may no longer see - arrives with the panel or its metadata null
    /// rather than missing. Neither is a broken page of history: it is one entry that
    /// cannot be turned into a series, and the rest of the page still can.
    #[test]
    fn takes_an_entry_with_no_panel() {
        for json in [
            r#"{"data":[{"parent_id":"GY8VEQ95Y","panel":null}]}"#,
            r#"{"data":[{"parent_id":"GY8VEQ95Y","panel":{"episode_metadata":null}}]}"#,
            r#"{"data":[{"parent_id":"GY8VEQ95Y","panel":{}}]}"#,
        ] {
            let entries = serde_json::from_str::<HistoryResponse>(json).unwrap().data;
            assert_eq!(entries[0].series_id(), "GY8VEQ95Y", "{json}");
            assert_eq!(entries[0].panel.episode_metadata.series_title, "", "{json}");
        }
    }
}
