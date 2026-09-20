use std::ffi::CString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

use crate::model::EpisodeInfo;
use crate::output::{MediaTrack, build_mux_command};

/// Playback hands ffmpeg named pipes instead of finished files, so the picture starts
/// while the segments are still coming down. Each track thread creates its pipe, works
/// out the key that unlocks it and publishes both here; the main thread waits until
/// every slot is filled before it starts ffmpeg and mpv.
pub struct LivePipes {
    directory: TempDir,
    stream: PathBuf,
    state: Mutex<PipeState>,
    ready: Condvar,
}

struct PipeState {
    tracks: Vec<Option<MediaTrack>>,
    failed: bool,
    abandoned: bool,
}

/// How often a track thread re-checks whether ffmpeg has opened its pipe yet.
const OPEN_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How often playback re-checks whether a track has given up.
const FAILURE_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Thirty seconds of slack at each end of an episode, for [`resume_at`].
const RESUME_SLACK: u32 = 30;

/// What mpv is asked over its control socket. A command is one line, and the newline is
/// what ends it, so it is written here rather than left to whoever sends this.
const TIME_POS_COMMAND: &str = concat!(r#"{"command":["get_property","time-pos"]}"#, "\n");

/// How often the position mpv is at is handed on. Often enough that a lid closed
/// mid-episode costs seconds rather than the evening, seldom enough to be nothing at all
/// beside the video coming down the same line.
const REPORT_INTERVAL: Duration = Duration::from_secs(15);

/// How often mpv is asked where it is. Well inside [`REPORT_INTERVAL`] on purpose: the
/// position reported when playback ends is the last one this thread was given, because
/// by the time anyone notices mpv has gone its socket has gone with it and there is
/// nobody left to ask.
const POSITION_POLL: Duration = Duration::from_secs(1);

/// How long mpv is given to bind its control socket, and how often it is tried. It is
/// not listening the instant it is spawned - there is a window to open and a
/// configuration to read first - so the first few attempts are expected to fail.
const IPC_WAIT: Duration = Duration::from_secs(5);
const IPC_CONNECT_POLL: Duration = Duration::from_millis(100);

/// How long one read of the control socket waits. mpv answers a socket on the same
/// machine in microseconds, so this is only ever spent waiting for an answer that is not
/// coming, and the next poll a second later asks again.
const IPC_READ_TIMEOUT: Duration = Duration::from_millis(500);

/// How many lines are read looking for one answer. mpv publishes events on the same
/// connection whether or not anyone asked, so the answer is not always the next line; the
/// bound keeps a talkative mpv from holding this thread in the loop.
const MAX_REPLY_LINES: usize = 16;

/// Puts a pipe back into blocking mode so `write` waits for room instead of returning
/// `EAGAIN`, which `write_all` does not retry.
fn clear_nonblocking(pipe: &File) -> Result<()> {
    let descriptor = pipe.as_raw_fd();
    // SAFETY: `descriptor` is borrowed from a live `File` for the duration of the calls.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error()).context("clear O_NONBLOCK");
    }
    Ok(())
}

fn make_fifo(path: &Path) -> Result<()> {
    let name =
        CString::new(path.as_os_str().as_bytes()).context("pipe path contains a NUL byte")?;
    // SAFETY: `name` is a valid NUL-terminated path that outlives the call.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("create named pipe {}", path.display()));
    }
    Ok(())
}

