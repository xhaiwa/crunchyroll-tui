use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;
use ratatui::widgets::ListState;

use crate::download::DownloadOptions;
use crate::model::{CatalogItem, Playhead, Season, SeasonEpisode};
use crate::util::{LANGUAGES, language_name};

use super::QUALITIES;
use super::art::Gallery;
use super::keys::{Bindings, Command};
use super::mouse::{self, Regions, Target};
use super::theme::Theme;
use super::worker::{Listing, Queued, Request, Response, Update, Worker};

/// Which column the keyboard is pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Series,
    Seasons,
    Episodes,
    /// The queue, which is a list like the other three and is worked like one.
    Downloads,
}

/// What the event loop has to leave the interface to do, because it needs the terminal
/// back: mpv draws over it.
pub enum Action {
    None,
    Quit,
    /// Played one after another, so quitting mpv moves on to the next episode.
    Play(Vec<SeasonEpisode>),
}

/// Where one episode in the queue has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Waiting its turn. One episode is downloaded at a time.
    Queued,
    Running,
    Done,
    /// Given up on, with the reason, which is the only account of it anyone will get:
    /// the download thread has no terminal to print one to.
    Failed(String),
}

/// One row of the queue.
///
/// What it is, rather than which episode it is: the row has to keep reading as itself
/// long after the episode column has moved on to another season, so it carries its own
/// words rather than an index into a list that will not hold still.
pub struct Download {
    /// The number the request was queued under, which is how an answer finds this row
    /// again. Titles repeat and numbers do not.
    pub id: usize,
    /// `S01E01`, the way the episode column numbers it.
    pub number: String,
    pub title: String,
    pub series: String,
    pub state: State,
    /// Each part of the episode the downloader has mentioned - subtitles, video, one
    /// audio track per locale, muxing - in the order it first mentioned them, with how
    /// far each has got. A total of zero is a part whose size nothing knows.
    pub stages: Vec<(String, u64, u64)>,
}

impl Download {
    /// What the row says about a download that is running: the part of the episode it
    /// is still waiting on, and how far that part has got, where anything knows.
    ///
    /// The first unfinished part rather than the one that spoke last. The video and the
    /// first audio track come down side by side, and a row showing whichever of them
    /// reported most recently would jump between two percentages that have nothing to
    /// do with each other. Taking them in the order they were first heard from means
    /// the bar crosses one part, then the next, and only ever forwards.
    pub fn stage(&self) -> Option<(&str, Option<f64>)> {
        self.stages
            .iter()
            .find(|(_, done, total)| *total == 0 || done < total)
            .map(|(stage, done, total)| {
                let fraction = (*total > 0).then(|| *done as f64 / *total as f64);
                (stage.as_str(), fraction)
            })
    }

    /// Takes in one answer about this download.
    fn update(&mut self, update: Update) {
        match update {
            Update::Started => self.state = State::Running,
            Update::Stage { stage, done, total } => {
                // Found by name rather than appended, so a part that reports a thousand
                // times is one row of this and not a thousand.
                match self.stages.iter_mut().find(|(named, ..)| *named == stage) {
                    Some(known) => *known = (stage, done, total),
                    None => self.stages.push((stage, done, total)),
                }
            }
            Update::Finished(Ok(())) => {
                self.state = State::Done;
                // Nothing is waiting on anything any more, and a part left at ninety
                // per cent under a row that says `done` reads as a contradiction.
                self.stages.clear();
            }
            Update::Finished(Err(error)) => self.state = State::Failed(error),
        }
    }

    /// Whether the thread is inside this one now. It has temporary files open and a
    /// playback session held at Crunchyroll, so it is the one row that cannot be
    /// dropped and the one reason quitting asks twice.
    pub const fn running(&self) -> bool {
        matches!(self.state, State::Running)
    }

    /// Whether this row may be taken out of the queue. Anything but the download in
    /// flight: that one is an hour of segments on a thread of its own and there is no
    /// calling it back, so it stays until it is over.
    pub const fn droppable(&self) -> bool {
        !self.running()
    }
}

pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// The playhead that counts an episode as watched, in the whole seconds the endpoint
/// takes.
///
/// Crunchyroll gives a running time in milliseconds, and the fraction of a second on the
/// end of it is neither here nor there - an episode whose playhead is at its last whole
/// second has been watched by any reading. A running time far too large to be one is
/// held at the largest second there is rather than wrapping round to an early one, since
/// a playhead near the start is the one answer that would be wrong.
fn whole_seconds(duration_ms: u64) -> u32 {
    u32::try_from(duration_ms / 1000).unwrap_or(u32::MAX)
}

/// The number Crunchyroll prints on an episode, which is "SP" or "1.5" as often as it
/// is a number. The numeric field has nothing useful to say about either, so it is only
/// the fallback.
fn episode_number(episode: &SeasonEpisode) -> String {
    if episode.episode.is_empty() {
        episode.episode_number.to_string()
    } else {
        episode.episode.clone()
    }
}

/// What a notice calls an episode: the number above, behind the E the episode column
/// shows. The title would be truer to the episode, but it is the number the user just
/// moved the cursor onto, and a sentence on the status line has no room for both.
fn episode_label(episode: &SeasonEpisode) -> String {
    format!("E{}", episode_number(episode))
}

/// One column: what it holds, where the cursor is, and whether it is still waiting.
pub struct Pane<T> {
    pub items: Vec<T>,
    pub state: ListState,
    pub loading: bool,
    pub error: Option<String>,
    /// The id these items belong to. A slow answer for a series the user has already
    /// moved away from arrives with the wrong owner and is dropped.
    pub owner: String,
    /// Where to put the cursor when the next answer arrives. A reload, or a list asked
    /// for again in another language, is still the same list to the user, so the cursor
    /// has no business going back to the top.
    pub pending_cursor: Option<usize>,
}

impl<T> Default for Pane<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            state: ListState::default(),
            loading: false,
            error: None,
            owner: String::new(),
            pending_cursor: None,
        }
    }
}

impl<T> Pane<T> {
    pub fn selected(&self) -> Option<&T> {
        self.state
            .selected()
            .and_then(|index| self.items.get(index))
    }

    pub fn set(&mut self, items: Vec<T>) {
        let restored = self
            .pending_cursor
            .take()
            .filter(|index| *index < items.len());
        self.state
            .select(restored.or_else(|| (!items.is_empty()).then_some(0)));
        self.items = items;
        self.loading = false;
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.state.select(None);
        self.loading = false;
        self.error = None;
        self.owner.clear();
        self.pending_cursor = None;
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() as isize - 1;
        let current = self.state.selected().unwrap_or(0) as isize;
        self.state
            .select(Some(current.saturating_add(delta).clamp(0, last) as usize));
    }

    pub fn select_edge(&mut self, last: bool) {
        if self.items.is_empty() {
            return;
        }
        self.state
            .select(Some(if last { self.items.len() - 1 } else { 0 }));
    }

    /// Puts the cursor on `index`, and ignores one past the end: a list answered again
    /// since the frame a click was aimed at may be shorter than that frame said it was.
    pub fn select(&mut self, index: usize) {
        if index < self.items.len() {
            self.state.select(Some(index));
        }
    }

    /// The first row the list drew and how many items it holds - everything the pointer
    /// needs to turn a row of the screen into an index. The offset is what the list
    /// widget wrote back as it drew, so it is where the list actually was rather than
    /// where it was asked to be.
    pub fn window(&self) -> (usize, usize) {
        (self.state.offset(), self.items.len())
    }
}

