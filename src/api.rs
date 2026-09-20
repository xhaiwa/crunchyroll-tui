use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::credentials::Secret;
use crate::model::{
    BrowseResponse, CatalogItem, CategoriesResponse, Category, Episode, EpisodeInfo,
    EpisodeMetadataResponse, HistoryEntry, HistoryResponse, Movie, MoviesResponse, ObjectsResponse,
    Playhead, PlayheadsResponse, SearchGroup, SearchResponse, Season, SeasonEpisode,
    SeasonEpisodesResponse, SeasonalTag, SeasonalTagsResponse, SeasonsResponse, WatchlistEntry,
    WatchlistResponse,
};

const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";

/// How long a TCP connection and TLS handshake may take before the host counts as
/// unreachable. `reqwest` leaves this unset, so the connect phase is otherwise bounded
/// only by whatever the operating system does about a SYN nobody answers.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The whole budget for an API call, headers and body together. Every one of them
/// carries a small JSON document or a manifest, so anything this side of it is a
/// connection that has stopped moving rather than a slow one.
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one read of a media body may go without a byte arriving.
///
/// `reqwest` applies a client's timeout to each read of a body rather than to the
/// response as a whole, which is the shape media downloads need. One on-demand track is
/// a single response covering twenty minutes of video, drained at the speed the consumer
/// wants it, so a deadline for the whole thing would cut a healthy stream off
/// mid-episode; a segment on a slow line has the same problem in miniature. Per read it
/// says the one thing worth saying instead - nothing is arriving any more - and a CDN
/// connection that has gone quiet is noticed in half a minute rather than parking a
/// worker on it until the process is killed.
///
/// This only holds for a body read by hand. The convenience readers (`bytes`, `json`)
/// take it as a deadline for the entire body, so media bodies wanted in one piece are
/// read through `download::read_body` rather than through those.
const MEDIA_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How many content ids one playheads request carries.
///
/// The ids are asked for in the query string, and a season of a long-running series is
/// hundreds of them - more URL than any server promises to read. A hundred keeps a
/// request well inside what is safe everywhere and still asks about the season anybody
/// is actually looking at in a single round trip.
const PLAYHEAD_CHUNK: usize = 100;

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Which account the token was issued for. Every endpoint that knows anything about
    /// the person logged in - the watchlist, the history, the playheads - is addressed
    /// by it rather than by the token alone.
    #[serde(default)]
    account_id: String,
}

#[derive(Clone)]
pub struct CrunchyrollClient {
    http: Client,
    media: Client,
    device_id: String,
    etp_rt: Secret,
    access_token: Arc<RwLock<String>>,
    /// Refreshed alongside the token, because it comes with it and because a token
    /// re-issued for another account would otherwise leave this one pointing at the
    /// wrong watchlist.
    account_id: Arc<RwLock<String>>,
    refresh_lock: Arc<Mutex<()>>,
    /// Where the running commentary goes. It is printed by default, but the TUI owns
    /// the terminal and needs to collect it instead of having it drawn over the frame.
    notice: Arc<dyn Fn(&str) + Send + Sync>,
    pub debug: bool,
}

/// The client that talks to the API: one budget for the whole call, because every
/// response it reads is small enough to arrive well inside it.
fn build_api_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(API_TIMEOUT)
        .build()
        .context("build HTTP client")
}

/// The client that talks to the CDN, pinned to HTTP/1.1 and given `stall` as the time
/// one read of a body may go without a byte.
///
/// Taken as a parameter so a test can watch a trickle of bytes against a timeout it
/// does not have to wait half a minute for.
fn build_media_client(stall: Duration) -> Result<Client> {
    Client::builder()
        .http1_only()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(stall)
        .build()
        .context("build media HTTP client")
}

/// The account named in an access token's claims.
///
/// The token is a JWT: three base64url segments separated by dots, the middle one a JSON
/// object. Nothing here verifies the signature, and nothing should - the token was just
/// handed over by the server that signed it, over TLS, and is about to be handed
/// straight back. This only reads a value out of something already trusted, so a token
/// in any shape other than the expected one is `None` rather than an error.
fn account_id_from_jwt(token: &str) -> Option<String> {
    use base64::Engine;

    let claims = token.split('.').nth(1)?;
    // JWT segments are base64url with the padding stripped, but a `=` or two on the end
    // is common enough in the wild that it costs nothing to accept them.
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    // `account_id` where the token carries one, and `sub` otherwise: the subject of a
    // token issued against an etp_rt cookie is the account it was issued for.
    ["account_id", "sub"]
        .into_iter()
        .filter_map(|claim| claims.get(claim)?.as_str())
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The rows of a watchlist the catalogue column can open.
///
/// The watchlist holds whatever the account put on it - series, films, the odd concert -
/// and it takes no `type` filter the way browse and search do, so the sifting happens
/// here. It used to keep the series and drop the rest, because a film had nowhere to go:
/// there is no seasons endpoint behind one and the middle column had nothing to show. It
/// has now, so the only rows left to drop are the ones nothing in this client knows how
/// to open, which [`crate::model::opens`] is the list of.
///
/// Split out from the request so the part that needs neither an account nor a network can
/// be tested against the two shapes the rows arrive in.
fn watchlist_items(entries: Vec<WatchlistEntry>) -> Vec<CatalogItem> {
    entries
        .into_iter()
        .map(WatchlistEntry::into_item)
        .filter(|item| item.opens().is_some())
        .collect()
}

/// The same sifting for a search answer, which needs it for a reason of its own: search
/// answers with one group per type asked for, and adds a `top_results` group that mixes
/// in whatever it likes regardless of what was asked. A music video turning up there is
/// kept now rather than dropped - it is a single playable thing, which is exactly what
/// this client can do something with - and an artist or a season still goes.
fn searched(groups: Vec<SearchGroup>) -> Vec<CatalogItem> {
    groups
        .into_iter()
        .flat_map(|group| group.items)
        .filter(|item| item.opens().is_some())
        .collect()
}

/// The films of a movie listing, as the rows the rest of the program already handles.
///
/// Everything below the episodes column - playing, the queue, the name of the file that
/// gets written - takes a [`SeasonEpisode`], and a film is one thing that plays, so it is
/// handed over as one rather than given a path of its own. Four fields have no answer of
/// their own for a film and are decided here.
///
/// The series title is the listing's, so a film lands in a directory named after itself
/// and is named there again with its own title beside the numbers, as in the file
/// `Suzume/Suzume S01E01 - Suzume [1080p].mkv`. Saying it twice is the price of
/// `output_path` staying the one function that names every file this program writes, and
/// a special case there would have to be paid for by every episode.
///
/// The season is 1 and the films are numbered from 1 in the order the listing gives them,
/// which for a feature split in half is the order they are meant to be watched in. Zero
/// would be the honest answer, since a film has no season and no episode, but it is honest
/// only inside the struct: it reaches the disk as `S00E00` in the middle of that file
/// name, where nothing can dress it up, and reaches the downloader's own commentary as
/// episode 0. Reading the numbers as what they are here says something true instead -
/// this is the first and only part of the thing named in front of them - and what the
/// screen calls the row is settled by `model::single_name` rather than by these two.
///
/// The audio locale is the film's own where it names one and the one that was asked for
/// where it does not. `download_episode` maps the requested locale onto the film's own id
/// when there are no dub versions to choose between, and an empty locale there means
/// "none of the requested audio locales are available" for every film there is. Whether
/// `/movies` names a locale at all could not be checked from here, so this is the
/// defensive reading of both answers.
fn films(movies: Vec<Movie>, asked_audio: &str) -> Vec<SeasonEpisode> {
    movies
        .into_iter()
        .enumerate()
        .map(|(index, movie)| SeasonEpisode {
            id: movie.id,
            // What the column, the queue and the panel call the row. The films endpoint
            // is asked about films and answers with films, so nothing else can say it.
            kind: "movie".to_owned(),
            season_number: 1,
            episode_number: i32::try_from(index + 1).unwrap_or(i32::MAX),
            series_title: if movie.movie_listing_title.is_empty() {
                movie.title.clone()
            } else {
                movie.movie_listing_title
            },
            audio_locale: if movie.audio_locale.is_empty() {
                asked_audio.to_owned()
            } else {
                movie.audio_locale
            },
            versions: movie.versions,
            title: movie.title,
            description: movie.description,
            duration_ms: movie.duration_ms,
            availability_starts: movie.availability_starts,
            images: movie.images,
            ..SeasonEpisode::default()
        })
        .collect()
}

/// The types browse and search are asked for.
///
/// Series, and the listings films are published under. Music is left out of the question
/// rather than guessed at: nothing in this session could ask Crunchyroll what `type`
/// value browse and search want for a concert or a music video, and a type they do not
/// recognise is a 400 for the whole page rather than a page with no music in it.
/// Music that arrives by any other road is kept and is playable: on the watchlist, in the
/// history, and in the mixed `top_results` a search brings along whatever it was asked
/// for. See [`crate::model::opens`].
const CATALOG_TYPES: &str = "series,movie_listing";

/// How many ids one `objects` request may name. The endpoint takes them as a
/// comma-separated path segment, so a whole page of history in one request would be a URL
/// some proxy between here and Crunchyroll is entitled to refuse; fifty is what the web
/// player asks for and is comfortably inside anything that counts.
const OBJECTS_PER_REQUEST: usize = 50;

/// What a page of history stands for, newest first and each one named once.
///
/// Mostly the series behind the episodes: the history is a list of things played and the
/// catalogue column holds what they belong to, so several entries in a row are usually
/// the same series being worked through. Keeping the first occurrence rather than the
/// last is the whole point: the first is the most recently watched, and what the column
/// is for is saying what was being watched last. An entry that belongs to nothing - a
/// concert - stands for itself instead; [`HistoryEntry::watched_id`] is where that is
/// decided.
///
/// Split out from the request because everything interesting about it - the order, and
/// what happens to an entry that names nothing at all - is worth pinning down without a
/// network behind it.
fn watched_ids(entries: &[HistoryEntry]) -> Vec<String> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .map(HistoryEntry::watched_id)
        .filter(|watched| !watched.is_empty() && seen.insert(*watched))
        .map(str::to_owned)
        .collect()
}

