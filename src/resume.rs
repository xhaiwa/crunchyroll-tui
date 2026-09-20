use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// What the format of the state file is at. A file written by a version of this program
/// that counted differently is not read and not guessed at: every file it names is swept
/// up and the episode starts again, which costs a download and never an MKV that is
/// quietly missing half its audio.
const STATE_VERSION: u32 = 1;

/// Where the mux writes while it is still writing.
///
/// The finished name is the one the "already downloaded, skipping" check looks for and
/// the one a media library indexes, so nothing may wear it until it is a whole episode.
/// The suffix goes on the end of the whole name rather than replacing the extension -
/// `... [1080p].mkv.part` rather than `... [1080p].part` - so that the finished name is
/// still legible in it, so that the file sorts next to the episode it will become, and
/// so that the two files an unfinished download leaves under a name of its own share one
/// prefix and can be thrown away together.
pub fn part_path(output: &Path) -> PathBuf {
    suffixed(output, ".part")
}

/// Where the record of what has already been fetched lives: beside the `.part` it
/// belongs to, under the same prefix, for the same reasons.
pub fn state_path(output: &Path) -> PathBuf {
    suffixed(output, ".part.json")
}

/// Appends to a path's file name rather than to its text. `format!("{}", path.display())`
/// is shorter and is what the rest of this file used to do, but it goes through a lossy
/// conversion, so a name this program never chose - a series title from a file system
/// that is not UTF-8 - would come back subtly different and point at nothing.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// What this run has been asked for, and what a state file has to agree with before
/// anything it names is touched.
///
/// Everything here changes what the bytes on disk are, which is why all of it has to
/// match and not just the episode: an audio track fetched at 192k is not the one a run
/// asking for 128k wants, and the order of the audio locales decides which of them the
/// MKV marks as default. Reusing a track across any of these differences hands back a
/// file that looks finished and is not what was asked for, which is the one outcome
/// worth refusing a perfectly good half-hour of downloading over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub episode_id: String,
    pub video_quality: String,
    pub audio_quality: String,
    pub audio_locales: Vec<String>,
    pub subtitle_locales: Vec<String>,
    pub caption_locales: Vec<String>,
}

/// One of the files an episode is muxed from, named by what it is rather than by where
/// it landed: the paths are temporary names that change from run to run, and this is
/// what two runs have in common.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "track", rename_all = "kebab-case")]
pub enum Track {
    Video,
    Audio { locale: String },
    Subtitles { locale: String, cc: bool },
}

/// A track that came out whole and is waiting for the mux.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finished {
    pub track: Track,
    pub file: PathBuf,
    /// How long the file was when it was written. A file that has since changed length
    /// is not the one this run made, so it is thrown away rather than muxed. Only the
    /// length is checked: a scratch file that something has rewritten to exactly its old
    /// size is past what a downloader can reasonably defend itself against, and the
    /// cheap check catches every case that actually happens - a copy that was cut off, a
    /// disk that filled up, a file half deleted by hand.
    pub bytes: u64,
}

/// A track that was still arriving when the run stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unfinished {
    pub track: Track,
    /// The encrypted buffer, which is where the bytes that did arrive are.
    pub buffer: PathBuf,
    /// Where the decrypted track was going. Recorded so that the empty file the first
    /// run made is written to rather than left behind beside a second one.
    pub file: PathBuf,
    /// How to carry the buffer on, for a track that can be carried on at all.
    pub pick_up: Option<PickUp>,
}

/// What the head of a buffer is, so that a later run can tell where the body picks up.
///
/// `init` is how many bytes of initialization segment sit in front of the body and
/// `index` is the byte of the remote file the body starts at, so the buffer holds
/// `index + (length - init)` bytes' worth of that file. Both are read out of the
/// manifest again on every run, so a manifest that now says something else is a manifest
/// describing a different file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickUp {
    pub init: u64,
    pub index: u64,
}

/// Everything a run leaves behind for the next one, as it is written to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub run: Run,
    pub finished: Vec<Finished>,
    pub unfinished: Vec<Unfinished>,
}

impl State {
    fn empty(run: Run) -> Self {
        Self {
            version: STATE_VERSION,
            run,
            finished: Vec::new(),
            unfinished: Vec::new(),
        }
    }

