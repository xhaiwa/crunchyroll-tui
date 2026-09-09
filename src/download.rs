use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Response;
use reqwest::header::{CONTENT_RANGE, RANGE};
use widevine::Key;

use crate::api::CrunchyrollClient;
use crate::drm::{content_key_hex, decrypt_mp4, get_license_keys};
use crate::manifest::{
    AdaptationSet, Manifest, Representation, SegmentBase, SegmentTemplate, build_url,
    expand_timeline, get_pssh, is_on_demand, parse_byte_range, parse_manifest,
    select_representation,
};
use crate::model::{Episode, EpisodeInfo, EpisodeMetadata, SeasonEpisode, Subtitle};
use crate::output::{MediaTrack, merge_everything};
use crate::play::{LivePipes, play};
use crate::progress::ProgressBar;
use crate::util::{language_name, sanitize_filename};

const MAX_WORKERS: usize = 10;
const MAX_BUFFERED_SEGMENTS: usize = MAX_WORKERS * 2;
/// How many audio versions are downloaded at once. Every one of them holds a playback
/// session open for as long as it runs, and Crunchyroll counts concurrent streams per
/// account: `--audio-lang all` on a series with a dozen dubs would otherwise ask for a
/// dozen at once and be answered with the 429 this code already has a message for.
const MAX_CONCURRENT_VERSIONS: usize = 3;
/// How many segment requests are in flight across the whole process. `MAX_WORKERS` is
/// per track, and several tracks download at once, so without a global bound a run with
/// many dubs points a hundred-odd parallel requests at the CDN.
const MAX_CONCURRENT_REQUESTS: usize = MAX_WORKERS;
/// How many playback sessions are opened at once, as opposed to held open. Playback
/// needs every track live at the same time and so cannot use `MAX_CONCURRENT_VERSIONS`;
/// staggering the requests that open the sessions is what keeps that burst down.
const MAX_CONCURRENT_SESSION_OPENS: usize = 1;
/// How many times a dropped on-demand response is picked up again before giving up.
const ON_DEMAND_RETRIES: u32 = 5;
const MEDIA_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";

/// Caps how many threads may be inside a region at once, across every track and version.
///
/// A permit is only ever held for the length of one request, never while waiting on
/// anything else, so the holders always drain and the waiters always get their turn.
struct Semaphore {
    free: Mutex<usize>,
    released: Condvar,
}

impl Semaphore {
    const fn new(permits: usize) -> Self {
        Self {
            free: Mutex::new(permits),
            released: Condvar::new(),
        }
    }

    fn acquire(&self) -> Permit<'_> {
        let mut free = self
            .released
            .wait_while(self.free.lock().expect("semaphore poisoned"), |free| {
                *free == 0
            })
            .expect("semaphore poisoned");
        *free -= 1;
        drop(free);
        Permit(self)
    }
}

struct Permit<'a>(&'a Semaphore);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().expect("semaphore poisoned") += 1;
        self.0.released.notify_one();
    }
}

static SEGMENT_REQUESTS: Semaphore = Semaphore::new(MAX_CONCURRENT_REQUESTS);
static SESSION_OPENS: Semaphore = Semaphore::new(MAX_CONCURRENT_SESSION_OPENS);

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub audio_langs: Vec<String>,
    pub subtitles_langs: Vec<String>,
    pub cc_langs: Vec<String>,
    pub video_quality: String,
    pub audio_quality: String,
    /// Play the episode with mpv as it arrives instead of writing an MKV.
    pub play: bool,
    pub mpv_args: Vec<String>,
}

fn temp_path(prefix: &str, suffix: &str) -> Result<PathBuf> {
    let (file, path) = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile()
        .context("create temporary media file")?
        .keep()
        .map_err(|error| error.error)
        .context("keep temporary media file")?;
    drop(file);
    Ok(path)
}

fn encrypted_path(output: &Path) -> PathBuf {
    PathBuf::from(format!("{}.enc", output.display()))
}

/// True once the player has let go of its end of a pipe, which is how a normal quit
/// reaches the threads that are still feeding it.
fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
    })
}

/// Asks the CDN for `url`, or for the `range` of it that is wanted.
fn media_request(client: &CrunchyrollClient, url: &str, range: Option<String>) -> Result<Response> {
    let mut request = client
        .media_client()
        .get(url)
        .header("origin", "https://static.crunchyroll.com")
        .header("referer", "https://static.crunchyroll.com/")
        .header("user-agent", MEDIA_USER_AGENT);
    if let Some(range) = range {
        request = request.header(RANGE, range);
    }
    let response = request.send().with_context(|| format!("download {url}"))?;
    if response.status() != reqwest::StatusCode::OK
        && response.status() != reqwest::StatusCode::PARTIAL_CONTENT
    {
        bail!("unexpected status {} downloading {url}", response.status());
    }
    Ok(response)
}

/// Reads a media body that is wanted in one piece.
///
/// `bytes()` would be shorter, but it hands the client's timeout to the whole response
/// rather than to one read of it, which forces a choice between cutting off a slow
/// connection that is still delivering and waiting out one that has died. Reading it by
/// hand keeps the per-read stall timeout for these bodies too, so a segment still
/// arriving is never given up on and a stalled one is noticed as soon as it stalls.
fn read_body(mut response: Response) -> Result<Vec<u8>> {
    // Only as a hint, and only up to a point: the length is the server's word, and a
    // wrong one should cost a few reallocations rather than the memory it asked for.
    let expected = response.content_length().unwrap_or(0).min(16 << 20) as usize;
    let mut body = Vec::with_capacity(expected);
    response
        .read_to_end(&mut body)
        .context("read media response")?;
    Ok(body)
}