impl LivePipes {
    /// Prepares the pipe ffmpeg muxes into and reserves `tracks` input slots.
    pub fn new(tracks: usize) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("crdl-live-")
            .tempdir()
            .context("create named pipe directory")?;
        let stream = directory.path().join("stream.mkv");
        make_fifo(&stream)?;
        Ok(Self {
            directory,
            stream,
            state: Mutex::new(PipeState {
                tracks: vec![None; tracks],
                failed: false,
                abandoned: false,
            }),
            ready: Condvar::new(),
        })
    }

    /// Creates the pipe for one input slot. Opening it for reading and for writing
    /// rendezvous in the kernel, which is what lets ffmpeg and the downloader meet.
    pub fn track(&self, slot: usize) -> Result<PathBuf> {
        let path = self.directory.path().join(format!("track-{slot}.mp4"));
        make_fifo(&path)?;
        Ok(path)
    }

    /// Where mpv is told to listen while it plays: beside the pipes, so that it goes
    /// when they do.
    ///
    /// The name is short and fixed because a Unix socket path is not a path like any
    /// other - the address a bind takes is capped near a hundred bytes, all in. Nothing
    /// about the episode goes in it: a series title is the one thing here long enough to
    /// overrun that, and it would do it for exactly the episodes worth watching.
    fn control_socket(&self) -> PathBuf {
        self.directory.path().join("mpv.sock")
    }

    pub fn publish(&self, slot: usize, track: MediaTrack) {
        let mut state = self.state.lock().expect("live pipes poisoned");
        state.tracks[slot] = Some(track);
        self.ready.notify_all();
    }

    pub fn fail(&self) {
        let mut state = self.state.lock().expect("live pipes poisoned");
        state.failed = true;
        self.ready.notify_all();
    }

    /// True once a track has given up, whether or not playback had already started.
    pub fn failed(&self) -> bool {
        self.state.lock().expect("live pipes poisoned").failed
    }

    /// Blocks until every track has published its pipe, or until one of them gave up.
    pub fn wait(&self) -> Option<Vec<MediaTrack>> {
        let state = self
            .ready
            .wait_while(self.state.lock().expect("live pipes poisoned"), |state| {
                !state.failed && state.tracks.iter().any(Option::is_none)
            })
            .expect("live pipes poisoned");
        (!state.failed).then(|| state.tracks.iter().flatten().cloned().collect())
    }

    /// Declares that no reader is coming any more, because playback failed to start or
    /// has already ended. Track threads still waiting for one give up instead of
    /// hanging the join that follows.
    pub fn abandon(&self) {
        let mut state = self.state.lock().expect("live pipes poisoned");
        state.abandoned = true;
    }

    /// Opens the write end of a pipe once ffmpeg has opened the read end, or returns
    /// `None` if playback was abandoned before that ever happened.
    ///
    /// A blocking `open` would be simpler but cannot be called off: a thread that
    /// reaches it after playback has already been given up parks in the kernel forever.
    /// A write-only open of a FIFO with no reader fails with `ENXIO` instead of
    /// blocking, which turns the wait into an interruptible poll.
    ///
    /// `O_CLOEXEC` is what makes the end of a track visible to ffmpeg. Without it every
    /// process spawned afterwards inherits the write end, so dropping the `File` here
    /// leaves the pipe open in that child and ffmpeg waits for bytes that never come.
    pub fn open_writer(&self, path: &Path) -> Result<Option<File>> {
        let name = CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("pipe path {} contains a NUL byte", path.display()))?;
        loop {
            // SAFETY: `name` is a valid NUL-terminated path that outlives the call.
            let descriptor = unsafe {
                libc::open(
                    name.as_ptr(),
                    libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if descriptor >= 0 {
                // SAFETY: `descriptor` was just returned by a successful `open` and is
                // not owned anywhere else, so the `File` takes sole ownership of it.
                let pipe = unsafe { File::from_raw_fd(descriptor) };
                clear_nonblocking(&pipe)
                    .with_context(|| format!("prepare media pipe {}", path.display()))?;
                return Ok(Some(pipe));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENXIO) {
                return Err(error).with_context(|| format!("open media pipe {}", path.display()));
            }
            if self.state.lock().expect("live pipes poisoned").abandoned {
                return Ok(None);
            }
            thread::sleep(OPEN_POLL_INTERVAL);
        }
    }
}

/// Where to open an episode the account has already seen some of, in seconds, or `None`
/// where there is nothing worth going back to.
///
/// Both ends of an episode are exceptions rather than positions. A playhead a few seconds
/// in is what watching the opening titles and closing the tab leaves behind, and starting
/// there is starting at the beginning with a jump in the way; one in the last half-minute
/// is a finished episode rather than a place to pick up, and resuming it would play the
/// credits and stop. `fully_watched` says the second of those outright, so it is taken at
/// its word whatever the position beside it reads.
///
/// A duration of zero is Crunchyroll not saying how long the episode runs, which happens
/// often enough - the metadata this client has in hand for a single episode URL carries
/// no running time at all - that reading it as a zero-length episode would throw every
/// resume away. The playhead is trusted instead: it was written by a player that did know.
pub fn resume_at(playhead: u32, duration_ms: u64, fully_watched: bool) -> Option<u32> {
    if fully_watched || playhead <= RESUME_SLACK {
        return None;
    }
    if duration_ms == 0 {
        return Some(playhead);
    }
    let duration = u32::try_from(duration_ms / 1000).unwrap_or(u32::MAX);
    (playhead.saturating_add(RESUME_SLACK) < duration).then_some(playhead)
}