/// The ids of one `objects` request each, comma-joined and ready to go into the path.
fn object_batches(ids: &[String]) -> Vec<String> {
    ids.chunks(OBJECTS_PER_REQUEST)
        .map(|batch| batch.join(","))
        .collect()
}

/// The catalogue entries put back into the order the ids were asked in.
///
/// `objects` promises nothing about the order it answers in, and for a list whose whole
/// meaning is its order that is not something to take on trust. An id the endpoint said
/// nothing about - a series that has been withdrawn, or one this account may no longer
/// see - simply is not in the result, which is better than a hole in the column.
fn in_asked_order(ids: &[String], items: Vec<CatalogItem>) -> Vec<CatalogItem> {
    let mut found: HashMap<String, CatalogItem> = items
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect();
    ids.iter().filter_map(|id| found.remove(id)).collect()
}

/// The requests one playheads lookup turns into: one per chunk of ids.
///
/// The list goes in through `query_pairs_mut` rather than being pasted into the URL by
/// hand, because a comma written raw is the URL saying something about its own shape
/// rather than the value saying something about its ids. Escaped, it arrives as the one
/// separator the endpoint is looking for.
///
/// Split out from the request so the chunking can be checked without an account and
/// without a network.
fn playhead_urls(account_id: &str, content_ids: &[String]) -> Vec<String> {
    content_ids
        .chunks(PLAYHEAD_CHUNK)
        .map(|chunk| {
            let mut url = reqwest::Url::parse(&format!(
                "https://www.crunchyroll.com/content/v2/{account_id}/playheads"
            ))
            .expect("valid playheads URL");
            url.query_pairs_mut()
                .append_pair("locale", "en-US")
                .append_pair("content_ids", &chunk.join(","));
            url.into()
        })
        .collect()
}

/// One page of a catalogue listing: the series it brought back, how long the whole list
/// is where that is known, and where to carry on from.
///
/// The catalogue column asks for a hundred series at a time and appends what comes back,
/// so every listing has to be able to say two things about itself that a bare list of
/// items cannot. `next` is the offset the column asks for when the cursor reaches the
/// bottom, and `None` is the end of the list - it is worked out here, beside the request,
/// because only this side knows how many rows came off the wire before the sifting each
/// endpoint does below.
///
/// Cloneable so that a test can hand the same page to a filter twice and compare what
/// each did with it; nothing on the way to the interface copies one.
#[derive(Clone)]
pub struct Page {
    pub items: Vec<CatalogItem>,
    /// How many entries the list holds in all, or `None` where nothing here counts the
    /// same things the column shows. A total is for the header to print against what is
    /// loaded, and a number the column can never reach would be worse than no number:
    /// `100 of 1203 series` has to mean there are 1103 more of them to walk to.
    pub total: Option<usize>,
    /// Where the next page starts, or `None` when this one was the last.
    pub next: Option<usize>,
}

/// Where the next page of a listing begins, or `None` when the one in hand was the end
/// of it.
///
/// Two separate things finish a list, and both are here because neither is reliable
/// alone. A page that came back shorter than it was asked for is the endpoint saying it
/// had nothing else to fill it with, which is the only answer a list with no published
/// total ever gives; and a page that reaches the total is the end by arithmetic, which
/// catches an endpoint that would rather hand back an empty page than stop.
///
/// The rows counted are the ones that came off the wire, not the ones that survived the
/// sifting each caller does afterwards: an artist dropped out of a watchlist page, or a
/// bare episode out of a search, still holds its place in the list the offsets count
/// through, and counting what was kept would ask for the same rows over again a page
/// later.
fn next_page(start: usize, asked: usize, returned: usize, total: Option<usize>) -> Option<usize> {
    let next = start + returned;
    let ended = returned < asked || total.is_some_and(|total| next >= total);
    (!ended).then_some(next)
}