fn download_part(client: &CrunchyrollClient, url: &str) -> Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 0..5 {
        if attempt > 0 {
            thread::sleep(Duration::from_secs((attempt * 2) as u64));
        }
        match media_request(client, url, None).and_then(read_body) {
            Ok(body) => return Ok(body),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("download failed"))
        .context("failed after 5 attempts"))
}

/// Segments waiting for their turn at the writer, plus how far the writer has got,
/// which is what bounds how far ahead the workers are allowed to fetch.
struct SegmentWindow {
    buffered: Vec<Option<Result<Vec<u8>>>>,
    written: usize,
    stopped: bool,
}

/// Fetches `urls` with `MAX_WORKERS` threads and writes them to `writer` in order,
/// holding at most `MAX_BUFFERED_SEGMENTS` fetched-but-unwritten segments at a time.
///
/// Each segment goes out the moment its turn comes rather than at the end of a batch.
/// Waiting for a whole batch bounds memory just as well but hands the consumer the
/// stream in bursts, which for playback means mpv gets a minute or so of video and then
/// nothing at all while the next batch downloads.
fn stream_segments<W, F, P>(writer: &mut W, urls: &[String], fetch: F, progress: P) -> Result<()>
where
    W: Write,
    F: Fn(&str) -> Result<Vec<u8>> + Sync,
    P: Fn(u64) + Sync,
{
    let fetched = AtomicU64::new(0);
    let next = AtomicUsize::new(0);
    let window = Mutex::new(SegmentWindow {
        buffered: (0..urls.len()).map(|_| None).collect(),
        written: 0,
        stopped: false,
    });
    let moved = Condvar::new();

    thread::scope(|scope| {
        for _ in 0..MAX_WORKERS.min(urls.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= urls.len() {
                        break;
                    }
                    // Hold off until the writer is close enough behind for this segment
                    // to fit in the window, so memory stays bounded either way.
                    let state = moved
                        .wait_while(window.lock().expect("segment window poisoned"), |state| {
                            !state.stopped && index >= state.written + MAX_BUFFERED_SEGMENTS
                        })
                        .expect("segment window poisoned");
                    if state.stopped {
                        break;
                    }
                    drop(state);

                    // Taken after the window has made room, and only for the request
                    // itself: a permit held across the wait above would let one track's
                    // idle workers lock every other track out.
                    let result = {
                        let _permit = SEGMENT_REQUESTS.acquire();
                        fetch(&urls[index])
                    };
                    if result.is_ok() {
                        progress(fetched.fetch_add(1, Ordering::Relaxed) + 1);
                    }
                    window.lock().expect("segment window poisoned").buffered[index] = Some(result);
                    moved.notify_all();
                }
            });
        }

        let outcome = (|| {
            for index in 0..urls.len() {
                let data = moved
                    .wait_while(window.lock().expect("segment window poisoned"), |state| {
                        state.buffered[index].is_none()
                    })
                    .expect("segment window poisoned")
                    .buffered[index]
                    .take()
                    .expect("waited until the segment was there")?;
                writer.write_all(&data).context("write media segment")?;
                window.lock().expect("segment window poisoned").written = index + 1;
                moved.notify_all();
            }
            Ok(())
        })();

        // Nothing more will be written, so release the workers that are still queueing
        // for a place in the window instead of leaving them to hold up the scope.
        window.lock().expect("segment window poisoned").stopped = true;
        moved.notify_all();
        outcome
    })
}

/// The first byte a `Content-Range: bytes {start}-{end}/{total}` covers, and the size of
/// the whole file when the header gave one.
///
/// `total` is allowed to be `*` when the server does not know how long the file is; the
/// range itself is honoured all the same, so the header still says where the body picks
/// up. Reading that as no `Content-Range` at all would call a served range an ignored
/// one and refuse to resume from a perfectly good response.
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let (range, total) = value.strip_prefix("bytes ")?.rsplit_once('/')?;
    let start = range.split_once('-')?.0.parse().ok()?;
    Some((start, total.parse().ok()))
}

/// The first byte this response carries and the size of the whole file.
///
/// Only a real `Content-Range` counts. A server that ignored the requested range
/// answers `200` with the file from the top, and that is not something to resume from.
fn content_range(response: &Response) -> Option<(u64, Option<u64>)> {
    response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_range)
}

/// Opens the response carrying `url` from `position` on, along with the size of the
/// whole file when the server was willing to say.
fn open_on_demand(
    client: &CrunchyrollClient,
    url: &str,
    start: u64,
    position: u64,
) -> Result<(Response, Option<u64>)> {
    let response = media_request(client, url, Some(format!("bytes={position}-")))?;
    match content_range(&response) {
        Some((first, _)) if first != position => {
            bail!("asked {url} for byte {position} but the response starts at {first}")
        }
        Some((_, total)) => Ok((response, total)),
        None if position > start => {
            bail!("cannot resume {url}: the server ignored the requested byte range")
        }
        // A first response without a Content-Range says nothing about where the file
        // ends, so there is no length to hold the body to.
        None => Ok((response, None)),
    }
}

