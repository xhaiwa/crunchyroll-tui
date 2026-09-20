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

/// What opening one row of the catalogue column leads to.
///
/// The column holds three kinds of thing, and it holds three because Crunchyroll
/// publishes them three ways rather than because the interface went looking for the
/// variety. A series is the shape the three columns were built around. A film is
/// published as a listing with the film inside it, so the listing is what the catalogue
/// shows and the film is what plays. A concert or a music video is already the thing
/// that plays, with nothing wrapped round it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opens {
    /// A series: seasons in the middle column, and episodes inside them.
    Seasons,
    /// A film's listing: the films it holds, which is usually exactly one.
    Films,
    /// Something that is already the thing that plays, so opening it fetches nothing.
    Itself,
}

/// What a catalogue entry of this Crunchyroll type opens into, and `None` for one this
/// client has nothing to do with - an artist, a season handed back on its own, or a type
/// nobody had published when this was written.
///
/// This is the one list of what the catalogue column may hold. The watchlist and the
/// history sift their rows through it, search sifts the mixed results through it, and
/// `open` asks it what to do next, so a type added here becomes visible in all four at
/// once rather than in whichever of them somebody remembered.
pub fn opens(kind: &str) -> Option<Opens> {
    match kind {
        "series" => Some(Opens::Seasons),
        "movie_listing" => Some(Opens::Films),
        // A film that arrived as the film rather than as the listing around it, which is
        // what the mixed part of a search answer does with one.
        "movie" => Some(Opens::Itself),
        other if is_music(other) => Some(Opens::Itself),
        _ => None,
    }
}

/// Whether a type names one of Crunchyroll's music items: a concert, or a music video.
///
/// A generous guess, deliberately. Music reaches this client from the watchlist, the
/// history and the mixed part of a search answer, and there was no account and no
/// network here to ask Crunchyroll what it actually calls one: `musicConcert` is the
/// spelling its own web player uses, `music_concert` is the spelling every other type on
/// these endpoints has, and the bare `concert` turns up in older answers. All three are
/// taken, and `musicVideo` beside them - the case and the punctuation are normalised away
/// rather than spelled out one variant at a time. Guessing wrong costs a row dropped the
/// way it is dropped today rather than a row that misbehaves, which is what makes the
/// guess worth making at all.
fn is_music(kind: &str) -> bool {
    let plain: String = kind
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|letter| letter.to_ascii_lowercase())
        .collect();
    matches!(plain.as_str(), "musicconcert" | "concert" | "musicvideo")
}

/// The one word the interface calls a row that is a whole thing in itself rather than
/// one episode of a season, and `None` for an ordinary episode.
///
/// Four places would otherwise be saying `Season 0` and `E1` about something that has
/// neither: the middle column's only row, the title above it, the number slot in the
/// episodes column and the number the queue puts on a row. They all ask here, so a film
/// is called the same thing wherever it is drawn.
pub fn single_name(kind: &str) -> Option<&'static str> {
    if kind == "movie_listing" || kind == "movie" {
        Some("Film")
    } else if is_music(kind) {
        Some("Music")
    } else {
        None
    }
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
    /// What this row is, where it is not an episode.
    ///
    /// A film, a concert or a music video fills the episodes column with a row of its
    /// own, and everything downstream of that row - playing it, queueing it, the file it
    /// writes - is the episode path unchanged, which is the whole point of handing one
    /// over in this shape. This is the single thing that has to differ: what the row is
    /// called on screen, since `E1` is something to say about an episode and nothing to
    /// say about a film. Empty for an episode, which is every row the seasons endpoint
    /// ever sends.
    #[serde(default, rename = "type")]
    pub kind: String,
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

/// How far into one episode the account has got, and whether it has seen the end of it.
///
/// Crunchyroll keeps this against the account rather than against the device, which is
/// the whole reason it is worth reading: the number the web player writes when its tab
/// closes is the number this client opens on, and the number written here while mpv
/// plays is the one the phone picks up on the train. None of it is local state.
///
/// Neither `playhead` nor `fully_watched` can be counted on to be there. Entries come
/// back with a null in place of one and without the other entirely, which is the same
/// treatment every other optional field in this file already gets.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Playhead {
    #[serde(default)]
    pub content_id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub playhead: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub fully_watched: bool,
}

