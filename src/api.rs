use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result, bail};
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::model::{
    BrowseResponse, CatalogItem, Episode, EpisodeInfo, EpisodeMetadataResponse, Season,
    SeasonEpisode, SeasonEpisodesResponse, SearchResponse, SeasonsResponse,
};

const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";

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

impl CrunchyrollClient {
    pub fn new(etp_rt: String, debug: bool) -> Result<Self> {
        let client = Self {
            http: Client::builder().build().context("build HTTP client")?,
            media: Client::builder()
                .http1_only()
                .build()
                .context("build media HTTP client")?,
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