/// Streams `url` from byte `start` to the end of the file into `writer`.
///
/// One response covers the whole track, and it is read at the speed the consumer drains
/// it: during playback that is real time, so twenty minutes on a connection the CDN is
/// free to drop. Every byte written is accounted for, so a response that dies partway
/// is picked up again from where it stopped instead of ending the track early and
/// silently, which downstream looks exactly like an episode that is only a minute long.
fn stream_on_demand(
    client: &CrunchyrollClient,
    url: &str,
    start: u64,
    title: &str,
    quiet: bool,
    writer: &mut impl Write,
) -> Result<()> {
    let (response, mut total) = open_on_demand(client, url, start, start)?;
    let bar = if quiet {
        ProgressBar::hidden()
    } else {
        let known = total.or_else(|| response.content_length().map(|length| start + length));
        ProgressBar::new(title, known.unwrap_or_default(), "")
    };
    let streamed = (|| {
        let mut position = start;
        let mut failures = 0_u32;
        let mut opened = Ok((response, total));
        loop {
            let problem = match opened {
                Ok((mut response, size)) => {
                    if let Some(size) = size {
                        total = Some(size);
                        bar.set_total(size);
                    }
                    let mut buffer = [0_u8; 64 * 1024];
                    let stopped = loop {
                        match response.read(&mut buffer) {
                            Ok(0) => break None,
                            Ok(count) => {
                                writer
                                    .write_all(&buffer[..count])
                                    .context("write on-demand media")?;
                                position += count as u64;
                                failures = 0;
                                bar.update(position);
                            }
                            Err(error) => break Some(anyhow::Error::new(error)),
                        }
                    };
                    match total {
                        Some(total) if position >= total => return Ok(()),
                        // Nothing said how long the file is, so a body that ends without
                        // an error has to be taken at its word.
                        None if stopped.is_none() => return Ok(()),
                        _ => {}
                    }
                    stopped.unwrap_or_else(|| anyhow::anyhow!("the response ended early"))
                }
                Err(error) => error,
            };
            failures += 1;
            if failures > ON_DEMAND_RETRIES {
                return Err(problem).with_context(|| {
                    format!(
                        "read on-demand media: stopped at byte {position}{} after {ON_DEMAND_RETRIES} attempts to carry on",
                        total.map_or(String::new(), |total| format!(" of {total}"))
                    )
                });
            }
            thread::sleep(Duration::from_secs(u64::from(failures) * 2));
            opened = open_on_demand(client, url, start, position);
        }
    })();
    bar.finish();
    streamed
}

fn download_range(client: &CrunchyrollClient, url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
    read_body(media_request(
        client,
        url,
        Some(format!("bytes={start}-{end}")),
    )?)
    .context("read ranged media response")
}

/// The two shapes a DASH representation comes in: numbered segments listed in a
/// SegmentTimeline, or one on-demand file addressed with byte ranges.
enum TrackSource<'a> {
    Segmented {
        template: &'a SegmentTemplate,
        representation: &'a Representation,
    },
    OnDemand {
        segment_base: &'a SegmentBase,
        representation: &'a Representation,
    },
}

impl<'a> TrackSource<'a> {
    fn new(
        manifest: &Manifest,
        set: &'a AdaptationSet,
        representation: &'a Representation,
    ) -> Result<Self> {
        if is_on_demand(manifest) {
            Ok(Self::OnDemand {
                segment_base: representation
                    .segment_base
                    .as_ref()
                    .context("on-demand representation has no SegmentBase")?,
                representation,
            })
        } else {
            Ok(Self::Segmented {
                template: set
                    .segment_template
                    .as_ref()
                    .context("segmented adaptation has no SegmentTemplate")?,
                representation,
            })
        }
    }

    /// Downloads the MP4 initialization segment. It carries the KID that picks the
    /// license key, so it has to come down before anything can be decrypted.
    fn initialization(&self, client: &CrunchyrollClient) -> Result<Vec<u8>> {
        match self {
            Self::Segmented {
                template,
                representation,
            } => {
                let url = build_url(
                    &representation.base_url,
                    &representation.id,
                    &template.initialization,
                    None,
                );
                download_part(client, &url).context("download initialization segment")
            }
            Self::OnDemand {
                segment_base,
                representation,
            } => {
                let (start, end) = parse_byte_range(&segment_base.initialization.range)?;
                download_range(client, &representation.base_url, start, end)
                    .context("download initialization range")
            }
        }
    }

    /// Streams everything after the initialization segment into `writer`. `quiet`
    /// suppresses the progress bar for playback, where mpv owns the terminal.
    fn body(
        &self,
        client: &CrunchyrollClient,
        title: &str,
        quiet: bool,
        writer: &mut impl Write,
    ) -> Result<()> {
        match self {
            Self::Segmented {
                template,
                representation,
            } => {
                let numbers = expand_timeline(
                    &template.timeline.segments,
                    template.start_number.unwrap_or(1),
                );
                let urls: Vec<_> = numbers
                    .into_iter()
                    .map(|number| {
                        build_url(
                            &representation.base_url,
                            &representation.id,
                            &template.media,
                            Some(number),
                        )
                    })
                    .collect();
                let bar = if quiet {
                    ProgressBar::hidden()
                } else {
                    ProgressBar::new(title, urls.len() as u64, "segments")
                };
                let streamed = stream_segments(
                    writer,
                    &urls,
                    |url| download_part(client, url),
                    |count| bar.update(count),
                );
                bar.finish();
                streamed
            }
            Self::OnDemand {
                segment_base,
                representation,
            } => {
                let (index_start, _) = parse_byte_range(&segment_base.index_range)?;
                stream_on_demand(
                    client,
                    &representation.base_url,
                    index_start,
                    title,
                    quiet,
                    writer,
                )
            }
        }
    }
}