/// The position out of one line of mpv's control socket, or `None` where that line is not
/// an answer carrying one.
///
/// An answer reads `{"data":842.123,"error":"success"}`. Anything else on that socket is
/// either an event mpv publishes of its own accord - it does that on the same connection,
/// whether or not anyone asked - or an answer that failed, which until playback has
/// actually started is the ordinary `property unavailable`. Neither is a position, and
/// neither is worth telling Crunchyroll about as if it were one.
fn time_pos(reply: &str) -> Option<f64> {
    let reply: serde_json::Value = serde_json::from_str(reply).ok()?;
    if reply.get("error")?.as_str()? != "success" {
        return None;
    }
    reply.get("data")?.as_f64()
}

/// Sleeps for `interval`, in slices the length the failure watcher uses, and gives up the
/// moment playback is over.
///
/// What comes after mpv is the interface being drawn again, and it waits on this thread by
/// way of the scope: a whole poll spent asleep in the kernel is a whole poll of a terminal
/// with nothing on it.
fn sleep_while_playing(interval: Duration, playing: &AtomicBool) {
    let until = Instant::now() + interval;
    while playing.load(Ordering::Relaxed) && Instant::now() < until {
        thread::sleep(FAILURE_POLL_INTERVAL.min(interval));
    }
}

/// Waits for mpv to bind its control socket, and connects to it.
///
/// Tried again rather than once, because mpv binds the socket some way into its own
/// startup. It gives up when the wait runs out and the moment playback ends, so a run
/// where mpv never came up at all does not hold the scope open for five seconds after it.
fn connect(socket: &Path, playing: &AtomicBool) -> Option<UnixStream> {
    let deadline = Instant::now() + IPC_WAIT;
    while playing.load(Ordering::Relaxed) {
        if let Ok(stream) = UnixStream::connect(socket) {
            stream.set_read_timeout(Some(IPC_READ_TIMEOUT)).ok()?;
            return Some(stream);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(IPC_CONNECT_POLL);
    }
    None
}

/// Asks mpv where it is, in whole seconds.
///
/// Lines are read until one of them is the answer, since mpv's events arrive on the same
/// connection and a burst of them around the start of a file would otherwise be mistaken
/// for one. A position mpv gives as negative is one it has not reached yet - it says that
/// while it is still getting to `--start` - and is no more a position than a missing one.
fn position(socket: &mut BufReader<UnixStream>) -> Option<u32> {
    socket
        .get_mut()
        .write_all(TIME_POS_COMMAND.as_bytes())
        .ok()?;
    for _ in 0..MAX_REPLY_LINES {
        let mut line = String::new();
        if socket.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if let Some(seconds) = time_pos(&line) {
            return (seconds >= 0.0).then_some(seconds as u32);
        }
    }
    None
}

/// Hands `latest` over if it says something `reported` did not, and remembers it.
///
/// Silence is the point: a paused episode answers with the same second for as long as it
/// is paused, and Crunchyroll has no use for being told that fifty times.
fn report_position(
    report: &(dyn Fn(u32) + Send + Sync),
    latest: Option<u32>,
    reported: &mut Option<u32>,
) {
    if latest != *reported
        && let Some(seconds) = latest
    {
        report(seconds);
        *reported = latest;
    }
}

/// Follows mpv along the episode and hands the position to `report`, every
/// [`REPORT_INTERVAL`] and once more when mpv goes.
///
/// Every step of it is best effort: a socket that never appears, a read that times out, a
/// reply that is not an answer. None of that is worth a word on a terminal mpv is drawing
/// on and none of it is worth ending an episode over, so a position that cannot be had is
/// simply not reported and the next poll tries again.
fn follow(socket: &Path, playing: &AtomicBool, report: &(dyn Fn(u32) + Send + Sync)) {
    let Some(stream) = connect(socket, playing) else {
        return;
    };
    let mut stream = BufReader::new(stream);
    let (mut latest, mut reported) = (None, None);
    let mut due = Instant::now() + REPORT_INTERVAL;
    while playing.load(Ordering::Relaxed) {
        if let Some(seconds) = position(&mut stream) {
            latest = Some(seconds);
        }
        if Instant::now() >= due {
            report_position(report, latest, &mut reported);
            due = Instant::now() + REPORT_INTERVAL;
        }
        sleep_while_playing(POSITION_POLL, playing);
    }
    // The one that matters: where the episode was left. mpv has already gone, so this is
    // the last position it gave rather than a fresh one.
    report_position(report, latest, &mut reported);
}

/// What playing an episode needs to know beyond the tracks themselves: what to hand mpv,
/// where in the episode to open it, and who to tell about the position it reaches.
///
/// Kept together in one place because they belong to the run rather than to the stream,
/// and because `play` has enough arguments already.
pub struct Playback<'a> {
    pub mpv_args: &'a [String],
    /// Where the account left off, from [`resume_at`].
    pub start_at: Option<u32>,
    /// Handed the position mpv is at, every so often and once at the end. `None` on a run
    /// with nobody to tell, which is also a run mpv is given no control socket for.
    pub playhead: Option<&'a (dyn Fn(u32) + Send + Sync)>,
}