/// The language list while it is open: which of the two settings it is choosing for,
/// and the locales it is offering.
pub struct Picker {
    pub audio: bool,
    pub pane: Pane<String>,
}

impl Picker {
    pub fn title(&self) -> &'static str {
        if self.audio {
            " Audio language "
        } else {
            " Subtitle language "
        }
    }
}

pub struct App {
    worker: Worker,
    pub options: DownloadOptions,
    /// The colours everything is drawn in.
    pub theme: Theme,
    /// Which key does what, so the help popup and the reminder along the bottom edge can
    /// say what this particular config file asked for rather than what vim would.
    pub keys: Bindings,
    /// The posters and episode stills, and the terminal's ability to draw them.
    pub art: Gallery,
    pub focus: Focus,
    pub series: Pane<CatalogItem>,
    pub seasons: Pane<Season>,
    pub episodes: Pane<SeasonEpisode>,
    /// The queue, oldest first. A pane like the other three, so the cursor, the wheel
    /// and the arithmetic that turns a row of the screen into an index are the ones the
    /// columns already use.
    pub downloads: Pane<Download>,
    /// The number the next download is queued under. It only ever goes up, so a row
    /// dropped and another queued in its place cannot be taken for it by an answer that
    /// was already on its way.
    next_download: usize,
    /// How far the account has got into each episode of the open season, by content id.
    /// It arrives after the episodes do and belongs to them, so it is emptied whenever
    /// they are: a marker held over from the last season would be painted onto whichever
    /// episode of this one happened to share an id, which is none of them.
    pub playheads: HashMap<String, Playhead>,
    pub listing: Listing,
    /// The search box while it is being typed into.
    pub editing: Option<String>,
    /// The language list while it is open.
    pub picker: Option<Picker>,
    pub notice: Option<Notice>,
    /// What the API client would have printed had the interface not owned the screen.
    notices: Arc<Mutex<Vec<String>>>,
    /// Where the last frame put everything, so a click can be aimed at it.
    pub regions: Regions,
    pub show_help: bool,
    pub quit: bool,
    pub tick: usize,
    /// Which browse order to come back to. The ring carries it along as it passes each
    /// one, so leaving a search - which is not on the ring - returns to the order that
    /// was last in use rather than to the top of the catalogue.
    sort: usize,
    /// Whether the opening list has already given up on the history.
    ///
    /// The interface opens on Continue watching and falls back to the catalogue when
    /// that comes back empty or fails, which is the first screen settling on something
    /// worth looking at rather than a rule about the list. Asking for the history again
    /// afterwards is a deliberate thing to do, and it then shows what it has - which is
    /// also what stops the fallback from being something that could fire twice.
    gave_up_on_history: bool,
    /// Whether quit was the last thing asked for. Leaving takes the download thread
    /// with it, so quit asks twice while an episode is in flight - see [`App::run`].
    leaving: bool,
}

impl App {
    pub fn new(
        worker: Worker,
        options: DownloadOptions,
        theme: Theme,
        keys: Bindings,
        notices: Arc<Mutex<Vec<String>>>,
        art: Gallery,
    ) -> Self {
        let mut app = Self {
            worker,
            options,
            theme,
            keys,
            art,
            focus: Focus::Series,
            series: Pane::default(),
            seasons: Pane::default(),
            episodes: Pane::default(),
            downloads: Pane::default(),
            next_download: 0,
            playheads: HashMap::new(),
            // The most useful first screen a video client has is the thing that was
            // being watched last, so that is what the interface opens on. An account
            // with no history, or a request that fails, falls back to the catalogue
            // when the answer arrives.
            listing: Listing::History,
            editing: None,
            picker: None,
            notice: None,
            notices,
            regions: Regions::default(),
            show_help: false,
            quit: false,
            tick: 0,
            sort: 0,
            gave_up_on_history: false,
            leaving: false,
        };
        app.request_catalog();
        app
    }

    pub fn audio(&self) -> String {
        self.options
            .audio_langs
            .first()
            .cloned()
            .unwrap_or_else(|| "ja-JP".to_owned())
    }

    pub fn subs(&self) -> String {
        self.options
            .subtitles_langs
            .first()
            .cloned()
            .unwrap_or_else(|| "en-US".to_owned())
    }