/// Where a media track ends up once it has been pulled off the CDN.
enum Destination<'a> {
    /// Buffer it on disk, decrypt it into a temporary MP4 and mux later.
    Files,
    /// Feed it into a named pipe and let ffmpeg decrypt it on the fly for mpv.
    Pipes(&'a LivePipes),
}

/// Buffers the encrypted track on disk, then decrypts it into a temporary MP4.
fn fetch_to_file(
    client: &CrunchyrollClient,
    title: &str,
    source: &TrackSource<'_>,
    is_video: bool,
    keys: &[Key],
) -> Result<PathBuf> {
    let init_data = source.initialization(client)?;
    let output = temp_path(
        if is_video {
            "crdl-video-"
        } else {
            "crdl-audio-"
        },
        ".mp4",
    )?;
    let encrypted = encrypted_path(&output);
    let result = (|| {
        let mut file = File::create(&encrypted).context("create encrypted temporary media")?;
        file.write_all(&init_data)
            .context("write initialization segment")?;
        source.body(client, title, false, &mut file)?;
        drop(file);
        decrypt_mp4(&init_data, &encrypted, &output, keys)?;
        Ok(output.clone())
    })();
    let _ = fs::remove_file(&encrypted);
    if result.is_err() {
        let _ = fs::remove_file(&output);
    }
    result
}

/// Streams the still-encrypted track into a named pipe, publishing the pipe and its key
/// first so ffmpeg can pick both up and decrypt as the bytes arrive.
fn fetch_to_pipe(
    client: &CrunchyrollClient,
    source: &TrackSource<'_>,
    keys: &[Key],
    pipes: &LivePipes,
    slot: usize,
    locale: String,
) -> Result<()> {
    let init_data = source.initialization(client)?;
    let key = content_key_hex(&init_data, keys)?;
    let path = pipes.track(slot)?;
    pipes.publish(slot, MediaTrack::media(path.clone(), locale, Some(key)));

    // Waits for ffmpeg to open the other end, which it only does once every track has
    // published and the player has been started.
    let Some(mut pipe) = pipes.open_writer(&path)? else {
        return Ok(());
    };
    let result = pipe
        .write_all(&init_data)
        .context("write initialization segment")
        .and_then(|()| source.body(client, "", true, &mut pipe));
    match result {
        Err(error) if is_broken_pipe(&error) => Ok(()),
        other => other,
    }
}

struct TrackRequest<'a> {
    manifest: &'a Manifest,
    locale: Option<&'a str>,
    quality: &'a str,
    is_video: bool,
    keys: &'a [Key],
    /// Index this track occupies in the live pipe set, ignored for file downloads.
    slot: usize,
}

/// Returns the finished track for a file download, and nothing for playback, where the
/// track is published through `LivePipes` long before it has finished streaming.
fn fetch_track(
    client: &CrunchyrollClient,
    request: &TrackRequest<'_>,
    destination: &Destination<'_>,
) -> Result<Option<MediaTrack>> {
    let period = request
        .manifest
        .periods
        .first()
        .context("manifest has no Period")?;
    let set = period
        .adaptation_sets
        .iter()
        .find(|set| set.is_video() == request.is_video)
        .with_context(|| {
            format!(
                "manifest has no {} adaptation set",
                if request.is_video { "video" } else { "audio" }
            )
        })?;
    let representation = select_representation(set, request.is_video, request.quality)?;
    let source = TrackSource::new(request.manifest, set, representation)?;
    let locale = request.locale.unwrap_or_default().to_owned();

    match destination {
        Destination::Files => {
            let title = if request.is_video {
                "Downloading video".to_owned()
            } else {
                format!("Downloading {} audio", language_name(&locale))
            };
            let file = fetch_to_file(client, &title, &source, request.is_video, request.keys)?;
            Ok(Some(MediaTrack::media(file, locale, None)))
        }
        Destination::Pipes(pipes) => {
            fetch_to_pipe(client, &source, request.keys, pipes, request.slot, locale)?;
            Ok(None)
        }
    }
}

#[derive(Default)]
struct VersionTracks {
    video: Option<MediaTrack>,
    audio: Option<MediaTrack>,
}