/// Muxes the live tracks into Matroska and lets mpv play the result as it is produced.
///
/// ffmpeg writes into a pipe rather than mpv's stdin so that mpv keeps the terminal and
/// its keyboard bindings still work.
pub fn play(
    pipes: &LivePipes,
    video: &MediaTrack,
    audio: &[MediaTrack],
    subtitles: &[MediaTrack],
    info: &EpisodeInfo,
    playback: &Playback<'_>,
) -> Result<()> {
    let mut ffmpeg = build_mux_command(video, audio, subtitles, info);
    // mpv owns the terminal, so anything ffmpeg says while it runs is drawn over by the
    // status line. Keep its log instead and show it only when it turns out to matter,
    // rather than leaving the tail end of a normal quit looking like a failure.
    let log = tempfile::NamedTempFile::new().context("create ffmpeg log")?;
    ffmpeg
        .args(["-f", "matroska"])
        .arg(&pipes.stream)
        .stdin(Stdio::null())
        .stderr(Stdio::from(log.reopen().context("open ffmpeg log")?));
    let ffmpeg = Mutex::new(
        ffmpeg
            .spawn()
            .context("start ffmpeg; is it installed and in PATH?")?,
    );

    let metadata = &info.episode_metadata;
    let control = pipes.control_socket();
    let mut mpv = Command::new("mpv");
    mpv.arg(format!(
        "--force-media-title={} S{:02}E{:02} - {}",
        metadata.series_title, metadata.season_number, metadata.episode_number, info.title
    ))
    // The pipe cannot be seeked, so keep a generous window of it in memory instead.
    .args([
        "--cache=yes",
        "--demuxer-max-bytes=256MiB",
        "--demuxer-max-back-bytes=128MiB",
    ]);
    if let Some(seconds) = playback.start_at {
        // Not a seek, whatever it looks like: the stream is a named pipe and cannot be
        // seeked, so mpv serves this by reading forward through its demuxer cache until
        // it arrives. It lands in the right place, but a long jump into an episode is not
        // instant. Dropping the leading segments at the source is what would make it
        // instant, and that reaches into the DASH segment loop and the decryption path
        // either side of it - a larger change than this one.
        mpv.arg(format!("--start={seconds}"));
    }
    if playback.playhead.is_some() {
        // Only where somebody is listening. A control socket is a way in, and one nobody
        // reads is one left open for anything else on the machine to drive mpv through.
        mpv.arg(format!("--input-ipc-server={}", control.display()));
    }
    // Last, so that anything asked for by hand beats what is set here: mpv keeps the last
    // value of an option it is given twice.
    mpv.args(playback.mpv_args).arg(&pipes.stream);

    let playing = AtomicBool::new(true);
    let result = thread::scope(|scope| {
        // A track that dies halfway leaves its pipe open at the far end for as long as
        // ffmpeg lives, and mpv then buffers forever for a stream that has stopped.
        // Ending the mux instead turns that into an ordinary end of file.
        scope.spawn(|| {
            while playing.load(Ordering::Relaxed) {
                if pipes.failed() {
                    let _ = ffmpeg.lock().expect("ffmpeg poisoned").kill();
                    return;
                }
                thread::sleep(FAILURE_POLL_INTERVAL);
            }
        });
        // Ends with mpv, the same way: `playing` is what says the episode is over, and
        // both of these threads are inside the scope that joins them before it returns.
        if let Some(report) = playback.playhead {
            scope.spawn(|| follow(&control, &playing, report));
        }
        let result = match mpv.spawn() {
            Ok(mut mpv) => mpv.wait().context("wait for mpv"),
            Err(error) => Err(error).context("start mpv; is it installed and in PATH?"),
        };
        playing.store(false, Ordering::Relaxed);
        result
    });
    // Whatever happened, tear ffmpeg down so the threads feeding it see their pipes close.
    let mut ffmpeg = ffmpeg.into_inner().expect("ffmpeg poisoned");
    let _ = ffmpeg.kill();
    let _ = ffmpeg.wait();

    let status = result?;
    if !status.success() {
        let complaint = std::fs::read_to_string(log.path()).unwrap_or_default();
        let complaint = complaint.trim();
        if complaint.is_empty() {
            bail!("mpv exited with {status}");
        }
        bail!("mpv exited with {status}; ffmpeg said: {complaint}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    use super::*;

    fn track(file: PathBuf) -> MediaTrack {
        MediaTrack::media(file, String::new(), None)
    }

    #[test]
    fn waits_for_every_slot() {
        let pipes = LivePipes::new(2).unwrap();
        let first = pipes.track(0).unwrap();
        pipes.publish(0, track(first.clone()));
        thread::scope(|scope| {
            scope.spawn(|| {
                let second = pipes.track(1).unwrap();
                pipes.publish(1, track(second));
            });
            let tracks = pipes.wait().expect("both slots publish");
            assert_eq!(tracks.len(), 2);
            assert_eq!(tracks[0].file, first);
        });
    }

    #[test]
    fn a_failed_track_wakes_the_waiter() {
        let pipes = LivePipes::new(2).unwrap();
        pipes.publish(0, track(pipes.track(0).unwrap()));
        thread::scope(|scope| {
            scope.spawn(|| pipes.fail());
            assert!(pipes.wait().is_none());
        });
    }

    #[test]
    fn a_writer_hands_over_once_a_reader_arrives() {
        let pipes = LivePipes::new(1).unwrap();
        let path = pipes.track(0).unwrap();
        let (sender, received) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut pipe = pipes.open_writer(&path).unwrap().expect("a reader arrives");
                pipe.write_all(b"segment").unwrap();
            });
            // Stands in for ffmpeg opening the input it was handed.
            let reader = File::open(&path).unwrap();
            sender.send(io::read_to_string(reader).unwrap()).unwrap();
        });
        assert_eq!(received.recv().unwrap(), "segment");
    }

    #[test]
    fn abandoning_frees_a_writer_that_no_reader_will_ever_open() {
        let pipes = LivePipes::new(1).unwrap();
        let path = pipes.track(0).unwrap();
        thread::scope(|scope| {
            // Abandons before the writer even starts polling, which is the race a
            // blocking open loses: the writer must still come back rather than park.
            pipes.abandon();
            let writer = scope.spawn(|| pipes.open_writer(&path).unwrap());
            assert!(writer.join().unwrap().is_none());
        });
    }

    #[test]
    fn a_writer_that_closes_reaches_the_reader_as_end_of_file() {
        let pipes = LivePipes::new(1).unwrap();
        let path = pipes.track(0).unwrap();
        thread::scope(|scope| {
            let writer = scope.spawn(|| pipes.open_writer(&path).unwrap().expect("a reader"));
            // Stands in for ffmpeg opening the input it was handed.
            let mut reader = File::open(&path).unwrap();
            let pipe = writer.join().unwrap();
            // Stands in for a player started once the writers already hold their pipes:
            // if the write end is inheritable it lives on here and hides the end of the
            // track from ffmpeg, which then waits for bytes that never come.
            let mut player = Command::new("sleep").arg("30").spawn().unwrap();
            drop(pipe);
            let (sender, read) = mpsc::channel();
            thread::spawn(move || {
                let mut byte = [0_u8; 1];
                let _ = sender.send(reader.read(&mut byte).unwrap());
            });
            let outcome = read.recv_timeout(Duration::from_secs(5));
            let _ = player.kill();
            let _ = player.wait();
            assert_eq!(outcome.ok(), Some(0), "the write end outlived its owner");
        });
    }

    /// Where an episode picks up decides what the viewer sees first, so both ends of it
    /// have to be exceptions: a position in the opening seconds is not a place anyone
    /// wants sent back to, and one at the end is a finished episode rather than a place
    /// at all. A running time Crunchyroll did not send is the common case for a single
    /// episode and must not take the resume away with it.
    #[test]
    fn an_episode_resumes_only_where_there_is_somewhere_to_resume() {
        // Twenty-four minutes and twenty-one seconds, as a season listing sends it.
        let episode = 1_461_000;
        assert_eq!(resume_at(0, episode, false), None, "nobody has opened it");
        assert_eq!(resume_at(12, episode, false), None, "the opening titles");
        assert_eq!(resume_at(30, episode, false), None, "the slack itself");
        assert_eq!(resume_at(31, episode, false), Some(31));
        assert_eq!(resume_at(842, episode, false), Some(842));
        assert_eq!(
            resume_at(1430, episode, false),
            Some(1430),
            "the last minute"
        );
        assert_eq!(
            resume_at(1431, episode, false),
            None,
            "thirty seconds from the end is an episode that has been watched"
        );
        assert_eq!(resume_at(2000, episode, false), None, "past the end of it");
        assert_eq!(
            resume_at(842, episode, true),
            None,
            "an episode Crunchyroll calls watched is watched, wherever the position is"
        );

        // No running time at all: the playhead is all there is to go on, and it came
        // from a player that knew how long the episode was.
        assert_eq!(resume_at(842, 0, false), Some(842));
        assert_eq!(resume_at(12, 0, false), None, "still the opening titles");
        assert_eq!(resume_at(842, 0, true), None);

        // A duration too large for the seconds to fit in a u32 is nonsense from
        // somewhere, and nonsense must not become an arithmetic overflow.
        assert_eq!(resume_at(u32::MAX, u64::MAX, false), None);
    }

    /// Only an answer that carries a position is a position. mpv publishes events on the
    /// same socket, and until playback has started it answers the question with a
    /// failure - reporting either of those to Crunchyroll would move the account's
    /// playhead to somewhere nobody watched.
    #[test]
    fn only_an_answer_that_carries_a_position_counts() {
        assert_eq!(
            time_pos(r#"{"data":842.123,"error":"success"}"#),
            Some(842.123)
        );
        assert_eq!(time_pos(r#"{"data":0,"error":"success"}"#), Some(0.0));
        for line in [
            r#"{"error":"property unavailable"}"#,
            r#"{"data":null,"error":"success"}"#,
            r#"{"event":"playback-restart"}"#,
            r#"{"event":"property-change","name":"time-pos","data":12.5}"#,
            "not json at all",
            "",
            "   ",
        ] {
            assert_eq!(time_pos(line), None, "{line:?}");
        }
    }

    /// The reader has to work against mpv as mpv actually behaves: it binds its socket
    /// some way into its own startup, it publishes events on the same connection whether
    /// or not anyone asked, and it answers a position it does not have yet with a
    /// failure. Everything here is what would otherwise be found out against a live
    /// player, where a wrong answer is a playhead moved to the wrong place.
    #[test]
    fn the_position_is_read_off_a_socket_that_talks_like_mpv() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let socket = directory.path().join("mpv.sock");
        let bind_at = socket.clone();
        let server = thread::spawn(move || {
            // mpv has not bound its socket the instant it is spawned.
            thread::sleep(Duration::from_millis(120));
            let listener = UnixListener::bind(&bind_at).expect("bind the test socket");
            let (stream, _) = listener.accept().expect("a connection from the reader");
            let mut lines = BufReader::new(&stream);
            let mut writer = &stream;
            let mut command = String::new();
            lines.read_line(&mut command).expect("a command");
            assert!(command.contains("time-pos"), "{command:?}");
            // An event first, the way mpv announces a file it has just opened.
            writer
                .write_all(
                    b"{\"event\":\"playback-restart\"}\n{\"data\":842.7,\"error\":\"success\"}\n",
                )
                .expect("answer the first command");
            command.clear();
            lines.read_line(&mut command).expect("a second command");
            writer
                .write_all(b"{\"error\":\"property unavailable\"}\n")
                .expect("answer the second command");
            // And then it quits, which is what takes the socket away.
        });

        let playing = AtomicBool::new(true);
        let stream = connect(&socket, &playing).expect("mpv binds its socket in the end");
        let mut stream = BufReader::new(stream);
        assert_eq!(
            position(&mut stream),
            Some(842),
            "an event line is not an answer"
        );
        assert_eq!(
            position(&mut stream),
            None,
            "a property mpv has not got is not a position"
        );
        server.join().expect("the test server");

        // And a socket nobody is ever going to bind is given up on the moment playback
        // is over, rather than held onto for the whole of the wait.
        let over = AtomicBool::new(false);
        assert!(connect(&directory.path().join("nobody.sock"), &over).is_none());
    }
}