/// The browse request, as a URL.
///
/// `categories` is a slug the categories endpoint gave out and `seasonal_tag` an id the
/// seasonal-tags endpoint did, and each is left out of the URL entirely when nothing is
/// chosen rather than sent empty: `categories=` is a question about the category whose
/// slug is the empty string, which no series has, and the answer to it is an empty
/// catalogue.
///
/// Split out from the request for the same reason `playhead_urls` is - so that what
/// actually goes over the wire can be read back without an account and without a
/// network.
fn browse_url(
    sort_by: &str,
    count: usize,
    start: usize,
    categories: Option<&str>,
    seasonal_tag: Option<&str>,
) -> String {
    let mut url = reqwest::Url::parse("https://www.crunchyroll.com/content/v2/discover/browse")
        .expect("valid browse URL");
    let mut query = url.query_pairs_mut();
    query
        .append_pair("sort_by", sort_by)
        .append_pair("type", CATALOG_TYPES)
        .append_pair("n", &count.to_string())
        .append_pair("start", &start.to_string())
        .append_pair("ratings", "true")
        .append_pair("locale", "en-US");
    if let Some(categories) = categories.filter(|slug| !slug.is_empty()) {
        query.append_pair("categories", categories);
    }
    if let Some(seasonal_tag) = seasonal_tag.filter(|id| !id.is_empty()) {
        query.append_pair("seasonal_tag", seasonal_tag);
    }
    // The serializer holds the URL borrowed and writes the query as it goes, so it has
    // to be let go of before the URL can be read back.
    drop(query);
    url.into()
}

impl CrunchyrollClient {
    pub fn new(etp_rt: Secret, debug: bool) -> Result<Self> {
        let client = Self {
            http: build_api_client()?,
            media: build_media_client(MEDIA_STALL_TIMEOUT)?,
            device_id: Uuid::new_v4().to_string(),
            etp_rt,
            access_token: Arc::new(RwLock::new(String::new())),
            account_id: Arc::new(RwLock::new(String::new())),
            refresh_lock: Arc::new(Mutex::new(())),
            notice: Arc::new(|message| println!("{message}")),
            debug,
        };
        client.refresh_access_token()?;
        Ok(client)
    }

    /// Sends everything this client would have printed to `notice` instead.
    pub fn with_notices(mut self, notice: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        self.notice = notice;
        self
    }

    /// Says something wherever this client's own commentary goes: printed, or collected
    /// for the status line. What goes wrong reporting a playback position is news of
    /// exactly that kind, and the thread it goes wrong on has nowhere else to put it.
    pub fn notice(&self, message: &str) {
        (self.notice)(message);
    }

    fn refresh_access_token(&self) -> Result<()> {
        let _guard = self.refresh_lock.lock().expect("refresh mutex poisoned");
        let response = self
            .http
            .post("https://www.crunchyroll.com/auth/v1/token")
            .header(AUTHORIZATION, "Basic bm9haWhkZXZtXzZpeWcwYThsMHE6")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(
                COOKIE,
                format!(
                    "device_id={}; etp_rt={}",
                    self.device_id,
                    self.etp_rt.expose()
                ),
            )
            .form(&[
                ("device_id", self.device_id.as_str()),
                ("device_type", "Firefox on Linux"),
                ("grant_type", "etp_rt_cookie"),
            ])
            .send()
            .context("request Crunchyroll access token")?;
        if !response.status().is_success() {
            // Cloudflare sits in front of this endpoint and answers anything it takes
            // for a bot with a challenge page, which otherwise looks just like a
            // rejected cookie.
            if response.headers().contains_key("cf-mitigated") {
                bail!(
                    "Cloudflare challenged the request ({}) before Crunchyroll saw it, so the etp_rt cookie was never checked.",
                    response.status()
                );
            }
            bail!(
                "Crunchyroll rejected the etp_rt cookie ({}). Copy a fresh one from a logged-in session.",
                response.status()
            );
        }
        let token: TokenResponse = response.json().context("decode access token response")?;
        if token.access_token.is_empty() {
            bail!("Crunchyroll returned an empty access token");
        }
        // The field is not always there - the shape of this response has changed before
        // and may again - but the token itself is a JWT that names the account in its
        // claims, so there is a second place to look before giving up on it.
        let account_id = if token.account_id.is_empty() {
            account_id_from_jwt(&token.access_token).unwrap_or_default()
        } else {
            token.account_id
        };
        *self.access_token.write().expect("token lock poisoned") = token.access_token;
        *self.account_id.write().expect("account lock poisoned") = account_id;
        Ok(())
    }

    /// Which account this client is logged in as.
    ///
    /// An error rather than an empty string: the endpoints that need it put it in the
    /// path, and one built around an empty id asks about an account that does not exist
    /// and comes back with a 404 that says nothing about why.
    pub fn account_id(&self) -> Result<String> {
        let account_id = self
            .account_id
            .read()
            .expect("account lock poisoned")
            .clone();
        if account_id.is_empty() {
            bail!(
                "Crunchyroll did not say which account this token belongs to, so the watchlist, the history and the playheads cannot be asked for."
            );
        }
        Ok(account_id)
    }

    fn send_authed(
        &self,
        method: Method,
        url: &str,
        headers: &HeaderMap,
        body: Option<&[u8]>,
    ) -> Result<Response> {
        for attempt in 0..2 {
            let token = self
                .access_token
                .read()
                .expect("token lock poisoned")
                .clone();
            let mut request = self
                .http
                .request(method.clone(), url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header(USER_AGENT, USER_AGENT_VALUE)
                .headers(headers.clone());
            if let Some(body) = body {
                request = request.body(body.to_vec());
            }
            let response = request
                .send()
                .with_context(|| format!("request {method} {url}"))?;
            if response.status() != reqwest::StatusCode::UNAUTHORIZED || attempt == 1 {
                return Ok(response);
            }
            (self.notice)("Access token expired. Refetching one...");
            self.refresh_access_token()?;
        }
        unreachable!()
    }

    /// A request carrying a small JSON document, which is the shape every endpoint that
    /// changes something about the account takes. The answer is handed back unchecked:
    /// what counts as success differs between them - a 200 here, a 204 there - and each
    /// caller can say what went wrong in its own words.
    fn send_json(&self, method: Method, url: &str, body: &serde_json::Value) -> Result<Response> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let body = serde_json::to_vec(body).context("encode the request body")?;
        self.send_authed(method, url, &headers, Some(&body))
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        self.send_authed(Method::GET, url, &HeaderMap::new(), None)?
            .error_for_status()
            .with_context(|| format!("Crunchyroll request failed: {url}"))?
            .json()
            .with_context(|| format!("decode Crunchyroll response: {url}"))
    }

    pub fn episode(&self, id: &str) -> Result<Episode> {
        let url = format!("https://www.crunchyroll.com/playback/v3/{id}/web/firefox/play");
        let response = self
            .send_authed(Method::GET, &url, &HeaderMap::new(), None)?
            .error_for_status()
            .context("request episode playback")?;
        let body = response.bytes().context("read episode playback response")?;
        if self.debug {
            println!("\n{}\n", String::from_utf8_lossy(&body));
        }
        let episode: Episode = serde_json::from_slice(&body).context("decode episode playback")?;
        if !episode.error.is_empty() {
            if episode.reason.is_empty() {
                eprintln!("Error: {}", episode.error);
            } else {
                eprintln!("Error: {} ({})", episode.error, episode.reason);
            }
            if episode.error.starts_with("429") {
                eprintln!(
                    "Crunchyroll is rate-limiting this account. Wait before retrying or use another account."
                );
            }
            bail!("playback error: {}", episode.error);
        }
        Ok(episode)
    }

