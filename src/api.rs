use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::model::{
    BrowseResponse, CatalogItem, Episode, EpisodeInfo, EpisodeMetadataResponse, SearchResponse,
    Season, SeasonEpisode, SeasonEpisodesResponse, SeasonsResponse,
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
}

#[derive(Clone)]
pub struct CrunchyrollClient {
    http: Client,
    media: Client,
    device_id: String,
    etp_rt: String,
    access_token: Arc<RwLock<String>>,
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

impl CrunchyrollClient {
    pub fn new(etp_rt: String, debug: bool) -> Result<Self> {
        let client = Self {
            http: build_api_client()?,
            media: build_media_client(MEDIA_STALL_TIMEOUT)?,
            device_id: Uuid::new_v4().to_string(),
            etp_rt,
            access_token: Arc::new(RwLock::new(String::new())),
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
                format!("device_id={}; etp_rt={}", self.device_id, self.etp_rt),
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
        *self.access_token.write().expect("token lock poisoned") = token.access_token;
        Ok(())
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

    use super::{Duration, build_media_client};

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