/// Opens one audio locale's playback session and pulls its tracks. The first version
/// also pulls the video, which every locale shares.
#[allow(clippy::too_many_arguments)]
fn fetch_version(
    client: &CrunchyrollClient,
    options: &DownloadOptions,
    index: usize,
    locale: &str,
    content_id: &str,
    first_episode: &Episode,
    active_streams: &Mutex<HashMap<String, String>>,
    destination: &Destination<'_>,
) -> Result<VersionTracks> {
    let episode: Episode = if index == 0 {
        first_episode.clone()
    } else {
        // One playback request at a time. Crunchyroll answers a burst of them with a
        // 429 that takes the whole run down, and the versions have nothing to gain from
        // opening their sessions in the same instant.
        let permit = SESSION_OPENS.acquire();
        let episode = client
            .episode(content_id)
            .with_context(|| format!("request playback for {locale}"))?;
        active_streams
            .lock()
            .expect("active streams poisoned")
            .insert(content_id.to_owned(), episode.token.clone());
        drop(permit);
        episode
    };
    let body = client.manifest(&episode.manifest_url)?;
    let manifest = parse_manifest(&body)?;
    let pssh = get_pssh(&manifest)
        .with_context(|| format!("no Widevine PSSH in the manifest for {locale}"))?;
    let keys = get_license_keys(client, &pssh, content_id, &episode.token)
        .with_context(|| format!("get Widevine license for {locale}"))?;

    let audio_request = TrackRequest {
        manifest: &manifest,
        locale: Some(locale),
        quality: &options.audio_quality,
        is_video: false,
        keys: &keys,
        slot: index + 1,
    };
    let mut tracks = VersionTracks::default();
    if index == 0 {
        let video_request = TrackRequest {
            manifest: &manifest,
            locale: None,
            quality: &options.video_quality,
            is_video: true,
            keys: &keys,
            slot: 0,
        };
        thread::scope(|scope| {
            let video = scope.spawn(|| fetch_track(client, &video_request, destination));
            let audio = fetch_track(client, &audio_request, destination);
            let video = video
                .join()
                .map_err(|_| anyhow::anyhow!("video download thread panicked"))?;
            match (audio, video) {
                (Ok(audio), Ok(video)) => {
                    tracks = VersionTracks { video, audio };
                    Ok(())
                }
                (Err(error), Ok(video)) => {
                    remove_track(video.as_ref());
                    Err(error)
                }
                (Ok(audio), Err(error)) => {
                    remove_track(audio.as_ref());
                    Err(error)
                }
                (Err(error), Err(_)) => Err(error),
            }
        })?;
    } else {
        tracks.audio = fetch_track(client, &audio_request, destination)?;
    }

    match client.delete_stream(content_id, &episode.token) {
        Ok(true) => {}
        Ok(false) | Err(_) => eprintln!(
            "Failed to remove the player stream; later episodes may be temporarily blocked."
        ),
    }
    active_streams
        .lock()
        .expect("active streams poisoned")
        .remove(content_id);
    Ok(tracks)
}

fn remove_track(track: Option<&MediaTrack>) {
    if let Some(track) = track {
        let _ = fs::remove_file(&track.file);
    }
}

fn download_subtitle(client: &CrunchyrollClient, subtitle: &Subtitle) -> Result<PathBuf> {
    let body =
        read_body(media_request(client, &subtitle.url, None)?).context("read subtitle response")?;
    let suffix = format!(
        ".{}",
        if subtitle.format.is_empty() {
            "ass"
        } else {
            &subtitle.format
        }
    );
    let path = temp_path("crdl-subs-", &suffix)?;
    if let Err(error) = fs::write(&path, &body).context("write subtitle file") {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// Subtitles are small and ffmpeg wants them as complete files, so they all come down
/// before any media starts moving.
fn fetch_subtitles(
    client: &CrunchyrollClient,
    jobs: &[(String, bool, Subtitle)],
) -> Result<Vec<MediaTrack>> {
    let mut tracks: Vec<Option<MediaTrack>> = vec![None; jobs.len()];
    let mut first_error = None;
    thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .enumerate()
            .map(|(index, (locale, is_cc, subtitle))| {
                scope.spawn(move || -> Result<(usize, MediaTrack)> {
                    let file = download_subtitle(client, subtitle).with_context(|| {
                        format!("download subtitles for {}", language_name(locale))
                    })?;
                    Ok((
                        index,
                        MediaTrack {
                            file,
                            locale: locale.clone(),
                            format: subtitle.format.clone(),
                            is_cc: *is_cc,
                            key: None,
                        },
                    ))
                })
            })
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(Ok((index, track))) => tracks[index] = Some(track),
                Ok(Err(error)) if first_error.is_none() => first_error = Some(error),
                Err(_) if first_error.is_none() => {
                    first_error = Some(anyhow::anyhow!("subtitle worker panicked"));
                }
                _ => {}
            }
        }
    });
    if let Some(error) = first_error {
        for track in tracks.iter().flatten() {
            let _ = fs::remove_file(&track.file);
        }
        return Err(error);
    }
    Ok(tracks.into_iter().flatten().collect())
}

/// What the version workers have finished with so far, and the first thing that went
/// wrong.
#[derive(Default)]
struct MediaResults {
    video: Option<MediaTrack>,
    audio: Vec<Option<MediaTrack>>,
    error: Option<anyhow::Error>,
}