    pub fn episode_info(&self, id: &str) -> Result<EpisodeInfo> {
        let url = format!(
            "https://www.crunchyroll.com/content/v2/cms/objects/{id}?ratings=true&preferred_audio_language=ja-JP&locale=en-US"
        );
        let mut response: EpisodeMetadataResponse = self.get_json(&url)?;
        response
            .data
            .drain(..)
            .next()
            .ok_or_else(|| anyhow::anyhow!("Crunchyroll returned no metadata for episode {id}"))
    }

    pub fn seasons(&self, id: &str, audio_locale: &str, sub_locale: &str) -> Result<Vec<Season>> {
        let url = format!(
            "https://www.crunchyroll.com/content/v2/cms/series/{id}/seasons?force_locale=&preferred_audio_language={audio_locale}&locale={sub_locale}"
        );
        Ok(self.get_json::<SeasonsResponse>(&url)?.data)
    }

    pub fn season_episodes(
        &self,
        id: &str,
        audio_locale: &str,
        sub_locale: &str,
    ) -> Result<Vec<SeasonEpisode>> {
        let url = format!(
            "https://www.crunchyroll.com/content/v2/cms/seasons/{id}/episodes?preferred_audio_language={audio_locale}&locale={sub_locale}"
        );
        Ok(self.get_json::<SeasonEpisodesResponse>(&url)?.data)
    }

    /// Where the account left off in each of `content_ids`.
    ///
    /// Not one entry per id: an episode nobody has opened is left out of the answer
    /// rather than sent back at zero, so this is a lookup to consult about an episode
    /// and not a list to walk alongside the one it was asked about.
    pub fn playheads(&self, content_ids: &[String]) -> Result<Vec<Playhead>> {
        // Before the account is asked for, because a season with no episodes in it is
        // an ordinary thing to ask about and not a reason to complain about the login.
        if content_ids.is_empty() {
            return Ok(Vec::new());
        }
        let account_id = self.account_id()?;
        let mut playheads = Vec::new();
        for url in playhead_urls(&account_id, content_ids) {
            playheads.extend(self.get_json::<PlayheadsResponse>(&url)?.data);
        }
        Ok(playheads)
    }

    /// The catalogue, in whatever order `sort_by` asks for: `popularity`,
    /// `newly_added` or `alphabetical`, narrowed to a category and an anime season where
    /// either was asked for.
    ///
    /// The total is worth passing on here and nowhere else. The types asked for are the
    /// types the column can open, and nothing is dropped from the page afterwards, so the
    /// count that comes back counts the rows this column actually shows - the offsets, the
    /// total and the rows on screen all count the same things. The simulcast filter is the
    /// one thing that does drop rows from a page, and it is sieved out on the worker
    /// rather than asked for here, which is why the header stops printing a total the
    /// moment it is on: see [`crate::tui::worker::Filters::sieve`].
    pub fn browse(
        &self,
        sort_by: &str,
        count: usize,
        start: usize,
        categories: Option<&str>,
        seasonal_tag: Option<&str>,
    ) -> Result<Page> {
        let url = browse_url(sort_by, count, start, categories, seasonal_tag);
        let answered = self.get_json::<BrowseResponse>(&url)?;
        let total = usize::try_from(answered.total).ok();
        Ok(Page {
            next: next_page(start, count, answered.data.len(), total),
            items: answered.data,
            total,
        })
    }

    /// Crunchyroll's own list of categories, which is what the genre filter offers.
    ///
    /// Asked for rather than written down here, because the list is Crunchyroll's to
    /// change and a slug this program had learned by heart would go on being offered for
    /// months after it stopped meaning anything. It is the same list the website's genre
    /// menu is drawn from.
    pub fn categories(&self) -> Result<Vec<Category>> {
        let url = "https://www.crunchyroll.com/content/v2/discover/categories?locale=en-US";
        Ok(self.get_json::<CategoriesResponse>(url)?.data)
    }

    /// And the anime seasons, newest first as Crunchyroll orders them - `fall-2024` and
    /// the forty or so before it.
    pub fn seasonal_tags(&self) -> Result<Vec<SeasonalTag>> {
        let url = "https://www.crunchyroll.com/content/v2/discover/seasonal_tags?locale=en-US";
        Ok(self.get_json::<SeasonalTagsResponse>(url)?.data)
    }

    /// The series a search turns up, most like the query first.
    ///
    /// No total. Search answers in groups and what is counted is the group as the
    /// endpoint filled it, before the sifting below throws away anything that is not a
    /// series; printing that beside a column holding fewer rows than it promises would
    /// be a header that never adds up. The end of the list is recognised the other way
    /// instead, by a page that comes back short.
    pub fn search(&self, query: &str, count: usize, start: usize) -> Result<Page> {
        let mut url = reqwest::Url::parse("https://www.crunchyroll.com/content/v2/discover/search")
            .expect("valid search URL");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("type", CATALOG_TYPES)
            .append_pair("n", &count.to_string())
            .append_pair("start", &start.to_string())
            .append_pair("ratings", "true")
            .append_pair("locale", "en-US");
        // Search answers with one group per requested type, so even a single type still
        // arrives wrapped in a group - and `top_results` comes along with whatever it
        // likes in it regardless of what was asked for. See [`searched`].
        //
        // The rows counted for the next page are the ones the groups arrived with rather
        // than the ones `searched` kept, for the reason `next_page` sets out: an offset
        // counts what the endpoint counts.
        let groups = self.get_json::<SearchResponse>(url.as_str())?.data;
        let returned: usize = groups.iter().map(|group| group.items.len()).sum();
        Ok(Page {
            items: searched(groups),
            total: None,
            next: next_page(start, count, returned, None),
        })
    }

    /// The films one movie listing holds, ready for the episodes column.
    ///
    /// `locale` and nothing else. The seasons and the episodes endpoints also take a
    /// `preferred_audio_language`, and it may well be that this one does too, but there
    /// was no account and no network here to ask Crunchyroll, and a parameter invented
    /// for an endpoint is a request that may come back 400 for every film there is.
    /// Which dub a film is played or written in is settled where it is settled for an
    /// episode: out of the versions the film carries, when the stream is asked for.
    pub fn movies(
        &self,
        id: &str,
        audio_locale: &str,
        sub_locale: &str,
    ) -> Result<Vec<SeasonEpisode>> {
        let url = format!(
            "https://www.crunchyroll.com/content/v2/cms/movie_listings/{id}/movies?locale={sub_locale}"
        );
        Ok(films(
            self.get_json::<MoviesResponse>(&url)?.data,
            audio_locale,
        ))
    }

