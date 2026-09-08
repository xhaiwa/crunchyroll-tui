use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::Duration;

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
    mpv_args: &[String],
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
    ])
    .args(mpv_args)
    .arg(&pipes.stream);

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
}