#[derive(Debug, Deserialize)]
pub struct PlayheadsResponse {
    #[serde(default)]
    pub data: Vec<Playhead>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Season {
    pub id: String,
    /// What this row is, where it is not a season at all.
    ///
    /// A film has no seasons endpoint behind it and the middle column still has to hold
    /// something, so it holds one row standing for the film itself. This is how that row
    /// says so - to the column drawing it, which calls it `Film` rather than `Season 0`,
    /// and to `open`, which has a different question to ask on its behalf. Empty for
    /// every season Crunchyroll sends.
    #[serde(default, rename = "type")]
    pub kind: String,
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

/// What the catalogue knows about a film before the film itself has been fetched.
///
/// The same idea as [`SeriesMetadata`] under the name a `movie_listing` carries it
/// under, and only the fields the panel below the columns has somewhere to put. The
/// names are the ones Crunchyroll's own clients read, and there was no network here to
/// check them against a live answer: a field named wrongly leaves a fact out of the
/// panel rather than leaving a film unopenable, which is why they are worth reading at
/// all on a guess.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MovieListingMetadata {
    #[serde(default, deserialize_with = "null_default")]
    pub movie_release_year: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub duration_ms: u64,
    #[serde(default)]
    pub subtitle_locales: Vec<String>,
    #[serde(default)]
    pub maturity_ratings: Vec<String>,
    #[serde(default)]
    pub is_dubbed: bool,
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
    /// The same thing for a film, which carries its facts under a name of its own and
    /// leaves `series_metadata` empty. Both are read where the panel has room for them.
    #[serde(default, deserialize_with = "null_default")]
    pub movie_listing_metadata: MovieListingMetadata,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Images,
}

impl CatalogItem {
    /// What opening this row leads to, or `None` for a row this client cannot open.
    pub fn opens(&self) -> Option<Opens> {
        opens(&self.kind)
    }
}

/// One film, as `/content/v2/cms/movie_listings/{id}/movies` hands it over.
///
/// A film is two objects on Crunchyroll's side: the listing, which is what the catalogue
/// shows and what a watchlist holds - the poster, the blurb, the title - and the film
/// inside it, which is what has a playback URL. Usually there is exactly one of the
/// latter; a feature Crunchyroll has split in half is two.
///
/// Only `id` and `title` can be counted on here. `audio_locale` and `versions` are read
/// because an episode carries both and a film may well carry them too, and everything
/// that reads them has an answer for a film that carries neither: see `api::films`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Movie {
    #[serde(default)]
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    /// The listing this film came out of, which is the name the column to the left is
    /// already showing and the name the file on disk goes under.
    #[serde(default, deserialize_with = "null_default")]
    pub movie_listing_title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_default")]
    pub duration_ms: u64,
    #[serde(default, deserialize_with = "null_default")]
    pub audio_locale: String,
    #[serde(default)]
    pub versions: Vec<DubVersion>,
    #[serde(default, deserialize_with = "null_default")]
    pub availability_starts: String,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Images,
}

#[derive(Debug, Deserialize)]
pub struct MoviesResponse {
    #[serde(default)]
    pub data: Vec<Movie>,
}

#[derive(Debug, Deserialize)]
pub struct BrowseResponse {
    #[serde(default)]
    pub data: Vec<CatalogItem>,
    /// How many series the whole listing holds. Browse is asked for series and nothing
    /// is dropped from its pages afterwards, so this counts the rows the catalogue
    /// column shows - which is what makes it worth printing in the header and worth
    /// believing about when there is nothing more to ask for. The same field on the
    /// watchlist and the history counts something else; see `api::Page`.
    #[serde(default)]
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

/// One thing the account has watched, newest first: usually an episode.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryEntry {
    /// What was played. It is the last answer rather than the first, for the entries
    /// that belong to nothing the column could show instead - see [`Self::watched_id`].
    #[serde(default, deserialize_with = "null_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub parent_id: String,
    /// What the parent is, where the entry says: `series` behind an episode. It is worth
    /// reading because it is the only way to tell a parent the catalogue column can do
    /// something with from one it cannot.
    #[serde(default, deserialize_with = "null_default")]
    pub parent_type: String,
    #[serde(default, deserialize_with = "null_default")]
    pub panel: HistoryPanel,
}