    fn say(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: false,
        });
    }

    /// Forgets where the last frame put everything. A resize, or anything that drew over
    /// the screen while the interface was handed away, leaves those boxes describing a
    /// picture that is gone, and a click hit against them would land somewhere arbitrary
    /// - so nothing may be clicked until the next frame has put them back.
    pub fn forget_layout(&mut self) {
        self.regions = Regions::default();
    }

    pub fn complain(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: true,
        });
    }

    /// What the interface has asked the worker for since this was last called. A command
    /// that only sends a request changes nothing a test can look at, so which item it
    /// picked has to be read off the request itself.
    #[cfg(test)]
    pub fn sent(&self) -> Vec<Request> {
        self.worker.sent()
    }

    /// Empties the episodes column and everything drawn alongside it.
    fn clear_episodes(&mut self) {
        self.episodes.clear();
        self.playheads.clear();
    }

    fn request_catalog(&mut self) {
        self.series.clear();
        self.seasons.clear();
        self.clear_episodes();
        self.series.loading = true;
        self.focus = Focus::Series;
        self.worker.send(Request::Catalog(self.listing.clone()));
    }

    fn request_seasons(&mut self) {
        let Some(series) = self.series.selected() else {
            return;
        };
        let series_id = series.id.clone();
        self.seasons.clear();
        self.clear_episodes();
        self.seasons.loading = true;
        self.seasons.owner = series_id.clone();
        self.focus = Focus::Seasons;
        self.worker.send(Request::Seasons {
            series_id,
            audio: self.audio(),
            subs: self.subs(),
        });
    }

    fn request_episodes(&mut self) {
        let Some(season) = self.seasons.selected() else {
            return;
        };
        let season_id = season.id.clone();
        self.clear_episodes();
        self.episodes.loading = true;
        self.episodes.owner = season_id.clone();
        self.focus = Focus::Episodes;
        self.worker.send(Request::Episodes {
            season_id,
            audio: self.audio(),
            subs: self.subs(),
        });
    }

    /// A catalogue answer, and the one decision the opening screen still has to make.
    ///
    /// The interface asks for Continue watching first, which is the best thing to open
    /// on right up until the account has never watched anything, or the request fails:
    /// an empty column, or an error where the catalogue should be, is a worse first
    /// screen than Popular. So this is where that is noticed - the only place the answer
    /// is known - and the catalogue is asked for instead, once, with the status line
    /// saying so, since a column showing a different list from the one that was asked
    /// for has no business doing it quietly.
    ///
    /// Split out of [`App::drain`] so a test can hand the interface an answer without a
    /// worker behind it.
    fn catalog_arrived(&mut self, listing: Listing, result: Result<Vec<CatalogItem>, String>) {
        if listing != self.listing {
            return;
        }
        let nothing_to_show = result.as_ref().is_ok_and(Vec::is_empty) || result.is_err();
        if listing == Listing::History && nothing_to_show && !self.gave_up_on_history {
            self.gave_up_on_history = true;
            self.sort = 0;
            self.listing = Listing::Browse(self.sort);
            self.request_catalog();
            let instead = self.listing.label();
            self.say(match result {
                Ok(_) => format!("Nothing watched yet - showing {instead}."),
                Err(error) => format!("Continue watching failed ({error}) - showing {instead}."),
            });
            return;
        }
        match result {
            Ok(items) => {
                let count = items.len();
                self.series.set(items);
                if count == 0 {
                    self.say("No series found.");
                }
            }
            Err(error) => {
                self.series.loading = false;
                self.series.error = Some(error.clone());
                self.complain(error);
            }
        }
    }

    /// Takes in whatever the worker has finished, and whatever the client wanted to say.
    pub fn drain(&mut self) {
        while let Some(response) = self.worker.try_recv() {
            self.accept(response);
        }
        self.art.drain();
        let pending: Vec<String> = self
            .notices
            .lock()
            .expect("notices poisoned")
            .drain(..)
            .collect();
        if let Some(last) = pending.into_iter().next_back() {
            self.say(last);
        }
    }

    /// One answer from the worker, checked against what the interface is showing now.
    ///
    /// Split out from `drain` so that a test can hand an answer straight over: a request
    /// that came back for a column the user has since left is the whole point of the
    /// owner on each pane, and it is not something a detached worker can be made to
    /// produce.
    pub fn accept(&mut self, response: Response) {
        match response {
            Response::Catalog { listing, result } => self.catalog_arrived(listing, result),
            Response::Seasons { series_id, result } => {
                if series_id != self.seasons.owner {
                    return;
                }
                match result {
                    Ok(items) => self.seasons.set(items),
                    Err(error) => {
                        self.seasons.loading = false;
                        self.seasons.error = Some(error.clone());
                        self.complain(error);
                    }
                }
            }
            Response::Episodes { season_id, result } => {
                if season_id != self.episodes.owner {
                    return;
                }
                match result {
                    Ok(items) => {
                        let episode_ids = items.iter().map(|episode| episode.id.clone()).collect();
                        self.episodes.set(items);
                        // Now rather than when the season was asked for: these are the
                        // ids the answer actually brought back.
                        self.worker.send(Request::Playheads {
                            season_id,
                            episode_ids,
                        });
                    }
                    Err(error) => {
                        self.episodes.loading = false;
                        self.episodes.error = Some(error.clone());
                        self.complain(error);
                    }
                }
            }
            Response::Playheads { season_id, result } => {
                if season_id != self.episodes.owner {
                    return;
                }
                // A marker that does not arrive is a row drawn the way it was drawn
                // before any of this, which is no reason to put an error where the user
                // is reading episode titles.
                if let Ok(playheads) = result {
                    self.playheads = playheads
                        .into_iter()
                        .map(|playhead| (playhead.content_id.clone(), playhead))
                        .collect();
                }
            }
            // Nothing on screen is redrawn by this - the watchlist and the history are
            // not among the three columns - so the sentence is the whole of it, and it
            // is shown whether the cursor has moved on since or not.
            Response::Account { result } => match result {
                Ok(message) => self.say(message),
                Err(error) => self.complain(error),
            },
            Response::Download { id, update } => self.download_moved(id, update),
        }
    }

    /// One answer about a queued download.
    ///
    /// The row is found by the number the request carried rather than by the episode,
    /// since the queue may hold the same episode twice and rows are dropped out of the
    /// middle of it. An answer about a row that has been dropped names nothing and is
    /// let go of.
    ///
    /// Only the two ends of a download are worth a sentence on the status line. The
    /// steps in between are what the panel is drawn from, and a status line repainted
    /// several times a second with the percentage of a track would leave no room for
    /// anything else the interface has to say.
    fn download_moved(&mut self, id: usize, update: Update) {
        let Some(download) = self
            .downloads
            .items
            .iter_mut()
            .find(|download| download.id == id)
        else {
            return;
        };
        let finished = match &update {
            Update::Finished(result) => Some((download.number.clone(), result.clone())),
            _ => None,
        };
        download.update(update);
        match finished {
            Some((number, Ok(()))) => self.say(format!("Downloaded {number}")),
            Some((number, Err(error))) => self.complain(format!("{number} failed: {error}")),
            None => {}
        }
    }

    fn pane_move(&mut self, focus: Focus, delta: isize) {
        match focus {
            Focus::Series => self.series.move_by(delta),
            Focus::Seasons => self.seasons.move_by(delta),
            Focus::Episodes => self.episodes.move_by(delta),
            Focus::Downloads => self.downloads.move_by(delta),
        }
    }

    fn focused_pane_move(&mut self, delta: isize) {
        self.pane_move(self.focus, delta);
    }

    /// Where a column's list was when it was last drawn: its first visible row, and how
    /// many items it holds.
    fn pane_window(&self, focus: Focus) -> (usize, usize) {
        match focus {
            Focus::Series => self.series.window(),
            Focus::Seasons => self.seasons.window(),
            Focus::Episodes => self.episodes.window(),
            Focus::Downloads => self.downloads.window(),
        }
    }

    fn cursor(&self, focus: Focus) -> Option<usize> {
        match focus {
            Focus::Series => self.series.state.selected(),
            Focus::Seasons => self.seasons.state.selected(),
            Focus::Episodes => self.episodes.state.selected(),
            Focus::Downloads => self.downloads.state.selected(),
        }
    }

    fn put_cursor(&mut self, focus: Focus, index: usize) {
        match focus {
            Focus::Series => self.series.select(index),
            Focus::Seasons => self.seasons.select(index),
            Focus::Episodes => self.episodes.select(index),
            Focus::Downloads => self.downloads.select(index),
        }
    }

    /// The item on `row` of a column, if the pointer is on one rather than on a border,
    /// a title, or the sentence an empty column draws.
    fn item_at(&self, focus: Focus, row: u16) -> Option<usize> {
        let (offset, len) = self.pane_window(focus);
        mouse::row_at(self.regions.column(focus), offset, len, row)
    }

    fn focused_pane_edge(&mut self, last: bool) {
        match self.focus {
            Focus::Series => self.series.select_edge(last),
            Focus::Seasons => self.seasons.select_edge(last),
            Focus::Episodes => self.episodes.select_edge(last),
            Focus::Downloads => self.downloads.select_edge(last),
        }
    }

    fn descend(&mut self) -> Action {
        match self.focus {
            Focus::Series => {
                self.request_seasons();
                Action::None
            }
            Focus::Seasons => {
                self.request_episodes();
                Action::None
            }
            Focus::Episodes => self.play(false),
            // There is nothing behind a queue row to open, and taking one out of the
            // list is the one thing the panel has to be asked for. Putting it on `open`
            // is what gives the pointer the same thing without a gesture of its own: a
            // second click on the row the cursor is already on.
            Focus::Downloads => {
                self.drop_download();
                Action::None
            }
        }
    }

    fn ascend(&mut self) {
        match self.focus {
            Focus::Downloads => self.focus = Focus::Episodes,
            Focus::Episodes => self.focus = Focus::Seasons,
            Focus::Seasons => self.focus = Focus::Series,
            // Leaving the leftmost column means leaving the search behind.
            Focus::Series => {
                if matches!(self.listing, Listing::Search(_)) {
                    self.listing = Listing::Browse(self.sort);
                    self.request_catalog();
                }
            }
        }
    }

    /// The episodes to act on: the selected one, or the rest of the season after it.
    fn selection(&mut self, to_end: bool) -> Vec<SeasonEpisode> {
        let Some(index) = self.episodes.state.selected() else {
            self.complain("Open a season first.");
            return Vec::new();
        };
        if to_end {
            self.episodes.items[index..].to_vec()
        } else {
            self.episodes.items[index..=index].to_vec()
        }
    }

    /// Puts the selected series on the watchlist, or takes it off.
    ///
    /// The series is the one the catalogue column has selected, whichever column the
    /// keyboard happens to be in. The seasons and the episodes on screen belong to that
    /// series, so someone working down in the episodes who asks for the watchlist means
    /// the series those episodes came from - there is nothing else the key could mean,
    /// and doing nothing in two columns out of three would be an odd way to say so.
    ///
    /// Which way it goes is settled on the worker thread, because finding out takes a
    /// request of its own.
    fn toggle_watchlist(&mut self) {
        let Some(series) = self.series.selected() else {
            self.complain("Pick a series first.");
            return;
        };
        self.worker.send(Request::Watchlist {
            series_id: series.id.clone(),
            series_title: series.title.clone(),
        });
    }

    /// Marks the selected episode watched, or unwatched again.
    ///
    /// This one is about the episode under the cursor and nothing else, so with no
    /// season open it says what `play` and `download` say - the answer is the same one:
    /// open a season.
    fn mark(&mut self, watched: bool) {
        let Some(episode) = self.episodes.selected() else {
            self.complain("Open a season first.");
            return;
        };
        let episode_id = episode.id.clone();
        let label = episode_label(episode);
        let seconds = if watched {
            whole_seconds(episode.duration_ms)
        } else {
            0
        };
        // Marking watched is putting the playhead at the end of the episode, and an
        // episode Crunchyroll gives no running time for has no end to put it at. A
        // playhead of zero is exactly what unwatched means, so sending one here would do
        // the opposite of what the key says - and quietly, since the account would come
        // back saying the episode had never been touched. Better to say there is nothing
        // to aim at.
        if watched && seconds == 0 {
            self.complain(format!(
                "Crunchyroll gives no running time for {label}, so there is no end to mark it watched at."
            ));
            return;
        }
        self.worker.send(Request::Playhead {
            episode_id,
            label,
            seconds,
        });
    }

    fn play(&mut self, to_end: bool) -> Action {
        let episodes = self.selection(to_end);
        if episodes.is_empty() {
            return Action::None;
        }
        Action::Play(episodes)
    }

    /// Puts the selection on the queue.
    ///
    /// The options travel with each episode rather than being read when its turn comes.
    /// The queue may be an hour deep and the interface goes on being used the whole
    /// time, so the quality and the languages that were along the top when the key was
    /// pressed are the ones the user meant - not whatever is up there by the time the
    /// thread reaches the request.
    fn queue_downloads(&mut self, whole_season: bool) {
        let episodes = if whole_season {
            if self.episodes.items.is_empty() {
                self.complain("Open a season first.");
            }
            self.episodes.items.clone()
        } else {
            self.selection(false)
        };
        if episodes.is_empty() {
            return;
        }
        let queued = episodes.len();
        for episode in episodes {
            let id = self.next_download;
            self.next_download += 1;
            self.downloads.items.push(Download {
                id,
                number: format!("S{:02}E{}", episode.season_number, episode_number(&episode)),
                title: episode.title.clone(),
                series: episode.series_title.clone(),
                state: State::Queued,
                stages: Vec::new(),
            });
            self.worker.send(Request::Download(Box::new(Queued {
                id,
                episode,
                options: self.options.clone(),
            })));
        }
        // A panel with a list in it and no cursor anywhere has nothing the arrow keys
        // could move, so the first row queued takes one.
        if self.downloads.state.selected().is_none() {
            self.downloads.state.select(Some(0));
        }
        self.say(match queued {
            1 => "Queued 1 episode for download".to_owned(),
            queued => format!("Queued {queued} episodes for download"),
        });
    }

    /// Takes the row under the cursor out of the queue.
    ///
    /// Anything but the download that is running, which is inside an hour of segments
    /// on a thread of its own and cannot be called back. One that has not started is
    /// dropped at both ends: the row goes, and the number goes to the worker so that
    /// the thread passes over the request when it reaches it.
    fn drop_download(&mut self) {
        let Some(index) = self.downloads.state.selected() else {
            return;
        };
        let Some(download) = self.downloads.items.get(index) else {
            return;
        };
        if !download.droppable() {
            self.complain("That one is already downloading.");
            return;
        }
        if download.state == State::Queued {
            self.worker.abandon(download.id);
        }
        self.downloads.items.remove(index);
        // The row the cursor was on has gone, so it lands on whatever took its place -
        // or on the last row, where it was the last row that went.
        let left = self.downloads.items.len();
        self.downloads
            .state
            .select((left > 0).then(|| index.min(left - 1)));
    }

    /// The locales worth offering for the current selection, most specific first: what
    /// the season lists, else what the series lists, else what was asked for on the
    /// command line, else every locale Crunchyroll publishes. Whatever is in use is
    /// always among them, so the list can open on it even when the selection does not
    /// admit to having it.
    fn locales(&self, audio: bool) -> Vec<String> {
        let from_season = self.seasons.selected().map(|season| {
            if audio {
                &season.audio_locales
            } else {
                &season.subtitle_locales
            }
        });
        let from_series = self.series.selected().map(|series| {
            if audio {
                &series.series_metadata.audio_locales
            } else {
                &series.series_metadata.subtitle_locales
            }
        });
        let configured = if audio {
            &self.options.audio_langs
        } else {
            &self.options.subtitles_langs
        };
        let mut locales = [from_season, from_series]
            .into_iter()
            .flatten()
            .find(|locales| !locales.is_empty())
            .cloned()
            .unwrap_or_else(|| {
                if configured.is_empty() {
                    LANGUAGES
                        .iter()
                        .map(|(locale, _)| (*locale).to_owned())
                        .collect()
                } else {
                    configured.clone()
                }
            });
        locales.push(if audio { self.audio() } else { self.subs() });
        locales.sort();
        locales.dedup();
        locales
    }

    /// Opens the language list on the locale in use.
    fn open_picker(&mut self, audio: bool) {
        let locales = self.locales(audio);
        let current = if audio { self.audio() } else { self.subs() };
        let mut pane = Pane {
            pending_cursor: locales.iter().position(|locale| *locale == current),
            ..Pane::default()
        };
        pane.set(locales);
        self.picker = Some(Picker { audio, pane });
    }

    fn cycle_locale(&mut self, audio: bool) {
        let locales = self.locales(audio);
        let current = if audio { self.audio() } else { self.subs() };
        let next = locales
            .iter()
            .position(|locale| *locale == current)
            .map_or(0, |index| (index + 1) % locales.len());
        if let Some(chosen) = locales.get(next).cloned() {
            self.choose_locale(audio, chosen);
        }
    }

    fn choose_locale(&mut self, audio: bool, locale: String) {
        let changed = locale != if audio { self.audio() } else { self.subs() };
        let name = language_name(&locale).to_owned();
        if audio {
            self.options.audio_langs = vec![locale];
            self.say(format!("Audio: {name}"));
        } else {
            self.options.subtitles_langs = vec![locale];
            self.say(format!("Subtitles: {name}"));
        }
        if changed {
            self.refresh_localised();
        }
    }

    /// Crunchyroll is asked for both lists in the chosen languages: titles come back
    /// localised, and an episode carries the dub that was asked for. So a language
    /// change only shows once the deepest list open has been asked for again - with the
    /// cursor put back, since it is still the same season being looked at.
    fn refresh_localised(&mut self) {
        let focus = self.focus;
        if !self.episodes.owner.is_empty() {
            let cursor = self.episodes.state.selected();
            self.request_episodes();
            self.episodes.pending_cursor = cursor;
        } else if !self.seasons.owner.is_empty() {
            let cursor = self.seasons.state.selected();
            self.request_seasons();
            self.seasons.pending_cursor = cursor;
        }
        self.focus = focus;
    }

    fn cycle_quality(&mut self) {
        let next = QUALITIES
            .iter()
            .position(|quality| *quality == self.options.video_quality)
            .map_or(0, |index| (index + 1) % QUALITIES.len());
        self.options.video_quality = QUALITIES[next].to_owned();
        let quality = self.options.video_quality.clone();
        self.say(format!("Video quality: {quality}"));
    }

    /// Moves the catalogue column on to the next list in the ring: the browse orders,
    /// then the account's own lists, then round again.
    ///
    /// The browse order is remembered as the ring goes past it, so a search left with
    /// `back` returns to the order that was last being browsed rather than to whichever
    /// one the interface happened to start on.
    fn cycle_source(&mut self) {
        self.listing = self.listing.next();
        if let Listing::Browse(sort) = self.listing {
            self.sort = sort;
        }
        self.request_catalog();
    }

    fn reload(&mut self) {
        match self.focus {
            Focus::Series => {
                let cursor = self.series.state.selected();
                self.request_catalog();
                self.series.pending_cursor = cursor;
            }
            Focus::Seasons => {
                let cursor = self.seasons.state.selected();
                self.request_seasons();
                self.seasons.pending_cursor = cursor;
            }
            Focus::Episodes => {
                let cursor = self.episodes.state.selected();
                self.request_episodes();
                self.episodes.pending_cursor = cursor;
            }
            // The queue is not a list anything answers with: it is what this run has
            // asked for, and there is nowhere to ask for it again.
            Focus::Downloads => {}
        }
    }

    fn edit_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.editing = None,
            KeyCode::Enter => {
                let query = self.editing.take().unwrap_or_default().trim().to_owned();
                self.listing = if query.is_empty() {
                    Listing::Browse(self.sort)
                } else {
                    Listing::Search(query)
                };
                self.request_catalog();
            }
            KeyCode::Backspace => {
                if let Some(query) = self.editing.as_mut() {
                    query.pop();
                }
            }
            KeyCode::Char(letter) => {
                if let Some(query) = self.editing.as_mut() {
                    query.push(letter);
                }
            }
            _ => {}
        }
    }

    /// The language list has the keys of a column, plus the one that cycles the columns
    /// to look at the other list without going back out first. Anything else does
    /// nothing while it is open, which is what a new command should do here until
    /// someone decides otherwise.
    fn edit_picker(&mut self, command: Command) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        match command {
            Command::Back | Command::Quit => self.picker = None,
            Command::Up => picker.pane.move_by(-1),
            Command::Down => picker.pane.move_by(1),
            Command::PageUp => picker.pane.move_by(-10),
            Command::PageDown => picker.pane.move_by(10),
            Command::Top => picker.pane.select_edge(false),
            Command::Bottom => picker.pane.select_edge(true),
            Command::NextColumn => {
                let other = !picker.audio;
                self.open_picker(other);
            }
            Command::AudioLanguage => self.open_picker(true),
            Command::SubtitleLanguage => self.open_picker(false),
            Command::Open => {
                let audio = picker.audio;
                let chosen = picker.pane.selected().cloned();
                self.picker = None;
                if let Some(chosen) = chosen {
                    self.choose_locale(audio, chosen);
                }
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        // ctrl-c is not one of the bindings. It is how a terminal program is left, and a
        // config file has no business being able to take it away.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        // The search box is typing rather than commands: every letter belongs in the
        // query, whatever it would otherwise do.
        if self.editing.is_some() {
            self.edit_search(key);
            return Action::None;
        }
        let command = self.keys.command(key);
        if self.picker.is_some() {
            self.notice = None;
            if let Some(command) = command {
                self.edit_picker(command);
            }
            return Action::None;
        }
        // The help popup is read and dismissed, so any key at all closes it - including
        // one that is bound to nothing.
        if self.show_help {
            self.show_help = false;
            return Action::None;
        }
        self.notice = None;
        let Some(command) = command else {
            return Action::None;
        };
        self.run(command)
    }

    /// Does what a command says, whatever asked for it: a key, or a word on the screen
    /// that was clicked. One place decides what a command means, so what the pointer
    /// does cannot drift away from what the keyboard does - and the help popup stays the
    /// whole list of what the interface can be asked for.
    fn run(&mut self, command: Command) -> Action {
        // Whether quit was the last thing asked for, cleared by anything else. Leaving
        // ends the process, and the download thread goes with it: an episode half
        // written is an hour of segments thrown away and a scratch file left behind.
        // So the first quit during one says so and the second is taken at its word,
        // which is a great deal less in the way than a dialogue box and costs a
        // keypress only while something is actually running. ctrl-c is not part of
        // this: it is how a terminal program is left, and it is not ours to argue with.
        let confirmed = std::mem::take(&mut self.leaving);
        match command {
            Command::Quit => {
                if !confirmed && self.downloads.items.iter().any(Download::running) {
                    self.leaving = true;
                    self.complain("A download is still running. Ask again to leave anyway.");
                    return Action::None;
                }
                return Action::Quit;
            }
            Command::Help => self.show_help = true,
            Command::Search => self.editing = Some(String::new()),
            Command::Up => self.focused_pane_move(-1),
            Command::Down => self.focused_pane_move(1),
            Command::PageUp => self.focused_pane_move(-10),
            Command::PageDown => self.focused_pane_move(10),
            Command::Top => self.focused_pane_edge(false),
            Command::Bottom => self.focused_pane_edge(true),
            Command::Open => return self.descend(),
            Command::Back => self.ascend(),
            Command::NextColumn => {
                self.focus = match self.focus {
                    Focus::Series => Focus::Seasons,
                    Focus::Seasons => Focus::Episodes,
                    Focus::Episodes => Focus::Downloads,
                    Focus::Downloads => Focus::Series,
                }
            }
            Command::Play => return self.play(false),
            Command::PlayRest => return self.play(true),
            Command::Download => self.queue_downloads(false),
            Command::DownloadSeason => self.queue_downloads(true),
            Command::Watchlist => self.toggle_watchlist(),
            Command::MarkWatched => self.mark(true),
            Command::MarkUnwatched => self.mark(false),
            Command::AudioLanguage => self.open_picker(true),
            Command::SubtitleLanguage => self.open_picker(false),
            Command::NextAudio => self.cycle_locale(true),
            Command::NextSubtitle => self.cycle_locale(false),
            Command::Quality => self.cycle_quality(),
            Command::Images => {
                let message = self.art.toggle();
                self.say(message);
            }
            Command::Order => self.cycle_source(),
            Command::Reload => self.reload(),
        }
        Action::None
    }

    /// What the pointer does.
    ///
    /// The keyboard's rules read through a mouse: a click puts the cursor somewhere, a
    /// click on where the cursor already is opens it, and every word drawn along an edge
    /// runs the command it names. Nothing here does anything no key does.
    pub fn on_mouse(&mut self, event: MouseEvent) -> Action {
        let at = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(at),
            MouseEventKind::Drag(MouseButton::Left) => {
                self.drag(at);
                Action::None
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.back(at);
                Action::None
            }
            MouseEventKind::ScrollUp => {
                self.wheel(at, -mouse::WHEEL);
                Action::None
            }
            MouseEventKind::ScrollDown => {
                self.wheel(at, mouse::WHEEL);
                Action::None
            }
            // The middle button pastes the primary selection, which is not ours to take
            // away; a touchpad tilted sideways is far too easy to do by accident for
            // anything as coarse as changing column; and `Moved` never arrives, because
            // the reporting this asks for does not include it.
            _ => Action::None,
        }
    }

    /// The left button pressed. On the press rather than the release: a press that turns
    /// into a drag is the same gesture either way, and waiting for the release only
    /// makes the interface answer late.
    fn click(&mut self, at: Position) -> Action {
        // The search box owns the pointer as it owns the keyboard: a click is the way
        // out of it, as escape is, and nothing else.
        if self.editing.is_some() {
            self.editing = None;
            return Action::None;
        }
        // The help popup is read and dismissed, so a click anywhere closes it - even one
        // that landed on a word that would otherwise have answered.
        if self.show_help {
            self.show_help = false;
            return Action::None;
        }
        let target = self.regions.at(at);
        self.notice = None;
        if self.picker.is_some() {
            match target {
                Target::Picker => self.click_picker(at),
                _ => self.picker = None,
            }
            return Action::None;
        }
        match target {
            Target::Button(command) => self.run(command),
            Target::Column(focus) => {
                let working_there = self.focus == focus;
                self.focus = focus;
                let Some(index) = self.item_at(focus, at.y) else {
                    return Action::None;
                };
                // A click on the row the cursor is already on, in the column already
                // being worked in, is what `open` is. The first click into a column can
                // only ever choose, so nothing is ever played by surprise - and two
                // quick clicks are two clicks on the same row, so a double click works
                // without a clock, which is what makes it behave the same over ssh.
                if working_there && self.cursor(focus) == Some(index) {
                    return self.descend();
                }
                self.put_cursor(focus, index);
                Action::None
            }
            Target::Picker | Target::Outside | Target::Nothing => Action::None,
        }
    }

    /// A drag moves the cursor and never opens: an open on the way past would fire on
    /// every row the pointer crossed, and one of them plays an episode. Only the column
    /// the drag started in answers - the press that began it is what focused that column
    /// - so a drag wandering out of its list does not start driving another one.
    fn drag(&mut self, at: Position) {
        if self.editing.is_some() || self.show_help {
            return;
        }
        if self.picker.is_some() {
            self.select_picker(at);
            return;
        }
        if self.regions.at(at) == Target::Column(self.focus)
            && let Some(index) = self.item_at(self.focus, at.y)
        {
            self.put_cursor(self.focus, index);
        }
    }

    /// The wheel moves the cursor of the column under the pointer and leaves the
    /// keyboard where it was: looking down a list is not the same as going to work in
    /// it, and each column keeps a cursor of its own, so a look costs nothing.
    fn wheel(&mut self, at: Position, delta: isize) {
        if self.editing.is_some() || self.show_help {
            return;
        }
        if self.picker.is_some() {
            if let Target::Picker = self.regions.at(at)
                && let Some(picker) = self.picker.as_mut()
            {
                picker.pane.move_by(delta);
            }
            return;
        }
        if let Target::Column(focus) = self.regions.at(at) {
            self.pane_move(focus, delta);
        }
    }

    /// The right button goes back, out of the column it was pressed on rather than out
    /// of wherever the keyboard happens to be - which is the only reading that does not
    /// depend on something invisible.
    fn back(&mut self, at: Position) {
        if self.editing.is_some() {
            self.editing = None;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.picker.is_some() {
            self.picker = None;
            return;
        }
        if let Target::Column(focus) = self.regions.at(at) {
            self.notice = None;
            self.focus = focus;
            self.ascend();
        }
    }

    /// The locale under the pointer, while the language list is open.
    fn picked_at(&self, at: Position) -> Option<usize> {
        let picker = self.picker.as_ref()?;
        let (offset, len) = picker.pane.window();
        mouse::row_at(self.regions.picker, offset, len, at.y)
    }

    fn select_picker(&mut self, at: Position) {
        if let Some(index) = self.picked_at(at)
            && let Some(picker) = self.picker.as_mut()
        {
            picker.pane.select(index);
        }
    }

    /// The columns' rule again: a click chooses a locale, and a click on the one already
    /// chosen applies it.
    fn click_picker(&mut self, at: Position) {
        let Some(index) = self.picked_at(at) else {
            return;
        };
        if self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.pane.state.selected() == Some(index))
        {
            self.edit_picker(Command::Open);
        } else {
            self.select_picker(at);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    use crate::download::DownloadOptions;
    use crate::model::CatalogItem;
    use crate::tui::art::Gallery;
    use crate::tui::keys::{Bindings, Command};
    use crate::tui::theme::Theme;
    use crate::tui::worker::{Listing, Request, Response, Update, Worker};

    use super::{Action, App, Focus, Pane, SeasonEpisode, State, episode_label, whole_seconds};

    /// An interface with nothing behind it: the worker swallows every request and never
    /// answers one, so the only answers it sees are those a test hands it directly.
    fn app() -> App {
        App::new(
            Worker::detached(),
            DownloadOptions {
                audio_langs: vec!["ja-JP".to_owned()],
                subtitles_langs: vec!["en-US".to_owned()],
                cc_langs: Vec::new(),
                video_quality: "1080p".to_owned(),
                audio_quality: "192k".to_owned(),
                play: false,
                mpv_args: Vec::new(),
                start_at: None,
                playhead: None,
                reporter: None,
            },
            Theme::default(),
            Bindings::default(),
            Arc::new(Mutex::new(Vec::new())),
            Gallery::detached(false),
        )
    }

    fn series(id: &str) -> CatalogItem {
        CatalogItem {
            id: id.to_owned(),
            kind: "series".to_owned(),
            ..CatalogItem::default()
        }
    }

    /// An open season, so that the download keys have something to act on. The catalogue
    /// the interface asked for as it opened is taken off the worker's hands here too,
    /// since none of these tests is about that.
    fn with_episodes(app: &mut App, count: usize) {
        app.episodes.set(
            (1..=count)
                .map(|number| SeasonEpisode {
                    id: format!("E{number}"),
                    episode: number.to_string(),
                    episode_number: number as i32,
                    season_number: 1,
                    series_title: "Frieren".to_owned(),
                    title: format!("Episode {number}"),
                    ..SeasonEpisode::default()
                })
                .collect(),
        );
        app.focus = Focus::Episodes;
        app.sent();
    }

    /// One answer about a download, as the queue's thread sends them.
    fn answer(app: &mut App, index: usize, update: Update) {
        let id = app.downloads.items[index].id;
        app.accept(Response::Download { id, update });
    }

    /// The whole life of one queued episode, which is all the account of it anyone
    /// gets: the panel is where a download's progress bar went, and the status line is
    /// its last word. Without this a row could sit at `queued` for the hour the file
    /// was being written, or read as still downloading long after it was there.
    #[test]
    fn a_download_walks_from_queued_to_done() {
        let mut app = app();
        with_episodes(&mut app, 1);
        app.run(Command::Download);

        assert_eq!(app.downloads.items.len(), 1);
        assert_eq!(app.downloads.items[0].state, State::Queued);
        assert_eq!(
            app.downloads.state.selected(),
            Some(0),
            "the panel has a list in it and nothing the arrow keys could move"
        );
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.text.contains("Queued")),
            "nothing said the episode had been queued"
        );

        answer(&mut app, 0, Update::Started);
        assert_eq!(app.downloads.items[0].state, State::Running);

        answer(
            &mut app,
            0,
            Update::Stage {
                stage: "video".to_owned(),
                done: 3,
                total: 12,
            },
        );
        assert_eq!(app.downloads.items[0].stage(), Some(("video", Some(0.25))));

        answer(&mut app, 0, Update::Finished(Ok(())));
        assert_eq!(app.downloads.items[0].state, State::Done);
        assert_eq!(
            app.downloads.items[0].stage(),
            None,
            "a download that is over is still waiting on a part of itself"
        );
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.text.contains("Downloaded S01E1") && !notice.error)
        );
    }

    /// An episode Crunchyroll will not hand over is no reason to drop the ones behind
    /// it: the queue is a list of things asked for one at a time, and the command line
    /// has always carried on through a season the same way. The reason is kept on the
    /// row, since the download thread has no terminal to have printed it to.
    #[test]
    fn a_failure_takes_one_episode_and_not_the_queue() {
        let mut app = app();
        with_episodes(&mut app, 2);
        app.run(Command::DownloadSeason);
        assert_eq!(app.downloads.items.len(), 2);

        answer(&mut app, 0, Update::Started);
        answer(
            &mut app,
            0,
            Update::Finished(Err("playback error: 420 Enhance Your Calm".to_owned())),
        );
        assert_eq!(
            app.downloads.items[0].state,
            State::Failed("playback error: 420 Enhance Your Calm".to_owned())
        );
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.error && notice.text.contains("Enhance Your Calm"))
        );

        // And the next one goes ahead exactly as it would have done.
        answer(&mut app, 1, Update::Started);
        answer(&mut app, 1, Update::Finished(Ok(())));
        assert_eq!(app.downloads.items[1].state, State::Done);
        assert_eq!(
            app.downloads.items[0].state,
            State::Failed("playback error: 420 Enhance Your Calm".to_owned()),
            "the failure was painted over by the answer for another row"
        );
    }

    /// The options a download runs with are the ones that were in force when it was
    /// asked for. A queue can be an hour deep and the interface goes on being used the
    /// whole time, so reading the quality when the episode's turn came would write a
    /// file at whatever had been cycled to since - and the two episodes of one season
    /// queued a minute apart would come out at different sizes.
    #[test]
    fn a_download_keeps_the_options_it_was_queued_with() {
        let mut app = app();
        with_episodes(&mut app, 1);
        app.run(Command::Download);
        app.run(Command::Quality);
        assert_eq!(app.options.video_quality, "720p");

        let sent = app.sent();
        let [Request::Download(job)] = sent.as_slice() else {
            panic!("the episode was not queued: {sent:?}");
        };
        assert_eq!(job.episode.id, "E1");
        assert_eq!(job.options.video_quality, "1080p");
        assert_eq!(job.options.audio_langs, ["ja-JP"]);
    }

    /// The bar crosses one part of the episode at a time, in the order the downloader
    /// first mentioned them. The video and the first audio track come down side by
    /// side, so a row showing whichever of them reported most recently would jump
    /// between two percentages that have nothing to do with each other.
    #[test]
    fn the_bar_follows_the_part_still_being_waited_on() {
        let mut app = app();
        with_episodes(&mut app, 1);
        app.run(Command::Download);
        answer(&mut app, 0, Update::Started);

        // Subtitles come down whole, so they are a part with no size to speak of until
        // they are over - which they say by reporting one out of one.
        let stage = |stage: &str, done, total| Update::Stage {
            stage: stage.to_owned(),
            done,
            total,
        };
        answer(&mut app, 0, stage("subtitles", 0, 0));
        assert_eq!(app.downloads.items[0].stage(), Some(("subtitles", None)));
        answer(&mut app, 0, stage("subtitles", 1, 1));

        answer(&mut app, 0, stage("video", 1, 10));
        answer(&mut app, 0, stage("Japanese audio", 8, 10));
        assert_eq!(
            app.downloads.items[0].stage(),
            Some(("video", Some(0.1))),
            "the audio ran ahead and took the bar with it"
        );

        answer(&mut app, 0, stage("video", 10, 10));
        assert_eq!(
            app.downloads.items[0].stage(),
            Some(("Japanese audio", Some(0.8))),
            "the bar stayed on a part that has finished"
        );
        assert_eq!(
            app.downloads.items[0].stages.len(),
            3,
            "a part that reports a thousand times is one row of the panel, not a thousand"
        );
    }

    /// A queue is not much use if nothing can be taken out of it: a season queued by
    /// mistake is ten rows and ten downloads to sit through. Everything but the episode
    /// that is already running can go - that one is inside an hour of segments on a
    /// thread of its own, and a row that vanished while the file went on being written
    /// would be a lie.
    #[test]
    fn a_row_can_be_dropped_unless_it_is_the_one_downloading() {
        let mut app = app();
        with_episodes(&mut app, 3);
        app.run(Command::DownloadSeason);
        app.focus = Focus::Downloads;
        answer(&mut app, 0, Update::Started);

        app.run(Command::Open);
        assert_eq!(app.downloads.items.len(), 3, "the running episode was cut");
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.text.contains("already downloading"))
        );

        // The two behind it have not been started, so they go - and the queue is told,
        // since the request is already on its channel and cannot be taken off it.
        app.run(Command::Down);
        app.run(Command::Open);
        assert_eq!(app.downloads.items.len(), 2);
        assert_eq!(app.downloads.state.selected(), Some(1));

        app.run(Command::Open);
        assert_eq!(app.downloads.items.len(), 1);
        assert_eq!(
            app.downloads.state.selected(),
            Some(0),
            "the cursor was left past the end of what is left"
        );

        // And a row that is over goes the same way.
        answer(&mut app, 0, Update::Finished(Ok(())));
        app.run(Command::Open);
        assert!(app.downloads.items.is_empty());
        assert_eq!(app.downloads.state.selected(), None);
    }

    /// Leaving takes the download thread with it, so the episode being written is an
    /// hour of segments thrown away and a scratch file left in the series directory. A
    /// second ask is cheap and only ever happens while something is running; nothing
    /// stands between anyone and the door the rest of the time.
    #[test]
    fn quitting_during_a_download_asks_twice() {
        let mut app = app();
        with_episodes(&mut app, 2);
        app.run(Command::DownloadSeason);

        // Nothing has started, so nothing is in the way.
        assert!(matches!(app.run(Command::Quit), Action::Quit));

        answer(&mut app, 0, Update::Started);
        assert!(matches!(app.run(Command::Quit), Action::None));
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.error && notice.text.contains("still running"))
        );
        assert!(matches!(app.run(Command::Quit), Action::Quit));

        // And asking for anything else in between is taking it back.
        answer(&mut app, 0, Update::Started);
        assert!(matches!(app.run(Command::Quit), Action::None));
        app.run(Command::Down);
        assert!(matches!(app.run(Command::Quit), Action::None));

        // A download that is over is not one to be held up by.
        answer(&mut app, 0, Update::Finished(Ok(())));
        assert!(matches!(app.run(Command::Quit), Action::Quit));
    }

    /// The panel is a stop on the ring rather than somewhere only the mouse can reach,
    /// and the ring closes: holding the key down can only go round.
    #[test]
    fn the_columns_cycle_through_the_downloads_panel() {
        let mut app = app();
        let mut visited = Vec::new();
        for _ in 0..5 {
            visited.push(app.focus);
            app.run(Command::NextColumn);
        }
        assert_eq!(
            visited,
            [
                Focus::Series,
                Focus::Seasons,
                Focus::Episodes,
                Focus::Downloads,
                Focus::Series
            ]
        );

        // And `back` leaves it the way it leaves any other column: one to the left.
        app.focus = Focus::Downloads;
        app.run(Command::Back);
        assert_eq!(app.focus, Focus::Episodes);
    }

    /// The interface opens on what was being watched last, and an account that has
    /// watched nothing must not be shown an empty column as its first screen - that is
    /// worse than the catalogue, which at least has something in it.
    #[test]
    fn an_empty_history_opens_the_catalogue_instead() {
        let mut app = app();
        assert_eq!(app.listing, Listing::History);

        app.catalog_arrived(Listing::History, Ok(Vec::new()));
        assert_eq!(app.listing, Listing::Browse(0));
        assert!(app.series.loading, "the catalogue was asked for");
        assert!(
            app.notice.is_some(),
            "the column is showing a list nobody asked for and said nothing about it"
        );

        // And the answer to that request is taken as the list it is.
        app.catalog_arrived(Listing::Browse(0), Ok(vec![series("GY8VEQ95Y")]));
        assert_eq!(app.listing, Listing::Browse(0));
        assert_eq!(app.series.items.len(), 1);
        assert!(!app.series.loading);
    }

    /// A history that cannot be fetched at all - an account Crunchyroll would not name,
    /// or a request that failed - is the same problem: an error message is no way to
    /// open. The error is still worth saying, since it is the only sign anything went
    /// wrong, but it belongs on the status line rather than in the column.
    #[test]
    fn a_failed_history_opens_the_catalogue_instead() {
        let mut app = app();
        app.catalog_arrived(Listing::History, Err("no account id".to_owned()));
        assert_eq!(app.listing, Listing::Browse(0));
        assert_eq!(app.series.error, None, "the column kept the error");
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.text.contains("no account id")),
            "the status line said nothing about why"
        );
    }

    /// The fallback asks for a list from inside the answer to another one, which is the
    /// shape a loop has. It fires once, for the first screen, and the history asked for
    /// on purpose afterwards shows whatever it has - including nothing.
    #[test]
    fn the_fallback_happens_once() {
        let mut app = app();
        app.catalog_arrived(Listing::History, Ok(Vec::new()));
        assert_eq!(app.listing, Listing::Browse(0));

        // An empty catalogue is not a reason to go looking for another list.
        app.catalog_arrived(Listing::Browse(0), Ok(Vec::new()));
        assert_eq!(app.listing, Listing::Browse(0));

        // Nor is the history, once it has been chosen deliberately.
        while app.listing != Listing::History {
            app.run(Command::Order);
        }
        app.catalog_arrived(Listing::History, Ok(Vec::new()));
        assert_eq!(
            app.listing,
            Listing::History,
            "the fallback fired a second time"
        );
    }

    /// The order key walks one ring, and the browse order it leaves off at is where a
    /// search comes back to - a search is not on the ring, so `back` out of one has to
    /// return to something, and the order last in use is the only answer that does not
    /// throw away what the user chose.
    #[test]
    fn leaving_a_search_returns_to_the_order_last_browsed() {
        let mut app = app();
        app.run(Command::Order);
        assert_eq!(app.listing, Listing::Browse(0));
        app.run(Command::Order);
        app.run(Command::Order);
        assert_eq!(app.listing, Listing::Browse(2));

        app.editing = Some("frieren".to_owned());
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.listing, Listing::Search("frieren".to_owned()));

        app.run(Command::Back);
        assert_eq!(app.listing, Listing::Browse(2));
        // And the ring carries on from there rather than starting over.
        app.run(Command::Order);
        assert_eq!(app.listing, Listing::Watchlist);
    }

    /// The playhead that marks an episode watched is its own running time, and the
    /// endpoint counts in seconds while Crunchyroll answers in milliseconds. Getting the
    /// conversion wrong by a factor of a thousand would leave every episode marked
    /// watched at the twenty-four-second mark, which Crunchyroll would take as barely
    /// started rather than as finished.
    #[test]
    fn an_episode_is_watched_to_its_last_whole_second() {
        assert_eq!(whole_seconds(1_461_000), 1461);
        // The running time rarely divides evenly, and the fraction left over is not
        // worth a request that says 1462 seconds of a 1461-second episode.
        assert_eq!(whole_seconds(1_461_999), 1461);
        // Crunchyroll sends no running time for some episodes, and there is no end to
        // put a playhead at. `mark` refuses that rather than sending the zero, which
        // would mean unwatched.
        assert_eq!(whole_seconds(0), 0);
        // And a nonsense duration is held at the last second there is rather than
        // wrapping round to a playhead near the start.
        assert_eq!(whole_seconds(u64::MAX), u32::MAX);
    }

    /// The notice names the episode by the number printed beside it, which for a special
    /// is not a number at all - and "ESP" beats "E0", which is what the number field has
    /// to say about one.
    #[test]
    fn an_episode_is_named_the_way_the_column_names_it() {
        let special = SeasonEpisode {
            episode: "SP".to_owned(),
            episode_number: 0,
            ..SeasonEpisode::default()
        };
        assert_eq!(episode_label(&special), "ESP");
        assert_eq!(
            episode_label(&SeasonEpisode {
                episode: String::new(),
                episode_number: 4,
                ..SeasonEpisode::default()
            }),
            "E4"
        );
    }

    #[test]
    fn cursor_stays_inside_a_pane() {
        let mut pane = Pane::default();
        pane.move_by(1);
        assert_eq!(pane.state.selected(), None, "an empty pane has no cursor");

        pane.set(vec!["a", "b", "c"]);
        assert_eq!(pane.state.selected(), Some(0));
        pane.move_by(-5);
        assert_eq!(pane.state.selected(), Some(0));
        pane.move_by(10);
        assert_eq!(
            pane.state.selected(),
            Some(2),
            "a page down stops at the end"
        );
        assert_eq!(pane.selected(), Some(&"c"));

        pane.select_edge(false);
        assert_eq!(pane.state.selected(), Some(0));
        pane.clear();
        assert_eq!(pane.selected(), None);
    }

    /// A click is aimed at the frame that was drawn before it, and the list may have been
    /// answered again in between, so the row it names can be one the list no longer has.
    #[test]
    fn a_row_the_list_no_longer_has_moves_nothing() {
        let mut pane = Pane::default();
        pane.set(vec!["a", "b", "c"]);
        assert_eq!(pane.window(), (0, 3));

        pane.select(2);
        assert_eq!(pane.state.selected(), Some(2));
        pane.select(7);
        assert_eq!(
            pane.state.selected(),
            Some(2),
            "the cursor stayed where it was"
        );

        pane.clear();
        pane.select(0);
        assert_eq!(pane.state.selected(), None, "an empty pane has no row 0");
        assert_eq!(pane.window(), (0, 0));
    }
}