    /// Every file the state names, which is also the list of what to sweep up when it
    /// turns out not to be worth keeping.
    fn files(&self) -> impl Iterator<Item = &Path> {
        self.finished
            .iter()
            .map(|entry| entry.file.as_path())
            .chain(
                self.unfinished
                    .iter()
                    .flat_map(|entry| [entry.buffer.as_path(), entry.file.as_path()]),
            )
    }
}

/// Whether `state` was written by a run asking for what this one is asking for.
///
/// Version first, because a file from another format may not mean what its fields say
/// it means even when they parse.
fn belongs_to(state: &State, run: &Run) -> bool {
    state.version == STATE_VERSION && state.run == *run
}

/// The byte of the remote file a buffer carries on from, or `None` when it cannot be
/// trusted to be a buffer of this track at all.
///
/// `recorded` is what the run that wrote the buffer said its head was and `now` is what
/// this run's manifest and initialization segment say; a difference between them means
/// the two runs are looking at different files, whatever the name on disk suggests.
/// `total` is the whole remote file's length, which is what keeps a buffer from being
/// carried on past the end of the thing it is a buffer of: a request for a byte beyond
/// it comes back 416, which is a status no amount of retrying softens.
///
/// A buffer that reaches exactly `total` is not an error but a run killed between the
/// last byte and the decryption: there is nothing left to fetch, and the caller is
/// expected to notice that the answer is `total` and go straight to decrypting.
fn carry_on(recorded: &PickUp, now: &PickUp, buffered: u64, total: u64) -> Option<u64> {
    if recorded != now || buffered < now.init {
        return None;
    }
    let from = now.index + (buffered - now.init);
    (from <= total).then_some(from)
}

/// A buffer that is worth carrying on, and where to carry it on from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedUp {
    pub buffer: PathBuf,
    pub file: PathBuf,
    /// The byte of the remote file to ask for next.
    pub from: u64,
    /// How much of the buffer is already on disk, so that a failed pick-up can be told
    /// apart from a connection that died after it had delivered something.
    pub buffered: u64,
    /// Whether the body is already whole and only the decryption is left.
    pub complete: bool,
}

/// The state file beside one episode's output, and the writing of it.
///
/// It is written at the few moments where what is on disk changes - a track starting, a
/// track arriving - rather than as the bytes flow, because everything that changes in
/// between is already written down by the buffer's own length. That is what keeps a
/// segmented track out of the picture: how many whole segments reached the disk is not
/// something a file length can be read for, so a segmented buffer is recorded only so
/// that the next run knows to sweep it up.
pub struct Resume {
    path: PathBuf,
    state: Mutex<State>,
}