    /// The series on the account's watchlist, most recently added first.
    ///
    /// Addressed by account rather than by token, so a session that never learned which
    /// account it belongs to says so here rather than asking about an account that does
    /// not exist and passing on the 404.
    ///
    /// The total this endpoint publishes is a count of the rows on the watchlist, films
    /// among them, and `watchlist_series` drops the films - so it is not passed on. The
    /// offsets still count every row, which is exactly why the arithmetic uses what came
    /// off the wire rather than what came out of the sifting.
    pub fn watchlist(&self, count: usize, start: usize) -> Result<Page> {
        let account_id = self.account_id()?;
        let mut url = reqwest::Url::parse(&format!(
            "https://www.crunchyroll.com/content/v2/discover/{account_id}/watchlist"
        ))
        .context("build the watchlist URL")?;
        url.query_pairs_mut()
            .append_pair("n", &count.to_string())
            .append_pair("start", &start.to_string())
            .append_pair("order", "desc")
            .append_pair("locale", "en-US")
            .append_pair("ratings", "true");
        let rows = self.get_json::<WatchlistResponse>(url.as_str())?.data;
        let returned = rows.len();
        Ok(Page {
            items: watchlist_items(rows),
            total: None,
            next: next_page(start, count, returned, None),
        })
    }

    /// What the account was last watching, newest first.
    ///
    /// Crunchyroll keeps the history as one row per thing played, so the same series
    /// turns up once for every episode of it that was watched. The catalogue column shows
    /// what those belong to and drills into it, so the entries are boiled down to the
    /// series behind them and then fetched in full: a title on its own would make this
    /// the one list in the column with no poster and nothing to say about itself.
    ///
    /// The fetching is also what sifts the list, which is why it is worth doing even for
    /// an id the entry already had. Only `objects` can say what a row turned out to be,
    /// and an episode watched outside any series, or an artist named as an entry's
    /// parent, is a row the column could do nothing with.
    ///
    /// This is the one listing that is not paged, and that boiling down is why. The
    /// endpoint counts and offsets episodes while the column holds what they belong to, so
    /// a page asked for at an offset of a hundred would begin a hundred episodes in and
    /// bring back however many series that happened to be - usually a handful, sometimes
    /// none at all, and overlapping whatever is already on screen, since a series watched
    /// yesterday and again this morning has episodes on both sides of the boundary.
    /// Nothing here can turn that into an offset of rows without paging through the
    /// whole history to find out. The alternatives were both worse: asking for one more
    /// page per keypress at the bottom would be a column that sometimes grows by nothing
    /// and sometimes repeats itself, and deduplicating across pages would make the
    /// column's length depend on how long the user had been scrolling. So the history is
    /// what one page of episodes says it is, and says so by reporting no next page - a
    /// hundred episodes is a long way back through anyone's watching, and the series
    /// worth continuing are at the top of it.
    pub fn history(&self, count: usize) -> Result<Page> {
        let account = self.account_id()?;
        let mut url = reqwest::Url::parse(&format!(
            "https://www.crunchyroll.com/content/v2/discover/{account}/history"
        ))
        .context("build the history URL")?;
        url.query_pairs_mut()
            .append_pair("page_size", &count.to_string())
            .append_pair("locale", "en-US")
            .append_pair("ratings", "true");
        let watched = self.get_json::<HistoryResponse>(url.as_str())?.data;
        Ok(Page {
            items: self
                .objects(&watched_ids(&watched))?
                .into_iter()
                .filter(|item| item.opens().is_some())
                .collect(),
            total: None,
            next: None,
        })
    }

    /// The catalogue entries for a set of ids, in the order they were asked for.
    pub fn objects(&self, ids: &[String]) -> Result<Vec<CatalogItem>> {
        let mut found = Vec::with_capacity(ids.len());
        for batch in object_batches(ids) {
            let url = format!(
                "https://www.crunchyroll.com/content/v2/cms/objects/{batch}?ratings=true&locale=en-US"
            );
            found.extend(self.get_json::<ObjectsResponse>(&url)?.data);
        }
        Ok(in_asked_order(ids, found))
    }

    /// Whether the watchlist already holds this series.
    ///
    /// Nothing answers that as a yes or a no. Asking the watchlist about one series
    /// comes back with the row it keeps for it, and with an empty list when it keeps
    /// none, so the length of the list is the answer.
    pub fn in_watchlist(&self, series_id: &str) -> Result<bool> {
        let account_id = self.account_id()?;
        let url = format!(
            "https://www.crunchyroll.com/content/v2/discover/{account_id}/watchlist/{series_id}?locale=en-US"
        );
        /// The row itself is never looked at, only counted, so nothing is built out of
        /// it - a shape that changes on Crunchyroll's side cannot break a question this
        /// narrow.
        #[derive(Deserialize)]
        struct WatchlistRows {
            #[serde(default)]
            data: Vec<serde::de::IgnoredAny>,
        }
        Ok(!self.get_json::<WatchlistRows>(&url)?.data.is_empty())
    }

    /// Puts the series on the watchlist. One half of a toggle rather than a way of
    /// making sure, so the caller is expected to have asked whether it is there already.
    pub fn watchlist_add(&self, series_id: &str) -> Result<()> {
        let account_id = self.account_id()?;
        let url = format!(
            "https://www.crunchyroll.com/content/v2/discover/{account_id}/watchlist?locale=en-US"
        );
        self.send_json(
            Method::POST,
            &url,
            &serde_json::json!({ "content_id": series_id }),
        )?
        .error_for_status()
        .context("put the series on the watchlist")?;
        Ok(())
    }

    /// Takes the series off the watchlist again.
    pub fn watchlist_remove(&self, series_id: &str) -> Result<()> {
        let account_id = self.account_id()?;
        let url = format!(
            "https://www.crunchyroll.com/content/v2/discover/{account_id}/watchlist/{series_id}?locale=en-US"
        );
        self.send_authed(Method::DELETE, &url, &HeaderMap::new(), None)?
            .error_for_status()
            .context("take the series off the watchlist")?;
        Ok(())
    }

    /// Moves one episode's playhead, in whole seconds from the start.
    ///
    /// Two things ride on this one request. It is how the position mpv has reached gets
    /// back to the account, so that the phone and the web player open where this client
    /// stopped; and it is how an episode is marked watched, which the name of the
    /// endpoint gives no hint of - Crunchyroll keeps no flag for it and counts an episode
    /// watched once its playhead has reached the end. So marking one watched is putting
    /// the playhead at the episode's running time, and marking it unwatched is putting it
    /// back to zero.
    ///
    /// Answered with a 204 and an empty body, so there is nothing to decode and nothing
    /// to hand back: either Crunchyroll took it or it did not.
    pub fn set_playhead(&self, content_id: &str, seconds: u32) -> Result<()> {
        let account_id = self.account_id()?;
        let url = format!("https://www.crunchyroll.com/content/v2/{account_id}/playheads");
        self.send_json(
            Method::POST,
            &url,
            &serde_json::json!({ "content_id": content_id, "playhead": seconds }),
        )?
        .error_for_status()
        .context("move the episode's playhead")?;
        Ok(())
    }