impl HistoryEntry {
    /// The id the catalogue column should show for this entry.
    ///
    /// The parent, for the ordinary case: the history is a list of episodes, the column
    /// holds series, and `parent_id` is the entry's own word for which series it came
    /// from. Where that is not filled in the panel hanging off the entry says the same
    /// thing a second time, which is why both are read.
    ///
    /// Then the entry itself, which is what a concert needs. Music is a single playable
    /// thing with nothing above it, so an entry for one names either no parent at all or,
    /// and this is a guess since there was no account here to watch a concert with, the
    /// artist behind it - which is not something this column can open. Either way the
    /// thing that was played is the row worth showing, and that is the entry's own id. An
    /// entry that says nothing about what its parent is gets the benefit of the doubt,
    /// because that is how most of them arrive.
    pub fn watched_id(&self) -> &str {
        let parent_opens = self.parent_type.is_empty() || opens(&self.parent_type).is_some();
        if !self.parent_id.is_empty() && parent_opens {
            return &self.parent_id;
        }
        let from_panel = &self.panel.episode_metadata.series_id;
        if from_panel.is_empty() {
            &self.id
        } else {
            from_panel
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

/// The words Crunchyroll has for one of its own categories or seasonal tags, in the
/// locale the list was asked for.
///
/// Only the title is read. The description beside it is a paragraph written for the
/// website's own genre pages, and a row of a list one keypress deep has no room for one.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Localization {
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
}

/// One of the categories the catalogue can be narrowed to.
///
/// `tenant_category` is the slug browse takes - `action`, `slice-of-life` - and it is
/// the field to read rather than the `slug` beside it, which is the same word for the
/// categories that have both and absent for the ones that do not. The sub-categories
/// hanging off each row are left alone: they are slugs of the same shape one level
/// down, and a flat list of twenty genres is something to read at a glance where a tree
/// is something to navigate.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Category {
    #[serde(default, deserialize_with = "null_default", rename = "tenant_category")]
    pub slug: String,
    #[serde(default, deserialize_with = "null_default")]
    pub localization: Localization,
}

/// One anime season, as browse wants it named: an id of the `fall-2024` shape, with the
/// words for it beside it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeasonalTag {
    #[serde(default, deserialize_with = "null_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub localization: Localization,
}

#[derive(Debug, Deserialize)]
pub struct CategoriesResponse {
    #[serde(default)]
    pub data: Vec<Category>,
}

#[derive(Debug, Deserialize)]
pub struct SeasonalTagsResponse {
    #[serde(default)]
    pub data: Vec<SeasonalTag>,
}

#[cfg(test)]
mod tests {
    use super::{
        CategoriesResponse, Episode, HistoryResponse, Opens, PlayheadsResponse, SeasonEpisode,
        SeasonEpisodesResponse, SeasonalTagsResponse, WatchlistResponse, opens, single_name,
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
        assert_eq!(entry.watched_id(), "GY8VEQ95Y");
        assert_eq!(entry.panel.episode_metadata.series_title, "Frieren");
        assert_eq!(entry.panel.episode_metadata.episode_number, 4);
    }