impl Resume {
    /// Picks up the state beside `output` when it belongs to this run, and starts an
    /// empty one when it does not.
    ///
    /// A state that does not belong is not merely ignored: every file it names is
    /// deleted first, because the state file is the only record that those files exist
    /// at all and dropping it without them would leave gigabytes of scratch in the
    /// user's series directory under names nothing points at any more. The same goes for
    /// a state that does belong but names a file that has gone missing or changed size,
    /// and for a buffer that cannot be carried on - it is swept for the same reason it
    /// is useless.
    ///
    /// Nothing here is an error. Every path out of it ends in a run that downloads the
    /// episode, and the worst a damaged state file can cost is the time it would have
    /// saved.
    pub fn open(output: &Path, run: Run) -> Self {
        let path = state_path(output);
        let state = fs::read(&path)
            .ok()
            .and_then(|body| serde_json::from_slice::<State>(&body).ok())
            .map_or_else(
                || State::empty(run.clone()),
                |state| {
                    if belongs_to(&state, &run) {
                        prune(state)
                    } else {
                        for file in state.files() {
                            let _ = fs::remove_file(file);
                        }
                        State::empty(run.clone())
                    }
                },
            );
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    /// The file a previous run already finished this track into, if it left one.
    pub fn finished(&self, track: &Track) -> Option<PathBuf> {
        self.with(|state| {
            state
                .finished
                .iter()
                .find(|entry| entry.track == *track)
                .map(|entry| entry.file.clone())
        })
    }

    /// Whether a previous run left a buffer for this track at all, which is what decides
    /// whether measuring the remote file is worth a request.
    pub fn has_buffer(&self, track: &Track) -> bool {
        self.with(|state| state.unfinished.iter().any(|entry| entry.track == *track))
    }

    /// Whether the buffer a previous run left for this track is worth carrying on, and
    /// from where.
    ///
    /// `init` is the initialization segment this run has just fetched, and the first
    /// bytes of the buffer have to be exactly it. That is the strong check of the three:
    /// the initialization segment carries the codec configuration and the key id, so two
    /// files that start with the same one are the same encode of the same content, which
    /// is the only thing that makes appending to somebody else's bytes safe. `total` is
    /// how long the whole remote file is now, and `None` - a server that would not say -
    /// is a refusal rather than a shrug.
    ///
    /// A buffer that fails any of it is deleted here rather than left to be wondered
    /// about, since the caller is about to fetch the track from the top and would
    /// otherwise write it to a second file beside the first.
    pub fn pick_up(
        &self,
        track: &Track,
        init: &[u8],
        index: u64,
        total: Option<u64>,
    ) -> Option<PickedUp> {
        let entry = self.with(|state| {
            state
                .unfinished
                .iter()
                .find(|entry| entry.track == *track)
                .cloned()
        })?;
        let now = PickUp {
            init: init.len() as u64,
            index,
        };
        let picked = (|| {
            let recorded = entry.pick_up?;
            let buffered = fs::metadata(&entry.buffer).ok()?.len();
            let total = total?;
            let from = carry_on(&recorded, &now, buffered, total)?;
            starts_with(&entry.buffer, init).then_some(PickedUp {
                buffer: entry.buffer.clone(),
                file: entry.file.clone(),
                from,
                buffered,
                complete: from == total,
            })
        })();
        if picked.is_none() {
            self.forget(track);
        }
        picked
    }

    /// Records that a track has started arriving into `buffer`, and where it is headed
    /// once it is decrypted. `pick_up` is what a later run needs to carry the buffer on,
    /// and is `None` for a segmented track, which cannot be carried on and is recorded
    /// only so that the buffer can be swept up.
    ///
    /// Anything this displaces is deleted on the way out. A track only starts twice when
    /// the first attempt at it turned out to be worth nothing - a manifest that has
    /// changed shape between runs, say - and the buffer of that attempt has just lost
    /// the one thing that knew where it was.
    pub fn starting(&self, track: &Track, buffer: PathBuf, file: PathBuf, pick_up: Option<PickUp>) {
        self.change(|state| {
            for displaced in state
                .unfinished
                .iter()
                .filter(|entry| entry.track == *track)
                .flat_map(|entry| [&entry.buffer, &entry.file])
                .filter(|path| **path != buffer && **path != file)
            {
                let _ = fs::remove_file(displaced);
            }
            state.unfinished.retain(|entry| entry.track != *track);
            state.unfinished.push(Unfinished {
                track: track.clone(),
                buffer,
                file,
                pick_up,
            });
        });
    }

    /// Records that a track came out whole, with the length that says later whether it
    /// is still the file this run wrote.
    pub fn arrived(&self, track: &Track, file: &Path) {
        let bytes = fs::metadata(file).map_or(0, |meta| meta.len());
        self.change(|state| {
            state.unfinished.retain(|entry| entry.track != *track);
            state.finished.retain(|entry| entry.track != *track);
            state.finished.push(Finished {
                track: track.clone(),
                file: file.to_path_buf(),
                bytes,
            });
        });
    }

    /// Throws away whatever is being kept for a track, files and all.
    pub fn forget(&self, track: &Track) {
        self.change(|state| {
            for entry in state
                .unfinished
                .iter()
                .filter(|entry| entry.track == *track)
            {
                let _ = fs::remove_file(&entry.buffer);
                let _ = fs::remove_file(&entry.file);
            }
            for entry in state.finished.iter().filter(|entry| entry.track == *track) {
                let _ = fs::remove_file(&entry.file);
            }
            state.unfinished.retain(|entry| entry.track != *track);
            state.finished.retain(|entry| entry.track != *track);
        });
    }

    /// Forgets the episode altogether, which is what a finished download does: the MKV
    /// is under its own name and there is nothing left for a later run to pick up. The
    /// entries go with the file so that nothing this run does afterwards can write them
    /// back out.
    pub fn clear(&self) {
        let mut state = self.state.lock().expect("resume state poisoned");
        state.finished.clear();
        state.unfinished.clear();
        let _ = fs::remove_file(&self.path);
    }

    fn with<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(&self.state.lock().expect("resume state poisoned"))
    }

    fn change(&self, edit: impl FnOnce(&mut State)) {
        let mut state = self.state.lock().expect("resume state poisoned");
        edit(&mut state);
        // Kept while the lock is held so that the file on disk is always one of the
        // states this run was actually in, rather than two writers' halves.
        save(&self.path, &state);
    }
}

