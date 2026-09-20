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
    BrowseResponse, CatalogItem, Episode, EpisodeInfo, EpisodeMetadataResponse, HistoryEntry,
    HistoryResponse, ObjectsResponse, SearchResponse, Season, SeasonEpisode,
    SeasonEpisodesResponse, SeasonsResponse, WatchlistEntry, WatchlistResponse,
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

/// The series among a watchlist's rows.
///
/// The watchlist takes no `type` filter the way browse and search do, so the sifting has
/// to happen here, and it is the same sifting `search` does for the same reason: a movie
/// has no seasons endpoint and no episodes endpoint, so one in this column is a dead end
/// for anyone who selects it.
///
/// Split out from the request so the part that does not need an account or a network can
/// be tested against the two shapes the rows arrive in.
fn watchlist_series(entries: Vec<WatchlistEntry>) -> Vec<CatalogItem> {
    entries
        .into_iter()
        .map(WatchlistEntry::into_item)
        .filter(|item| item.kind == "series")
        .collect()
}

/// How many ids one `objects` request may name. The endpoint takes them as a
/// comma-separated path segment, so a whole page of history in one request would be a URL
/// some proxy between here and Crunchyroll is entitled to refuse; fifty is what the web
/// player asks for and is comfortably inside anything that counts.
const OBJECTS_PER_REQUEST: usize = 50;

/// The series behind a page of history, newest first and each one named once.
///
/// The history is a list of episodes and the catalogue column holds series, so several
/// entries in a row are usually the same series being worked through. Keeping the first
/// occurrence rather than the last is the whole point: the first is the most recently
/// watched, and what the column is for is saying what was being watched last.
///
/// Split out from the request because everything interesting about it - the order, and
/// what happens to an entry that names no series - is worth pinning down without a
/// network behind it.
fn series_watched(entries: &[HistoryEntry]) -> Vec<String> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .map(HistoryEntry::series_id)
        .filter(|series| !series.is_empty() && seen.insert(*series))
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

    /// The catalogue, in whatever order `sort_by` asks for: `popularity`,
    /// `newly_added` or `alphabetical`.
    ///
    /// Only series are asked for. A movie listing has no seasons and no episodes
    /// endpoint, so one in the list would be a dead end for anyone who selected it.
    pub fn browse(&self, sort_by: &str, count: usize, start: usize) -> Result<Vec<CatalogItem>> {
        let mut url = reqwest::Url::parse("https://www.crunchyroll.com/content/v2/discover/browse")
            .expect("valid browse URL");
        url.query_pairs_mut()
            .append_pair("sort_by", sort_by)
            .append_pair("type", "series")
            .append_pair("n", &count.to_string())
            .append_pair("start", &start.to_string())
            .append_pair("ratings", "true")
            .append_pair("locale", "en-US");
        Ok(self.get_json::<BrowseResponse>(url.as_str())?.data)
    }

    pub fn search(&self, query: &str, count: usize) -> Result<Vec<CatalogItem>> {
        let mut url = reqwest::Url::parse("https://www.crunchyroll.com/content/v2/discover/search")
            .expect("valid search URL");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("type", "series")
            .append_pair("n", &count.to_string())
            .append_pair("ratings", "true")
            .append_pair("locale", "en-US");
        // Search answers with one group per requested type, so a single `type=series`
        // still arrives wrapped in a group. `top_results` mixes types in regardless of
        // what was asked for, and anything that is not a series is a dead end here.
        Ok(self
            .get_json::<SearchResponse>(url.as_str())?
            .data
            .into_iter()
            .flat_map(|group| group.items)
            .filter(|item| item.kind == "series")
            .collect())
    }

    /// The series on the account's watchlist, most recently added first.
    ///
    /// Addressed by account rather than by token, so a session that never learned which
    /// account it belongs to says so here rather than asking about an account that does
    /// not exist and passing on the 404.
    pub fn watchlist(&self, count: usize) -> Result<Vec<CatalogItem>> {
        let account_id = self.account_id()?;
        let mut url = reqwest::Url::parse(&format!(
            "https://www.crunchyroll.com/content/v2/discover/{account_id}/watchlist"
        ))
        .context("build the watchlist URL")?;
        url.query_pairs_mut()
            .append_pair("n", &count.to_string())
            .append_pair("order", "desc")
            .append_pair("locale", "en-US")
            .append_pair("ratings", "true");
        Ok(watchlist_series(
            self.get_json::<WatchlistResponse>(url.as_str())?.data,
        ))
    }

    /// The series the account was last watching, newest first.
    ///
    /// Crunchyroll keeps the history as episodes, one row per thing played, so the same
    /// series turns up once for every episode of it that was watched. The catalogue
    /// column shows series and drills into seasons, so the episodes are boiled down to
    /// the series behind them and then fetched in full: a title on its own would make
    /// this the one list in the column with no poster and nothing to say about itself.
    pub fn history(&self, count: usize) -> Result<Vec<CatalogItem>> {
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
        self.objects(&series_watched(&watched))
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

    use crate::model::{CatalogItem, HistoryResponse, WatchlistResponse};

    use super::{
        Duration, OBJECTS_PER_REQUEST, account_id_from_jwt, build_media_client, in_asked_order,
        object_batches, series_watched, watchlist_series,
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

    /// A watchlist holds whatever the account put on it, and that includes films. One in
    /// this column would be a dead end - there is no seasons endpoint behind it - so it
    /// is dropped here the way `search` drops one, rather than being drawn as a row that
    /// does nothing when it is opened.
    #[test]
    fn a_film_on_the_watchlist_is_not_offered() {
        let json = r#"{"total":3,"data":[
            {"id":"GY8VEQ95Y","panel":{"id":"GY8VEQ95Y","type":"series","title":"Frieren"}},
            {"id":"GM5V7XW1Q","panel":{"id":"GM5V7XW1Q","type":"movie_listing","title":"Suzume"}},
            {"id":"G9DUEG5MB","type":"series","title":"Dandadan"}
        ]}"#;
        let response: WatchlistResponse = serde_json::from_str(json).expect("a watchlist");
        let titles: Vec<String> = watchlist_series(response.data)
            .into_iter()
            .map(|item| item.title)
            .collect();
        assert_eq!(titles, ["Frieren", "Dandadan"]);
    }

    /// A catalogue entry that is nothing but its id, which is all the ordering cares
    /// about.
    fn item(id: &str) -> CatalogItem {
        CatalogItem {
            id: id.to_owned(),
            ..CatalogItem::default()
        }
    }

    /// The history is a list of episodes, and watching three of one series in a row is
    /// the ordinary case: the column has to show that series once, where the first and
    /// most recent of those three put it. An entry that names no series at all belongs to
    /// nothing the column can drill into, so it goes.
    #[test]
    fn boils_the_history_down_to_the_series_watched() {
        let json = r#"{"data":[
            {"parent_id":"GY8VEQ95Y"},
            {"parent_id":"GY8VEQ95Y"},
            {"parent_id":"GRMG8ZQZR"},
            {"parent_id":"","panel":{"episode_metadata":{"series_id":"GEXH3W4JP"}}},
            {"parent_id":"GY8VEQ95Y"},
            {"id":"GZ7UV8KWZ","panel":null}
        ]}"#;
        let entries = serde_json::from_str::<HistoryResponse>(json).unwrap().data;
        assert_eq!(
            series_watched(&entries),
            ["GY8VEQ95Y", "GRMG8ZQZR", "GEXH3W4JP"]
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