    pub fn manifest(&self, url: &str) -> Result<Vec<u8>> {
        let body = self
            .send_authed(Method::GET, url, &HeaderMap::new(), None)?
            .error_for_status()
            .context("request DASH manifest")?
            .bytes()
            .context("read DASH manifest")?
            .to_vec();
        if self.debug {
            println!("\n{}\n", String::from_utf8_lossy(&body));
        }
        Ok(body)
    }

    pub fn send_license_challenge(
        &self,
        content_id: &str,
        video_token: &str,
        challenge: &[u8],
    ) -> Result<Vec<u8>> {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert("x-cr-content-id", HeaderValue::from_str(content_id)?);
        headers.insert("x-cr-video-token", HeaderValue::from_str(video_token)?);
        headers.insert(
            "origin",
            HeaderValue::from_static("https://static.crunchyroll.com"),
        );
        headers.insert(
            "referer",
            HeaderValue::from_static("https://static.crunchyroll.com/"),
        );
        let response = self
            .send_authed(
                Method::POST,
                "https://www.crunchyroll.com/license/v1/license/widevine",
                &headers,
                Some(challenge),
            )?
            .error_for_status()
            .context("request Widevine license")?;
        #[derive(Deserialize)]
        struct LicenseResponse {
            license: String,
        }
        let encoded: LicenseResponse = response.json().context("decode license response")?;
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(encoded.license)
            .context("decode base64 license")
    }

    pub fn delete_stream(&self, content_id: &str, stream_token: &str) -> Result<bool> {
        let url =
            format!("https://www.crunchyroll.com/playback/v1/token/{content_id}/{stream_token}");
        Ok(self
            .send_authed(Method::DELETE, &url, &HeaderMap::new(), None)?
            .status()
            == reqwest::StatusCode::NO_CONTENT)
    }