/// Writes the state through a file beside it rather than over the top of it.
///
/// A write that is interrupted partway leaves JSON that will not parse, and a state file
/// that will not parse is a state file whose every track is thrown away - the exact half
/// hour of downloading this is all here to save. Renaming a whole file into place cannot
/// land halfway, so the worst an interrupted save costs is the last track it was about
/// to record.
///
/// Failing to save is not reported. The download is what the user asked for and it is
/// still running; all a lost state file means is that a run that is killed later starts
/// the episode again, which is what would have happened anyway.
fn save(path: &Path, state: &State) {
    let Ok(body) = serde_json::to_vec_pretty(state) else {
        return;
    };
    let pending = suffixed(path, ".new");
    if fs::write(&pending, &body).is_ok() && fs::rename(&pending, path).is_err() {
        let _ = fs::remove_file(&pending);
    }
}

/// Drops the entries whose files are no longer what they were said to be, deleting what
/// they named on the way out.
fn prune(mut state: State) -> State {
    state.finished.retain(|entry| {
        let kept = fs::metadata(&entry.file).is_ok_and(|meta| meta.len() == entry.bytes);
        if !kept {
            let _ = fs::remove_file(&entry.file);
        }
        kept
    });
    state.unfinished.retain(|entry| {
        // A buffer with nothing to say about where it picks up is a segmented track, and
        // the only use left for it is knowing which file to delete.
        let kept = entry.pick_up.is_some() && fs::metadata(&entry.buffer).is_ok();
        if !kept {
            let _ = fs::remove_file(&entry.buffer);
            let _ = fs::remove_file(&entry.file);
        }
        kept
    });
    state
}