/// Downloads every version to temporary files and returns the video plus one audio
/// track per requested locale.
///
/// The versions go through a queue rather than a thread apiece: each one holds a
/// playback session open for as long as it runs, and a dozen dubs asking for a dozen
/// sessions at once is what Crunchyroll answers with a 429.
fn download_media(
    client: &CrunchyrollClient,
    options: &DownloadOptions,
    versions: &[(String, String)],
    first_episode: &Episode,
    active_streams: &Mutex<HashMap<String, String>>,
) -> Result<(MediaTrack, Vec<MediaTrack>)> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(MediaResults {
        audio: (0..versions.len()).map(|_| None).collect(),
        ..MediaResults::default()
    });
    thread::scope(|scope| {
        let handles: Vec<_> = (0..MAX_CONCURRENT_VERSIONS.min(versions.len()))
            .map(|_| {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((locale, content_id)) = versions.get(index) else {
                            break;
                        };
                        let outcome = fetch_version(
                            client,
                            options,
                            index,
                            locale,
                            content_id,
                            first_episode,
                            active_streams,
                            &Destination::Files,
                        );
                        let mut results = results.lock().expect("download results poisoned");
                        match outcome {
                            Ok(tracks) => {
                                results.audio[index] = tracks.audio;
                                if tracks.video.is_some() {
                                    results.video = tracks.video;
                                }
                            }
                            Err(error) if results.error.is_none() => results.error = Some(error),
                            Err(_) => {}
                        }
                    }
                })
            })
            .collect();
        // Joining takes the panic off the scope's hands, which is what lets a worker
        // that came apart be reported and cleaned up after rather than unwinding
        // straight through and leaving the temporary files behind.
        for handle in handles {
            if handle.join().is_err() {
                let mut results = results.lock().expect("download results poisoned");
                results
                    .error
                    .get_or_insert_with(|| anyhow::anyhow!("download worker panicked"));
            }
        }
    });

    let MediaResults {
        video,
        audio,
        error: first_error,
    } = results.into_inner().expect("download results poisoned");
    if let Some(error) = first_error {
        remove_track(video.as_ref());
        for track in audio.iter().flatten() {
            let _ = fs::remove_file(&track.file);
        }
        return Err(error);
    }
    let video = video.context("video download produced no file")?;
    let audio = audio
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .context("an audio download produced no file")?;
    Ok((video, audio))
}

/// Feeds every version into a named pipe and starts mpv as soon as each track knows its
/// pipe and key, so playback begins while the download is still running.
///
/// Unlike `download_media` this cannot queue the versions: ffmpeg opens every pipe at
/// once and a version that has not started yet would never publish, so playback would
/// wait for it forever. The bound that is left is `SESSION_OPENS` on the way in and
/// `SEGMENT_REQUESTS` on the segments themselves.
fn play_media(
    client: &CrunchyrollClient,
    options: &DownloadOptions,
    versions: &[(String, String)],
    first_episode: &Episode,
    active_streams: &Mutex<HashMap<String, String>>,
    subtitles: &[MediaTrack],
    info: &EpisodeInfo,
) -> Result<()> {
    let pipes = LivePipes::new(1 + versions.len())?;
    let mut first_error = None;
    thread::scope(|scope| {
        let handles: Vec<_> = versions
            .iter()
            .enumerate()
            .map(|(index, (locale, content_id))| {
                let pipes = &pipes;
                scope.spawn(move || {
                    let result = fetch_version(
                        client,
                        options,
                        index,
                        locale,
                        content_id,
                        first_episode,
                        active_streams,
                        &Destination::Pipes(pipes),
                    );
                    if let Err(error) = &result {
                        // mpv owns the terminal and shows nothing but a stalled clock,
                        // so say why the stream stopped at the moment it stops.
                        eprintln!("\nThe {locale} stream stopped: {error:#}");
                        // And wake the main thread, whether it is still waiting for this
                        // track to publish or already playing what the others sent.
                        pipes.fail();
                    }
                    result.map(|_| ())
                })
            })
            .collect();

        if let Some(tracks) = pipes.wait() {
            let (video, audio) = tracks
                .split_first()
                .expect("the video always occupies the first slot");
            println!("Buffering, mpv will open shortly...");
            if let Err(error) = play(&pipes, video, audio, subtitles, info, &options.mpv_args) {
                first_error = Some(error);
            }
        }
        // Playback is over, one way or another: no reader is coming, so threads still
        // waiting for one give up instead of blocking the join below forever.
        pipes.abandon();

        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) if first_error.is_none() => first_error = Some(error),
                Err(_) if first_error.is_none() => {
                    first_error = Some(anyhow::anyhow!("playback worker panicked"));
                }
                _ => {}
            }
        }
    });
    first_error.map_or(Ok(()), Err)
}

pub fn build_guid_by_locale(info: &EpisodeInfo, base_content_id: &str) -> HashMap<String, String> {
    let mut result: HashMap<_, _> = info
        .episode_metadata
        .versions
        .iter()
        .map(|version| (version.audio_locale.clone(), version.guid.clone()))
        .collect();
    if result.is_empty() && !info.episode_metadata.audio_locale.is_empty() {
        result.insert(
            info.episode_metadata.audio_locale.clone(),
            base_content_id.to_owned(),
        );
    }
    result
}

fn requested_or_all(requested: &[String], available: &HashMap<String, Subtitle>) -> Vec<String> {
    if requested == ["all"] {
        let mut result: Vec<_> = available
            .iter()
            .filter(|(_, subtitle)| !subtitle.url.is_empty())
            .map(|(locale, _)| locale.clone())
            .collect();
        result.sort();
        result
    } else {
        requested.to_vec()
    }
}