    /// The client that talks to the CDN, kept separate from the one that talks to the
    /// API and pinned to HTTP/1.1.
    ///
    /// Media downloads run several at a time and are drained at the speed the consumer
    /// wants them, which during playback is real time. Multiplexed onto one HTTP/2
    /// connection they share its flow-control window, so a video body nobody is reading
    /// quickly holds the window shut and starves the audio requests beside it. A
    /// connection each costs a few sockets and takes that away.
    pub fn media_client(&self) -> &Client {
        &self.media
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    use crate::download::{episode_info, output_path};
    use crate::model::{
        CatalogItem, HistoryResponse, MoviesResponse, SearchResponse, WatchlistResponse, opens,
    };

    use super::{
        CATALOG_TYPES, Duration, OBJECTS_PER_REQUEST, account_id_from_jwt, browse_url,
        build_media_client, films, in_asked_order, next_page, object_batches, playhead_urls,
        searched, watched_ids, watchlist_items,
    };

    /// A JWT with `claims` as its payload, signed by nobody: the segments are what is
    /// read here, and a signature this code never checks is not worth faking.
    fn jwt(claims: &str) -> String {
        use base64::Engine;
        format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims)
        )
    }

    /// The token response is meant to name the account, but the shape of it has changed
    /// before, and the token itself says the same thing in its claims.
    #[test]
    fn reads_the_account_out_of_a_token() {
        assert_eq!(
            account_id_from_jwt(&jwt(r#"{"sub":"a1b2c3"}"#)).as_deref(),
            Some("a1b2c3")
        );
        assert_eq!(
            account_id_from_jwt(&jwt(r#"{"account_id":"a1b2c3","sub":"benefit-user"}"#)).as_deref(),
            Some("a1b2c3"),
            "the account the token names beats the subject it was issued to"
        );
        // Padding is not part of a JWT segment, but a `=` on the end is common enough
        // in the wild to be worth taking.
        let padded = format!("header.{}.signature", {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(r#"{"sub":"padded"}"#)
        });
        assert_eq!(account_id_from_jwt(&padded).as_deref(), Some("padded"));
    }

    /// Nothing here is an error: a token in an unexpected shape means the account has to
    /// be found elsewhere or given up on, not that the session is broken.
    #[test]
    fn a_token_that_names_no_account_is_not_a_failure() {
        for token in [
            "",
            "not-a-jwt",
            "header.!!!not-base64!!!.signature",
            &jwt("not json"),
            &jwt(r#"{"aud":"crunchyroll"}"#),
            &jwt(r#"{"sub":""}"#),
            &jwt(r#"{"sub":42}"#),
        ] {
            assert_eq!(account_id_from_jwt(token), None, "{token}");
        }
    }

    /// A watchlist holds whatever the account put on it, and that includes films and the
    /// occasional concert. All three are kept now: a film opens into the films its listing
    /// holds and a concert is already the thing that plays, so neither is the dead end it
    /// was when this list was series only. What is still dropped is a row nothing here
    /// knows how to open, which is the only kind left that would do nothing when selected.
    #[test]
    fn the_watchlist_offers_films_and_concerts_as_well_as_series() {
        let json = r#"{"total":5,"data":[
            {"id":"GY8VEQ95Y","panel":{"id":"GY8VEQ95Y","type":"series","title":"Frieren"}},
            {"id":"GM5V7XW1Q","panel":{"id":"GM5V7XW1Q","type":"movie_listing","title":"Suzume"}},
            {"id":"G9DUEG5MB","type":"series","title":"Dandadan"},
            {"id":"MC413F1E4B","panel":{"id":"MC413F1E4B","type":"musicConcert",
                                        "title":"LiSA LiVE is Smile Always"}},
            {"id":"GARTIST1","panel":{"id":"GARTIST1","type":"artist","title":"LiSA"}}
        ]}"#;
        let response: WatchlistResponse = serde_json::from_str(json).expect("a watchlist");
        let titles: Vec<String> = watchlist_items(response.data)
            .into_iter()
            .map(|item| item.title)
            .collect();
        assert_eq!(
            titles,
            ["Frieren", "Suzume", "Dandadan", "LiSA LiVE is Smile Always"]
        );
    }

    /// The catalogue asks for two types, and the whole of what this feature is about is
    /// that the second one is there. Both have to be types `open` can do something with:
    /// a type asked for that nothing knows how to open would be a page of rows that do
    /// nothing. Music is deliberately not among them - see [`CATALOG_TYPES`].
    #[test]
    fn the_catalogue_asks_for_films_as_well_as_series() {
        let types: Vec<&str> = CATALOG_TYPES.split(',').collect();
        assert_eq!(types, ["series", "movie_listing"]);
        for kind in types {
            assert!(
                opens(kind).is_some(),
                "{kind} is asked for and cannot be opened"
            );
        }
    }

    /// Search answers in groups, one per type asked for, plus a `top_results` that mixes
    /// in whatever it likes. A film among them is a row like any other now; a music video
    /// that turns up unasked is kept for the same reason, since it is a single playable
    /// thing. A season on its own is not something this column can open.
    #[test]
    fn a_search_keeps_the_films_and_drops_what_cannot_be_opened() {
        let json = r#"{"total":2,"data":[
            {"type":"top_results","count":3,"items":[
                {"id":"GY8VEQ95Y","type":"series","title":"Frieren"},
                {"id":"GSEASON1","type":"season","title":"Frieren Season 1"},
                {"id":"MV88BB8DC","type":"musicVideo","title":"Zankyosanka"}
            ]},
            {"type":"movie_listing","count":1,"items":[
                {"id":"GM5V7XW1Q","type":"movie_listing","title":"Suzume"}
            ]}
        ]}"#;
        let response: SearchResponse = serde_json::from_str(json).expect("a search answer");
        let titles: Vec<String> = searched(response.data)
            .into_iter()
            .map(|item| item.title)
            .collect();
        assert_eq!(titles, ["Frieren", "Zankyosanka", "Suzume"]);
    }

    /// A film is handed to the episodes column, the queue and the downloader as the one
    /// thing they all take, which is why the four fields a film has no answer of its own
    /// for are settled in `films` rather than special-cased in each of them. This is what
    /// they come to: the listing's title in front so the file lands in a directory named
    /// after the film, season one, and the films numbered from one in the order the
    /// listing gives them - which for a feature split in half is the order to watch them
    /// in.
    ///
    /// The audio locale is the one that matters most. A film that names none is labelled
    /// with the locale that was asked for, because the downloader hangs its single stream
    /// on the locale it finds here and an empty one means "none of the requested audio
    /// locales are available" for every film there is.
    #[test]
    fn a_film_becomes_a_row_the_episodes_column_can_show() {
        let json = r#"{"total":2,"data":[
            {"id":"GY8DVXWZ1","title":"Suzume","movie_listing_id":"GM5V7XW1Q",
             "movie_listing_title":"Suzume","description":"A door opens.",
             "duration_ms":7212000,
             "images":{"thumbnail":[[{"width":320,"source":"still.jpg"}]]}},
            {"id":"GY8DVXWZ2","title":"Suzume Part 2","movie_listing_title":"Suzume",
             "audio_locale":"ja-JP","duration_ms":null,
             "versions":[{"audio_locale":"en-US","guid":"GDUB0001"}]}
        ]}"#;
        let response: MoviesResponse = serde_json::from_str(json).expect("a movie listing");
        let films = films(response.data, "de-DE");
        assert_eq!(films.len(), 2);

        assert_eq!(films[0].id, "GY8DVXWZ1");
        assert_eq!(films[0].kind, "movie");
        assert_eq!(films[0].title, "Suzume");
        assert_eq!(films[0].series_title, "Suzume");
        assert_eq!(films[0].season_number, 1);
        assert_eq!(films[0].episode_number, 1);
        assert_eq!(films[0].description, "A door opens.");
        assert_eq!(films[0].duration_ms, 7_212_000);
        assert_eq!(films[0].images.thumbnail(320), Some("still.jpg"));
        assert_eq!(
            films[0].audio_locale, "de-DE",
            "a film that names no locale is labelled with the one that was asked for"
        );

        assert_eq!(
            films[1].episode_number, 2,
            "the second part is the second row"
        );
        assert_eq!(
            films[1].audio_locale, "ja-JP",
            "and one that does keeps its own"
        );
        assert_eq!(films[1].versions.len(), 1, "a dub to choose is still a dub");

        // And this is what the whole arrangement is for: the name of the file, out of the
        // one function that names every file this program writes.
        assert_eq!(
            output_path(&episode_info(&films[0]), "1080p").to_str(),
            Some("Suzume/Suzume S01E01 - Suzume [1080p].mkv")
        );
    }

    /// Where the catalogue column is told to carry on from, and when it is told there is
    /// nothing to carry on to. Getting this wrong is not a cosmetic matter: an offset
    /// that stands still asks for the same hundred series over and over, and one that
    /// runs past the end leaves a hole in the middle of the list.
    ///
    /// What is counted is what the endpoint sent, which is the callers' side of the
    /// bargain: a watchlist page of a hundred rows with five films among it is still a
    /// hundred rows of somebody's list, and carrying on from the ninety-five that were
    /// kept would fetch those five again at the head of the next page.
    #[test]
    fn a_page_that_came_back_short_is_the_end_of_the_list() {
        assert_eq!(next_page(0, 100, 100, None), Some(100));
        assert_eq!(next_page(100, 100, 100, None), Some(200));
        assert_eq!(
            next_page(100, 100, 40, None),
            None,
            "a page the endpoint could not fill is the last one it has"
        );
        assert_eq!(
            next_page(0, 100, 0, None),
            None,
            "an empty page is the end of the list, not a reason to ask again"
        );

        // A published total ends the list by arithmetic, without an empty page having to
        // be fetched to find that out.
        assert_eq!(next_page(0, 100, 100, Some(250)), Some(100));
        assert_eq!(next_page(100, 100, 100, Some(200)), None);
        assert_eq!(
            next_page(100, 100, 100, Some(150)),
            None,
            "a page that has already run past the total is the end of the list"
        );
    }

    /// A catalogue entry that is nothing but its id, which is all the ordering cares
    /// about.
    fn item(id: &str) -> CatalogItem {
        CatalogItem {
            id: id.to_owned(),
            ..CatalogItem::default()
        }
    }

    /// The history is a list of things played, and watching three episodes of one series
    /// in a row is the ordinary case: the column has to show that series once, where the
    /// first and most recent of those three put it. An entry that belongs to no series
    /// stands for itself instead, which is what a concert needs - what `objects` says it
    /// turned out to be decides whether it can stay - and an entry that names nothing at
    /// all is nothing to ask about.
    #[test]
    fn boils_the_history_down_to_what_was_watched() {
        let json = r#"{"data":[
            {"parent_id":"GY8VEQ95Y","parent_type":"series"},
            {"parent_id":"GY8VEQ95Y"},
            {"parent_id":"GRMG8ZQZR"},
            {"parent_id":"","panel":{"episode_metadata":{"series_id":"GEXH3W4JP"}}},
            {"parent_id":"GY8VEQ95Y"},
            {"id":"MC413F1E4B","parent_id":"GARTIST1","parent_type":"artist"},
            {"id":"MC413F1E4B","panel":null},
            {"panel":null}
        ]}"#;
        let entries = serde_json::from_str::<HistoryResponse>(json).unwrap().data;
        assert_eq!(
            watched_ids(&entries),
            ["GY8VEQ95Y", "GRMG8ZQZR", "GEXH3W4JP", "MC413F1E4B"]
        );
    }

    /// Newest-watched first is the only thing this list has over the catalogue, and the
    /// objects endpoint makes no promise about the order it answers in. An id it says
    /// nothing about - a series withdrawn, or one this account may no longer see - leaves
    /// no gap, and anything it volunteered that was not asked for is not part of the
    /// order and has no place in the column.
    #[test]
    fn puts_the_objects_answer_back_into_the_asked_for_order() {
        let asked: Vec<String> = ["GY8VEQ95Y", "GRMG8ZQZR", "GWITHDRAWN", "GY5P48XEY"]
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
        let answered = vec![
            item("GY5P48XEY"),
            item("GUNASKED"),
            item("GRMG8ZQZR"),
            item("GY8VEQ95Y"),
        ];
        let ordered: Vec<String> = in_asked_order(&asked, answered)
            .into_iter()
            .map(|series| series.id)
            .collect();
        assert_eq!(ordered, ["GY8VEQ95Y", "GRMG8ZQZR", "GY5P48XEY"]);
    }

    /// The ids go into the path as one comma-separated segment, so a whole page of
    /// history in a single request is a URL long enough for something in the middle to
    /// refuse it. Nothing to ask about is no request at all, which is what keeps an
    /// account with an empty history from asking the objects endpoint about no ids.
    #[test]
    fn asks_about_fifty_ids_at_a_time() {
        let remainder = 20;
        let ids: Vec<String> = (0..OBJECTS_PER_REQUEST * 2 + remainder)
            .map(|index| format!("G{index:03}"))
            .collect();
        let batches = object_batches(&ids);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].split(',').count(), OBJECTS_PER_REQUEST);
        assert_eq!(batches[1].split(',').count(), OBJECTS_PER_REQUEST);
        assert_eq!(batches[2].split(',').count(), remainder);
        assert!(batches[0].starts_with("G000,G001,"));
        assert_eq!(
            batches[2].split(',').next_back(),
            ids.last().map(String::as_str)
        );
        assert!(object_batches(&[]).is_empty());
    }

    /// The ids travel in the query string, so a season long enough to overrun what a
    /// server will read has to be asked for in pieces - and the commas between them have
    /// to arrive as separators rather than as characters inside an id.
    #[test]
    fn a_long_list_of_ids_is_asked_for_in_pieces() {
        let ids: Vec<String> = (0..250).map(|index| format!("G{index}")).collect();
        let urls = playhead_urls("a1b2c3", &ids);

        let asked_about = |url: &str| {
            reqwest::Url::parse(url)
                .expect("a URL")
                .query_pairs()
                .find(|(key, _)| key == "content_ids")
                .map(|(_, value)| value.split(',').map(str::to_owned).collect::<Vec<_>>())
                .expect("a content_ids pair")
        };
        assert_eq!(urls.len(), 3);
        assert_eq!(asked_about(&urls[0]).len(), 100);
        assert_eq!(asked_about(&urls[1])[0], "G100");
        assert_eq!(
            asked_about(&urls[2]).len(),
            50,
            "the last piece is a short one"
        );
        assert!(
            urls[0].starts_with("https://www.crunchyroll.com/content/v2/a1b2c3/playheads?"),
            "{}",
            urls[0]
        );
        assert!(urls[0].contains("locale=en-US"));
        assert!(
            !urls[0].contains(','),
            "a raw comma is the URL's own punctuation, not the list's: {}",
            urls[0]
        );

        assert!(
            playhead_urls("a1b2c3", &[]).is_empty(),
            "nothing to ask about is nothing to ask"
        );
    }

    /// What a narrowed catalogue actually asks for. A filter nobody has set has to leave
    /// no trace in the URL at all: `categories=` is a question about a category whose
    /// slug is the empty string, and the honest answer to it is an empty catalogue. The
    /// slugs and the ids both carry hyphens, which have to arrive as part of the value
    /// rather than as anything the URL means by itself.
    #[test]
    fn the_browse_url_carries_only_the_filters_that_are_set() {
        let pairs = |url: &str| {
            reqwest::Url::parse(url)
                .expect("a URL")
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<Vec<_>>()
        };
        let plain = browse_url("popularity", 100, 0, None, None);
        assert!(
            plain.starts_with("https://www.crunchyroll.com/content/v2/discover/browse?"),
            "{plain}"
        );
        let asked = pairs(&plain);
        assert!(asked.contains(&("sort_by".to_owned(), "popularity".to_owned())));
        assert!(
            asked.contains(&("type".to_owned(), CATALOG_TYPES.to_owned())),
            "the filtered catalogue asked for fewer kinds of row than the plain one: {plain}"
        );
        assert!(asked.contains(&("n".to_owned(), "100".to_owned())));
        assert!(asked.contains(&("start".to_owned(), "0".to_owned())));
        assert!(
            !asked.iter().any(|(key, _)| key == "categories"),
            "an unset filter is not a filter set to nothing: {plain}"
        );
        assert!(!asked.iter().any(|(key, _)| key == "seasonal_tag"));

        let narrowed = browse_url(
            "newly_added",
            100,
            0,
            Some("slice-of-life"),
            Some("fall-2024"),
        );
        let asked = pairs(&narrowed);
        assert!(asked.contains(&("categories".to_owned(), "slice-of-life".to_owned())));
        assert!(asked.contains(&("seasonal_tag".to_owned(), "fall-2024".to_owned())));

        // The cleared filter is an empty string on its way through the interface, and it
        // has to read as "no category" here rather than as one nothing belongs to.
        let cleared = pairs(&browse_url(
            "popularity",
            100,
            0,
            Some(""),
            Some("fall-2024"),
        ));
        assert!(!cleared.iter().any(|(key, _)| key == "categories"));
        assert!(cleared.contains(&("seasonal_tag".to_owned(), "fall-2024".to_owned())));
    }

    /// Serves one request: the headers for a `promised`-byte body, then `sent` of those
    /// bytes handed over one at a time `gap` apart, and silence afterwards.
    fn dribbling_server(promised: usize, sent: usize, gap: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a test server");
        let address = format!(
            "http://{}/media",
            listener.local_addr().expect("test server address")
        );
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // The request itself is of no interest, but it has to come off the socket.
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {promised}\r\n\r\n");
            if stream.write_all(headers.as_bytes()).is_err() {
                return;
            }
            for _ in 0..sent {
                thread::sleep(gap);
                if stream.write_all(b"x").is_err() {
                    return;
                }
            }
            // Held open, so a client still waiting for the rest of the body is waiting
            // on a silent socket rather than on a closed one.
            thread::sleep(Duration::from_secs(5));
        });
        address
    }

    /// The media client's timeout has to apply to each read rather than to the response
    /// as a whole: one on-demand track arrives as a single body drained at the speed the
    /// consumer wants it, so a total deadline would cut a perfectly healthy stream off
    /// partway through the episode.
    #[test]
    fn a_media_body_may_outlast_the_stall_timeout() {
        // Six bytes 150ms apart: 900ms in all, comfortably past the timeout, with no
        // single wait anywhere near it.
        let url = dribbling_server(6, 6, Duration::from_millis(150));
        let client = build_media_client(Duration::from_millis(500)).expect("media client");
        let mut response = client.get(&url).send().expect("send the request");
        let mut body = Vec::new();
        response
            .read_to_end(&mut body)
            .expect("read the whole body");
        assert_eq!(body, b"xxxxxx");
    }

    /// And it does have to fire. A CDN connection that goes quiet mid-body is what the
    /// timeout is there to notice, rather than parking a worker on it until the process
    /// is killed.
    #[test]
    fn a_silent_media_body_gives_up() {
        // Promises ten bytes and sends one, leaving the client on an open, silent socket.
        let url = dribbling_server(10, 1, Duration::from_millis(10));
        let client = build_media_client(Duration::from_millis(300)).expect("media client");
        let mut response = client.get(&url).send().expect("send the request");
        let started = Instant::now();
        let outcome = response.read_to_end(&mut Vec::new());
        assert!(
            outcome.is_err(),
            "a body that stopped arriving must not read as a finished one"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "gave up only after {:?}",
            started.elapsed()
        );
    }
}