/// Whether `path` begins with exactly `head`.
fn starts_with(path: &Path, head: &[u8]) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut found = vec![0_u8; head.len()];
    file.read_exact(&mut found).is_ok() && found == head
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Run {
        Run {
            episode_id: "GE00198973JAJP".into(),
            video_quality: "1080p".into(),
            audio_quality: "192k".into(),
            audio_locales: vec!["ja-JP".into(), "en-US".into()],
            subtitle_locales: vec!["en-US".into()],
            caption_locales: Vec::new(),
        }
    }

    fn state() -> State {
        State {
            version: STATE_VERSION,
            run: run(),
            finished: vec![Finished {
                track: Track::Video,
                file: PathBuf::from("Series/.crdl-video-abc.mp4"),
                bytes: 1_500_000_000,
            }],
            unfinished: vec![Unfinished {
                track: Track::Audio {
                    locale: "ja-JP".into(),
                },
                buffer: PathBuf::from("Series/.crdl-audio-def.mp4.enc"),
                file: PathBuf::from("Series/.crdl-audio-def.mp4"),
                pick_up: Some(PickUp {
                    init: 1024,
                    index: 4096,
                }),
            }],
        }
    }

    /// The state file is the only thing standing between a killed run and downloading
    /// the episode all over again, so every field of it has to survive the trip through
    /// JSON - including the enum that says which track an entry is about, which is the
    /// part serde has to be told how to tag.
    #[test]
    fn a_state_file_says_the_same_thing_when_it_is_read_back() {
        let written = serde_json::to_vec(&state()).expect("the state serialises");
        let read: State = serde_json::from_slice(&written).expect("the state parses");
        assert_eq!(read, state());
    }

    /// Every one of these changes what the bytes on disk are supposed to be, so every
    /// one of them has to stop a run from reusing them. Getting this wrong is what hands
    /// back an MKV that looks finished and is not the episode that was asked for.
    #[test]
    fn a_state_from_a_different_run_is_refused() {
        assert!(belongs_to(&state(), &run()));

        /// One way in which a later run can be asking for something else, and what to
        /// call it when it turns out to have been let through.
        type Difference = (&'static str, fn(&mut Run));

        let differences: [Difference; 7] = [
            ("another episode", |run| run.episode_id = "GX000000".into()),
            ("another video quality", |run| {
                run.video_quality = "720p".into();
            }),
            ("another audio quality", |run| {
                run.audio_quality = "128k".into();
            }),
            ("another audio locale", |run| {
                run.audio_locales = vec!["ja-JP".into(), "de-DE".into()];
            }),
            // The first audio locale is the one the MKV marks as default, so the order
            // is part of what was asked for and not an accident of the listing.
            ("the audio locales in another order", |run| {
                run.audio_locales.reverse();
            }),
            ("one subtitle locale fewer", |run| {
                run.subtitle_locales.clear();
            }),
            ("a closed caption that was not asked for before", |run| {
                run.caption_locales = vec!["en-US".into()];
            }),
        ];
        for (what, change) in differences {
            let mut asked = run();
            change(&mut asked);
            assert!(!belongs_to(&state(), &asked), "{what} was accepted");
        }

        // And a file from a format this program no longer writes is not guessed at.
        let mut older = state();
        older.version = STATE_VERSION - 1;
        assert!(!belongs_to(&older, &run()));
    }

    /// The rules that decide whether somebody else's bytes may be appended to. The
    /// dangerous answer is not "no" - that costs a download - but a "yes" that splices
    /// two different encodes of the same episode into one file, which ffmpeg will mux
    /// without complaint.
    #[test]
    fn a_buffer_is_only_carried_on_where_it_lines_up() {
        let recorded = PickUp {
            init: 1024,
            index: 4096,
        };
        // Two thousand bytes of body are on disk, so the next byte wanted is that far
        // past the start of the body.
        assert_eq!(carry_on(&recorded, &recorded, 3024, 9_000), Some(6096));
        // A buffer holding nothing but the initialization segment starts at the body.
        assert_eq!(carry_on(&recorded, &recorded, 1024, 9_000), Some(4096));
        // A whole body, which is a run killed before it could decrypt what it had.
        assert_eq!(carry_on(&recorded, &recorded, 5928, 9_000), Some(9_000));

        // A manifest that now describes the file differently describes a different file.
        let moved = PickUp {
            init: 1024,
            index: 8192,
        };
        assert_eq!(carry_on(&recorded, &moved, 3024, 9_000), None);
        let grown = PickUp {
            init: 2048,
            index: 4096,
        };
        assert_eq!(carry_on(&recorded, &grown, 3024, 9_000), None);
        // Too short to be holding the initialization segment it claims to.
        assert_eq!(carry_on(&recorded, &recorded, 512, 9_000), None);
        // And longer than the file it is supposed to be part of, which would have the
        // next request ask for a byte past the end and be answered 416 for ever.
        assert_eq!(carry_on(&recorded, &recorded, 6000, 9_000), None);
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("the temporary directory is writable");
    }

    /// A state that does not belong to this run takes its scratch files with it. It is
    /// the only record that they exist, so forgetting it on its own would leave several
    /// gigabytes in the user's series directory that nothing ever names again.
    #[test]
    fn a_refused_state_sweeps_up_after_itself() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let output = directory.path().join("Series S01E01 [1080p].mkv");
        let video = directory.path().join(".crdl-video-abc.mp4");
        let buffer = directory.path().join(".crdl-audio-def.mp4.enc");
        let audio = directory.path().join(".crdl-audio-def.mp4");
        write(&video, b"video");
        write(&buffer, b"audio");
        write(&audio, b"");

        let mut kept = state();
        kept.finished[0].file = video.clone();
        kept.finished[0].bytes = 5;
        kept.unfinished[0].buffer = buffer.clone();
        kept.unfinished[0].file = audio.clone();
        save(&state_path(&output), &kept);

        let mut asked = run();
        asked.video_quality = "720p".into();
        let resume = Resume::open(&output, asked);
        assert!(!video.exists(), "the finished track was left behind");
        assert!(!buffer.exists(), "the buffer was left behind");
        assert!(!audio.exists());
        assert_eq!(resume.finished(&Track::Video), None);
    }

    /// A track is only reused while the file behind it is the one that was written. A
    /// file that has since changed length is somebody else's, or half of one, and muxing
    /// it would produce an episode that is missing the end of a track.
    #[test]
    fn a_track_whose_file_has_changed_is_not_reused() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let output = directory.path().join("Series S01E01 [1080p].mkv");
        let video = directory.path().join(".crdl-video-abc.mp4");
        write(&video, b"video");

        let mut kept = state();
        kept.finished[0].file = video.clone();
        kept.finished[0].bytes = 5;
        kept.unfinished.clear();
        save(&state_path(&output), &kept);
        assert_eq!(
            Resume::open(&output, run()).finished(&Track::Video),
            Some(video.clone())
        );

        write(&video, b"video and then some");
        assert_eq!(Resume::open(&output, run()).finished(&Track::Video), None);
        assert!(!video.exists(), "the file that was refused was left behind");
    }

    /// A buffer is carried on only when it starts with exactly the initialization
    /// segment this run fetched. Two files that start with the same one are the same
    /// encode of the same content, which is the whole of what makes appending to bytes
    /// somebody else downloaded safe.
    #[test]
    fn a_buffer_from_another_encode_is_thrown_away() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let output = directory.path().join("Series S01E01 [1080p].mkv");
        let buffer = directory.path().join(".crdl-audio-def.mp4.enc");
        let audio = directory.path().join(".crdl-audio-def.mp4");
        let track = Track::Audio {
            locale: "ja-JP".into(),
        };
        let head = vec![7_u8; 1024];

        let mut kept = state();
        kept.finished.clear();
        kept.unfinished[0].buffer = buffer.clone();
        kept.unfinished[0].file = audio.clone();
        let mut body = head.clone();
        body.extend(std::iter::repeat_n(0_u8, 2000));
        write(&buffer, &body);
        write(&audio, b"");
        save(&state_path(&output), &kept);

        let resume = Resume::open(&output, run());
        let picked = resume
            .pick_up(&track, &head, 4096, Some(9_000))
            .expect("the buffer starts with this run's initialization segment");
        assert_eq!(picked.from, 4096 + 2000);
        assert_eq!(picked.buffered, 3024);
        assert!(!picked.complete);

        // The same buffer against an initialization segment it does not start with.
        let resume = Resume::open(&output, run());
        assert_eq!(
            resume.pick_up(&track, &vec![9_u8; 1024], 4096, Some(9_000)),
            None
        );
        assert!(!buffer.exists(), "the refused buffer was left behind");
        assert!(!audio.exists());
    }

    /// A finished download has nothing left to pick up, and an abandoned one keeps
    /// What the second run is left with: the tracks that arrived are handed back, the
    /// track that was still arriving is offered as a buffer to carry on, and the ones
    /// nothing was ever written down for are asked for again. Getting any of the three
    /// wrong either downloads something twice or muxes something that is not there.
    #[test]
    fn a_second_run_asks_only_for_what_is_missing() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let output = directory.path().join("Series S01E01 [1080p].mkv");
        let video = directory.path().join(".crdl-video-abc.mp4");
        let subtitles = directory.path().join(".crdl-subs-ghi.ass");
        let buffer = directory.path().join(".crdl-audio-def.mp4.enc");
        let audio = directory.path().join(".crdl-audio-def.mp4");
        let english = Track::Subtitles {
            locale: "en-US".into(),
            cc: false,
        };
        write(&video, b"video");
        write(&subtitles, b"subtitles");
        write(&buffer, &[0_u8; 3024]);
        write(&audio, b"");

        let first = Resume::open(&output, run());
        first.arrived(&Track::Video, &video);
        first.arrived(&english, &subtitles);
        first.starting(
            &Track::Audio {
                locale: "ja-JP".into(),
            },
            buffer.clone(),
            audio.clone(),
            Some(PickUp {
                init: 1024,
                index: 4096,
            }),
        );

        let second = Resume::open(&output, run());
        assert_eq!(second.finished(&Track::Video), Some(video));
        assert_eq!(second.finished(&english), Some(subtitles));
        assert!(second.has_buffer(&Track::Audio {
            locale: "ja-JP".into()
        }));
        // The second audio locale was never started, so there is nothing to offer for
        // it and nothing to measure a remote file against.
        let untouched = Track::Audio {
            locale: "en-US".into(),
        };
        assert_eq!(second.finished(&untouched), None);
        assert!(!second.has_buffer(&untouched));
    }

    /// A finished download has nothing left to pick up, and an abandoned one keeps
    /// everything it has - which is the whole point of the file.
    #[test]
    fn a_finished_episode_clears_its_state() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let output = directory.path().join("Series S01E01 [1080p].mkv");
        let video = directory.path().join(".crdl-video-abc.mp4");
        write(&video, b"video");

        let resume = Resume::open(&output, run());
        resume.arrived(&Track::Video, &video);
        assert!(state_path(&output).exists());
        assert_eq!(
            Resume::open(&output, run()).finished(&Track::Video),
            Some(video.clone())
        );

        resume.clear();
        assert!(!state_path(&output).exists());
        assert_eq!(Resume::open(&output, run()).finished(&Track::Video), None);
    }
}