fn filter_available(
    requested: Vec<String>,
    available: &HashMap<String, Subtitle>,
    kind: &str,
    episode_number: i32,
) -> Vec<String> {
    requested
        .into_iter()
        .filter(|locale| {
            let present = available.get(locale).is_some_and(|item| !item.url.is_empty());
            if !present {
                println!(
                    "! {kind} locale {locale} is not available for episode {episode_number}, skipping it."
                );
            }
            present
        })
        .collect()
}

pub fn download_episode(
    client: &CrunchyrollClient,
    base_content_id: &str,
    info: &EpisodeInfo,
    options: &DownloadOptions,
) -> Result<()> {
    let series_title = sanitize_filename(&info.episode_metadata.series_title);
    let output_file = if options.play {
        None
    } else {
        let episode_title = sanitize_filename(&info.title);
        fs::create_dir_all(&series_title)
            .with_context(|| format!("create output directory {series_title}"))?;
        Some(Path::new(&series_title).join(format!(
            "{series_title} S{:02}E{:02} - {episode_title} [{}].mkv",
            info.episode_metadata.season_number,
            info.episode_metadata.episode_number,
            options.video_quality
        )))
    };
    if output_file.as_ref().is_some_and(|file| file.exists()) {
        println!(
            "Episode {} is already downloaded, skipping...",
            info.episode_metadata.episode_number
        );
        return Ok(());
    }

    let guid_by_locale = build_guid_by_locale(info, base_content_id);
    let audio_langs = if options.audio_langs == ["all"] {
        let mut result = Vec::new();
        let primary = &info.episode_metadata.audio_locale;
        if guid_by_locale.contains_key(primary) {
            result.push(primary.clone());
        }
        let mut rest: Vec<_> = guid_by_locale
            .keys()
            .filter(|locale| *locale != primary)
            .cloned()
            .collect();
        rest.sort();
        result.extend(rest);
        result
    } else {
        options.audio_langs.clone()
    };
    let versions: Vec<_> = audio_langs
        .iter()
        .filter_map(|locale| {
            if let Some(guid) = guid_by_locale.get(locale) {
                Some((locale.clone(), guid.clone()))
            } else {
                println!(
                    "! Audio locale {locale} is not available for episode {}, skipping it.",
                    info.episode_metadata.episode_number
                );
                None
            }
        })
        .collect();
    if versions.is_empty() {
        bail!(
            "none of the requested audio locales are available for episode {}",
            info.episode_metadata.episode_number
        );
    }

    println!(
        "{}: {} (S{:02}E{:02}) from {}",
        if options.play {
            "Playing"
        } else {
            "Downloading"
        },
        info.title,
        info.episode_metadata.season_number,
        info.episode_metadata.episode_number,
        info.episode_metadata.series_title
    );
    let first_episode = client.episode(&versions[0].1)?;
    let active_streams = Arc::new(Mutex::new(HashMap::<String, String>::from([(
        versions[0].1.clone(),
        first_episode.token.clone(),
    )])));

    let result = (|| -> Result<()> {
        let subtitles_langs = filter_available(
            requested_or_all(&options.subtitles_langs, &first_episode.subtitles),
            &first_episode.subtitles,
            "Subtitle",
            info.episode_metadata.episode_number,
        );
        let cc_langs = filter_available(
            requested_or_all(&options.cc_langs, &first_episode.captions),
            &first_episode.captions,
            "Closed caption",
            info.episode_metadata.episode_number,
        );
        println!(
            "Audio locales: {} | Subtitle locales: {} | CC locales: {}",
            audio_langs.join(", "),
            subtitles_langs.join(", "),
            cc_langs.join(", ")
        );

        let mut sub_jobs = Vec::new();
        for locale in subtitles_langs {
            let subtitle = first_episode.subtitles[&locale].clone();
            sub_jobs.push((locale, false, subtitle));
        }
        for locale in cc_langs {
            let caption = first_episode.captions[&locale].clone();
            sub_jobs.push((locale, true, caption));
        }
        let subtitle_tracks = fetch_subtitles(client, &sub_jobs)?;
        if !subtitle_tracks.is_empty() {
            println!("Downloaded subtitles!");
        }

        let outcome = match &output_file {
            Some(output_file) => {
                download_media(client, options, &versions, &first_episode, &active_streams)
                    .and_then(|(video, audio)| {
                        let merged =
                            merge_everything(&video, &audio, &subtitle_tracks, output_file, info);
                        remove_track(Some(&video));
                        for track in &audio {
                            let _ = fs::remove_file(&track.file);
                        }
                        merged
                    })
            }
            None => play_media(
                client,
                options,
                &versions,
                &first_episode,
                &active_streams,
                &subtitle_tracks,
                info,
            ),
        };
        for track in &subtitle_tracks {
            let _ = fs::remove_file(&track.file);
        }
        outcome
    })();

    println!("Cleaning up playback sessions...");
    let remaining = std::mem::take(&mut *active_streams.lock().expect("active streams poisoned"));
    for (content_id, stream_token) in remaining {
        let _ = client.delete_stream(&content_id, &stream_token);
    }
    result
}

/// The shape `download_episode` wants, out of what a season listing gives.
pub fn episode_info(episode: &SeasonEpisode) -> EpisodeInfo {
    EpisodeInfo {
        episode_metadata: EpisodeMetadata {
            series_title: episode.series_title.clone(),
            season_number: episode.season_number,
            episode_number: episode.episode_number,
            audio_locale: episode.audio_locale.clone(),
            versions: episode.versions.clone(),
            availability_starts: episode.availability_starts.clone(),
        },
        title: episode.title.clone(),
    }
}