    /// `parent_id` is the entry's own word for which series it came from, and it is not
    /// always there. The panel says the same thing a second time, so an entry without one
    /// is still worth keeping - and an entry that belongs to nothing the column can open
    /// stands for itself, which is how a concert stays in Continue watching instead of
    /// being dropped for having no series behind it.
    #[test]
    fn finds_the_row_a_history_entry_stands_for() {
        let json = r#"{"data":[
            {"parent_id":"","panel":{"episode_metadata":{"series_id":"GY8VEQ95Y"}}},
            {"panel":{"episode_metadata":{"series_id":"GRMG8ZQZR"}}},
            {"parent_id":"GY5P48XEY","panel":{"episode_metadata":{"series_id":"G0LDEN"}}},
            {"parent_id":"GARTIST1","parent_type":"artist","id":"GCONCERT"},
            {"panel":{"episode_metadata":{"series_id":null}}},
            {"id":"GZ7UV8KWZ"}
        ]}"#;
        let entries = serde_json::from_str::<HistoryResponse>(json).unwrap().data;
        assert_eq!(entries[0].watched_id(), "GY8VEQ95Y");
        assert_eq!(entries[1].watched_id(), "GRMG8ZQZR");
        assert_eq!(
            entries[2].watched_id(),
            "GY5P48XEY",
            "the entry's own parent beats the panel's copy of it"
        );
        assert_eq!(
            entries[3].watched_id(),
            "GCONCERT",
            "an artist is not a row this column can open, so the concert stands for itself"
        );
        assert_eq!(
            entries[4].watched_id(),
            "",
            "an entry that names nothing at all names nothing at all"
        );
        assert_eq!(entries[5].watched_id(), "GZ7UV8KWZ");
    }

    /// The one list of what the catalogue column may hold: the watchlist, the history and
    /// a search all sift their rows through it, and `open` asks it what to do next. A
    /// spelling missed here is a film or a concert quietly dropped from four lists at
    /// once, which is why the music types are read generously - nothing in this session
    /// could ask Crunchyroll which of the three spellings it actually sends.
    #[test]
    fn says_what_each_kind_of_row_opens_into() {
        assert_eq!(opens("series"), Some(Opens::Seasons));
        assert_eq!(opens("movie_listing"), Some(Opens::Films));
        assert_eq!(opens("movie"), Some(Opens::Itself));
        for music in ["musicConcert", "music_concert", "concert", "musicVideo"] {
            assert_eq!(opens(music), Some(Opens::Itself), "{music}");
            assert_eq!(single_name(music), Some("Music"), "{music}");
        }
        // A season or an episode handed back on its own belongs inside a row rather than
        // being one, an artist is a page this client has nothing to draw, and a type
        // nobody had published when this was written is the same problem as an artist.
        for other in ["season", "episode", "artist", "musicArtist", "", "whatever"] {
            assert_eq!(opens(other), None, "{other}");
            assert_eq!(single_name(other), None, "{other}");
        }
        assert_eq!(single_name("series"), None);
        for film in ["movie_listing", "movie"] {
            assert_eq!(single_name(film), Some("Film"), "{film}");
        }
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
            assert_eq!(entries[0].watched_id(), "GY8VEQ95Y", "{json}");
            assert_eq!(entries[0].panel.episode_metadata.series_title, "", "{json}");
        }
    }

    /// The answer decides whether a row is marked and where mpv opens, so one odd entry
    /// in a season must not cost the other twenty-five. An episode nobody has opened
    /// comes back without a position at all, and `fully_watched` is missing about as
    /// often as it is null.
    #[test]
    fn reads_a_playhead_however_little_of_it_is_there() {
        let json = r#"{"data":[
            {"content_id":"GZ7UV8KWZ","playhead":842,"fully_watched":false,
             "last_modified":"2026-05-01T10:11:12Z"},
            {"content_id":"G2","playhead":1400,"fully_watched":true},
            {"content_id":"G3","playhead":12,"fully_watched":null},
            {"content_id":"G4"}
        ]}"#;
        let playheads = serde_json::from_str::<PlayheadsResponse>(json)
            .unwrap()
            .data;
        assert_eq!(playheads[0].content_id, "GZ7UV8KWZ");
        assert_eq!(playheads[0].playhead, 842);
        assert!(!playheads[0].fully_watched);
        assert!(playheads[1].fully_watched);
        assert!(!playheads[2].fully_watched, "a null is not a yes");
        assert_eq!(
            playheads[3].playhead, 0,
            "an episode with no position is one at the start of it"
        );
    }

    /// The two lists the catalogue filters are chosen from, as Crunchyroll sends them.
    /// A category is asked for by the slug in `tenant_category` and a season by its id,
    /// and both are shown by the title under `localization` - so a row that arrives
    /// without one of those two is a row that cannot be offered, which is why the empty
    /// cases are pinned here rather than discovered in front of a user.
    #[test]
    fn reads_the_categories_and_the_seasons_on_offer() {
        let json = r#"{"total":2,"data":[
            {"tenant_category":"action","slug":"action","sub_categories":[],
             "localization":{"title":"Action","description":"Punching.","locale":"en-US"}},
            {"tenant_category":"slice-of-life",
             "localization":{"title":"Slice of Life","description":null,"locale":"en-US"}},
            {"tenant_category":"seinen","localization":null},
            {"localization":{"title":"Nothing browse can be asked for"}}
        ]}"#;
        let categories = serde_json::from_str::<CategoriesResponse>(json)
            .expect("a list of categories")
            .data;
        assert_eq!(categories[0].slug, "action");
        assert_eq!(categories[0].localization.title, "Action");
        assert_eq!(categories[1].slug, "slice-of-life");
        assert_eq!(categories[1].localization.title, "Slice of Life");
        assert_eq!(categories[2].localization.title, "", "a null localisation");
        assert_eq!(categories[3].slug, "", "and a row with no slug at all");

        let json = r#"{"total":2,"data":[
            {"id":"fall-2024","localization":{"title":"Fall 2024","locale":"en-US"}},
            {"id":"summer-2024","localization":{"title":null}}
        ]}"#;
        let seasons = serde_json::from_str::<SeasonalTagsResponse>(json)
            .expect("a list of seasonal tags")
            .data;
        assert_eq!(seasons[0].id, "fall-2024");
        assert_eq!(seasons[0].localization.title, "Fall 2024");
        assert_eq!(seasons[1].id, "summer-2024");
        assert_eq!(seasons[1].localization.title, "");
    }
}