pub fn download_season(
    client: &CrunchyrollClient,
    options: &DownloadOptions,
    episodes: &[SeasonEpisode],
) -> Result<()> {
    let Some(first) = episodes.first() else {
        println!("This season contains no episodes.");
        return Ok(());
    };
    println!(
        "{} season {} of {} ({} episodes)\n",
        if options.play {
            "Playing"
        } else {
            "Downloading"
        },
        first.season_number,
        first.series_title,
        episodes.len()
    );
    for episode in episodes {
        let info = episode_info(episode);
        if let Err(error) = download_episode(client, &episode.id, &info, options) {
            eprintln!(
                "Failed to download episode {}: {error:#}",
                episode.episode_number
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DubVersion;

    fn segment_body(index: usize) -> Vec<u8> {
        vec![b'a' + (index % 26) as u8; 1 + index % 7]
    }

    #[test]
    fn streams_segments_in_order() {
        let urls: Vec<_> = (0..200).map(|index| index.to_string()).collect();
        let mut output = Vec::new();
        stream_segments(
            &mut output,
            &urls,
            |url| {
                let index = url.parse::<usize>().unwrap();
                thread::sleep(Duration::from_millis(((200 - index) % 13) as u64));
                Ok(segment_body(index))
            },
            |_| {},
        )
        .unwrap();
        let expected: Vec<_> = (0..200).flat_map(segment_body).collect();
        assert_eq!(output, expected);
    }

    struct FailingWriter(io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.0, "write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn fetches_no_further_ahead_than_the_window() {
        struct CountingWriter<'a>(&'a AtomicUsize);

        impl Write for CountingWriter<'_> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let urls: Vec<_> = (0..200).map(|index| index.to_string()).collect();
        let written = AtomicUsize::new(0);
        let widest = AtomicUsize::new(0);
        let mut output = CountingWriter(&written);
        stream_segments(
            &mut output,
            &urls,
            |url| {
                let index = url.parse::<usize>().unwrap();
                widest.fetch_max(
                    index.saturating_sub(written.load(Ordering::SeqCst)),
                    Ordering::SeqCst,
                );
                Ok(vec![b'x'])
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(written.load(Ordering::SeqCst), urls.len());
        // Every segment left the writer long before the last one was fetched, which is
        // what keeps a player fed rather than handing it the stream one batch at a time.
        assert!(
            widest.load(Ordering::SeqCst) < MAX_BUFFERED_SEGMENTS,
            "fetched {} segments ahead of the writer",
            widest.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn reads_a_content_range_of_unknown_length() {
        // A served range whose total the server will not commit to. Reading this as an
        // absent header used to make a resumed download bail with "the server ignored
        // the requested byte range" when the server had done exactly as it was asked.
        assert_eq!(parse_content_range("bytes 100-200/*"), Some((100, None)));
        assert_eq!(
            parse_content_range("bytes 100-200/4096"),
            Some((100, Some(4096)))
        );
        assert_eq!(parse_content_range("bytes 0-0/1"), Some((0, Some(1))));
        // Nothing usable: no start to resume from.
        assert_eq!(parse_content_range("items 1-2/3"), None);
        assert_eq!(parse_content_range("bytes */1234"), None);
        assert_eq!(parse_content_range("bytes 100-200"), None);
    }

    #[test]
    fn a_semaphore_never_lets_more_than_its_permits_through() {
        const PERMITS: usize = 3;
        let semaphore = Semaphore::new(PERMITS);
        let inside = AtomicUsize::new(0);
        let widest = AtomicUsize::new(0);
        thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    for _ in 0..40 {
                        let _permit = semaphore.acquire();
                        widest
                            .fetch_max(inside.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                        thread::sleep(Duration::from_micros(50));
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(inside.load(Ordering::SeqCst), 0);
        assert!(
            widest.load(Ordering::SeqCst) <= PERMITS,
            "{} threads were inside at once",
            widest.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn propagates_writer_errors() {
        let mut output = FailingWriter(io::ErrorKind::Other);
        let error =
            stream_segments(&mut output, &["0".into()], |_| Ok(vec![1]), |_| {}).unwrap_err();
        assert!(format!("{error:#}").contains("write failed"));
        assert!(!is_broken_pipe(&error));
    }

    #[test]
    fn recognises_a_closed_player() {
        let mut output = FailingWriter(io::ErrorKind::BrokenPipe);
        let error =
            stream_segments(&mut output, &["0".into()], |_| Ok(vec![1]), |_| {}).unwrap_err();
        assert!(is_broken_pipe(&error));
    }

    #[test]
    fn versions_are_authoritative() {
        let info = EpisodeInfo {
            episode_metadata: EpisodeMetadata {
                audio_locale: "it-IT".into(),
                versions: vec![
                    DubVersion {
                        audio_locale: "ja-JP".into(),
                        guid: "ja-guid".into(),
                    },
                    DubVersion {
                        audio_locale: "it-IT".into(),
                        guid: "it-guid".into(),
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = build_guid_by_locale(&info, "base-id");
        assert_eq!(result["ja-JP"], "ja-guid");
        assert_eq!(result["it-IT"], "it-guid");
    }
}
