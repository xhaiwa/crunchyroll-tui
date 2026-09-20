use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;
use ratatui::widgets::ListState;

use crate::api::Page;
use crate::download::{DownloadOptions, OnDisk, episode_info, on_disk};
use crate::model::{CatalogItem, Opens, Playhead, Season, SeasonEpisode, opens, single_name};
use crate::util::{LANGUAGES, language_name};

use super::QUALITIES;
use super::art::Gallery;
use super::keys::{Bindings, Command};
use super::mouse::{self, Regions, Target};
use super::theme::Theme;
use super::worker::{
    Choice, FilterKind, Filters, Listing, Queued, Request, Response, Update, Worker,
};

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

/// What the seasons column calls a season: its own title, unless it has none or is
/// carrying the series' title, which the column to its left is already showing. The row
/// and the narrowing read a season the same way, so what is typed is matched against
/// what is on the screen rather than against what Crunchyroll happened to send.
///
/// The one row a film or a concert fills that column with is called what it is - `Film`,
/// `Music` - rather than claiming a season number it has not got. Its title would only
/// repeat the column to the left, which is the same rule the seasons themselves follow.
pub fn season_title(season: &Season, series_title: &str) -> String {
    if let Some(word) = single_name(&season.kind) {
        word.to_owned()
    } else if season.title.is_empty() || season.title == series_title {
        format!("Season {}", season.season_number)
    } else {
        season.title.clone()
    }
}

/// What a notice calls an episode: the number above, behind the E the episode column
/// shows. The title would be truer to the episode, but it is the number the user just
/// moved the cursor onto, and a sentence on the status line has no room for both.
///
/// A film has no number to be called by, and the one row it fills the column with is the
/// row every one of these sentences is about, so there it is the title that goes in.
/// `Marked Suzume watched` is the thing to say: `Marked E1 watched` reads back a number
/// nobody gave it, and `Marked Film watched` is what the column calls the row said out
/// loud where it makes no sense.
fn episode_label(episode: &SeasonEpisode) -> String {
    match single_name(&episode.kind) {
        Some(word) if episode.title.is_empty() => word.to_owned(),
        Some(_) => episode.title.clone(),
        None => format!("E{}", episode_number(episode)),
    }
}

/// How much of the season is marked, as the status line says it.
///
/// "nothing marked" rather than "0 episodes marked", because the sentence it ends is
/// read after unmarking the last one, and a count of zero is a thing to work out rather
/// than an answer.
fn marks_tally(count: usize) -> String {
    match count {
        0 => "nothing marked".to_owned(),
        1 => "1 episode marked".to_owned(),
        many => format!("{many} episodes marked"),
    }
}

/// What the status line says about a filter that cannot reach the list in front of it.
///
/// One clause for both of the moments it is needed - a filter set from the watchlist,
/// and the column cycled off the catalogue with one on - because they are the same fact
/// and hearing it two ways would read as two different rules.
fn only_the_catalogue(what: &str) -> String {
    format!("{what} - the filters narrow the catalogue only.")
}

/// What a row of the catalogue column reads as, for the narrowing to match against and
/// for a page arriving to be held to.
///
/// Out here rather than inside [`App::narrow`] with the other three columns' words, and
/// for the one reason worth moving it for: the catalogue is the column that grows a page
/// at a time, so [`Pane::append`] needs the same reading of a row that the narrowing was
/// typed against. Two spellings of it could drift, and the drift would show as rows
/// appearing under a query that does not hold them.
fn series_text(series: &CatalogItem) -> String {
    series.title.clone()
}

/// Whether a row holds what was typed into the filter box.
///
/// A plain substring rather than the subsequence fzf matches with, and the reason is
/// the order of these lists. fzf can afford to let `tje` find `The Journey Ends`,
/// because it sorts what it matched by how well it matched and the best line ends up
/// under the cursor; this cannot sort, because the order of a column here is a fact
/// about it - a season is numbered, the queue is the order things were asked for - and
/// a list shuffled by a score is a list nobody can read down. Left in their own order,
/// a subsequence match is barely a filter: three letters scattered anywhere in a title
/// keep most of a season, and a narrowing that does not narrow is worse than none.
/// Every row a substring leaves standing visibly holds what was typed, which is the
/// whole of the explanation anyone needs for why it is still there.
///
/// Case is ignored until the query carries one, which is vim's smartcase and fzf's
/// default: `ed` finds `Ed` and `wanted` alike, and `Ed` finds only the first. Someone
/// typing in lower case is typing quickly, and a capital is someone being specific.
fn matches(row: &str, query: &str) -> bool {
    if query.chars().any(char::is_uppercase) {
        row.contains(query)
    } else {
        row.to_lowercase().contains(query)
    }
}

/// What a column has been narrowed to: what was typed, and the rows of the list it
/// leaves standing, in the order the list had them.
struct Narrowing {
    query: String,
    rows: Vec<usize>,
}

/// One column: what it holds, where the cursor is, and whether it is still waiting.
///
/// The list is kept whole and a narrowing is a list of the rows of it that are showing,
/// which is the one arrangement the cursor cannot come adrift from. Everything on the
/// screen counts in rows rather than in items - `state.selected()` is a row, the list
/// widget is handed the rows and writes its own offset back in them, and the pointer
/// turns a line of the terminal into one - so there is a single index space up here,
/// and [`Pane::selected`] is the only place it is turned back into an item. Drawing the
/// whole list and skipping the hidden rows as they went past was the other way to do
/// it, and it would have left the widget's offset counting one thing while the cursor,
/// the marks and the mouse counted another.
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
    /// What this column has been narrowed to, while it is narrowed to anything. Each
    /// column keeps its own, which is why it lives here rather than beside the box that
    /// types it: walking over to the seasons and back has no business taking a
    /// narrowing along, or dropping the one that is already there.
    narrowing: Option<Narrowing>,
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
            narrowing: None,
        }
    }
}

impl<T> Pane<T> {
    /// What was typed into this column's filter box, while a narrowing is on it.
    pub fn query(&self) -> Option<&str> {
        self.narrowing
            .as_ref()
            .map(|narrowing| narrowing.query.as_str())
    }

    /// How many rows the column is showing: all of them, or what the narrowing left.
    pub fn rows(&self) -> usize {
        self.narrowing
            .as_ref()
            .map_or(self.items.len(), |narrowing| narrowing.rows.len())
    }

    /// The item a row of the column is drawn from.
    fn item_of(&self, row: usize) -> Option<usize> {
        match &self.narrowing {
            Some(narrowing) => narrowing.rows.get(row).copied(),
            None => (row < self.items.len()).then_some(row),
        }
    }

    /// Which row an item is on, where the narrowing leaves it showing at all.
    fn row_of(&self, index: usize) -> Option<usize> {
        match &self.narrowing {
            Some(narrowing) => narrowing.rows.iter().position(|shown| *shown == index),
            None => (index < self.items.len()).then_some(index),
        }
    }

    /// The items the column is showing, in the order it shows them: what is drawn, and
    /// what the keys that mean "everything in this column" act on.
    pub fn shown(&self) -> Vec<&T> {
        match &self.narrowing {
            Some(narrowing) => narrowing
                .rows
                .iter()
                .filter_map(|index| self.items.get(*index))
                .collect(),
            None => self.items.iter().collect(),
        }
    }

    /// Where the cursor is in the whole list, which is what anything kept alongside the
    /// list - a playhead, a mark, the cursor a reload puts back - is keyed by.
    pub fn selected_index(&self) -> Option<usize> {
        self.state.selected().and_then(|row| self.item_of(row))
    }

    pub fn selected(&self) -> Option<&T> {
        self.selected_index()
            .and_then(|index| self.items.get(index))
    }

    /// Narrows the column to the rows whose words hold `query`, or widens it again when
    /// the query is empty. `text` reads a row back as the words it shows, which is what
    /// the user is typing at: see [`App::narrow`], which knows what the rows of each
    /// column say and is the only caller.
    ///
    /// The cursor follows the item it was on wherever the query leaves it standing, and
    /// falls to the first row where it does not. It cannot simply be left where it was:
    /// a cursor on a row nobody can see is the one thing this must not do, since what
    /// is played, what is queued and what a click lands on are all read off it.
    pub fn narrow(&mut self, query: &str, text: impl Fn(&T) -> String) {
        let was_on = self.selected_index();
        self.narrowing = (!query.is_empty()).then(|| Narrowing {
            query: query.to_owned(),
            rows: self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| matches(&text(item), query))
                .map(|(index, _)| index)
                .collect(),
        });
        let landed = was_on.and_then(|index| self.row_of(index));
        self.state
            .select(landed.or_else(|| (self.rows() > 0).then_some(0)));
    }

    /// Takes in a list, with the cursor put back where a reload asked for it.
    ///
    /// A narrowing does not survive this, whichever of the ways the column came to be
    /// filled again: a season opened, a reload, a change of language, another catalogue
    /// listing. What was typed is a claim about the words that were on the screen when
    /// it was typed, and none of those hands the same words back - a new season has
    /// rows nobody has typed at, and a change of language brings the same episodes back
    /// under different titles, where a query typed against the English ones matches
    /// nothing. Either would leave a column looking empty for a reason three keystrokes
    /// in the past with nothing on the screen still saying so. The cursor and the marks
    /// are carried across a reload, and they are the argument for this rather than
    /// against it: those name a row that can be found again, while a narrowing names
    /// text that is about to be replaced.
    pub fn set(&mut self, items: Vec<T>) {
        self.narrowing = None;
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
        self.narrowing = None;
        self.state.select(None);
        self.loading = false;
        self.error = None;
        self.owner.clear();
        self.pending_cursor = None;
    }

    pub fn move_by(&mut self, delta: isize) {
        let rows = self.rows();
        if rows == 0 {
            return;
        }
        let last = rows as isize - 1;
        let current = self.state.selected().unwrap_or(0) as isize;
        self.state
            .select(Some(current.saturating_add(delta).clamp(0, last) as usize));
    }

    pub fn select_edge(&mut self, last: bool) {
        let rows = self.rows();
        if rows == 0 {
            return;
        }
        self.state.select(Some(if last { rows - 1 } else { 0 }));
    }

    /// Puts the cursor on `row`, and ignores one past the end: a list answered again
    /// since the frame a click was aimed at may be shorter than that frame said it was,
    /// and so may a narrowing typed since.
    pub fn select(&mut self, row: usize) {
        if row < self.rows() {
            self.state.select(Some(row));
        }
    }

    /// Adds to what the column is already showing, for a list that arrives a page at a
    /// time.
    ///
    /// The cursor is left alone rather than put back at the top: the user is standing at
    /// the bottom of the list, which is what asked for these rows in the first place, and
    /// a column that jumped home every time a page landed would be unusable. A column
    /// that was empty until now gets a cursor, since a list with rows in it and nothing
    /// selected cannot be walked.
    ///
    /// A narrowing survives this, and the new rows are held to it - which is the opposite
    /// of what [`Pane::set`] does with one, for the opposite reason. A page is the same
    /// list going on, with the same words on the screen the query was typed against, so
    /// the query still means what it meant; and a page arriving unnarrowed would append a
    /// hundred rows that do not match to the bottom of a column whose title says it is
    /// showing four. The rows already there keep their places, so the cursor keeps its
    /// row.
    pub fn append(&mut self, items: Vec<T>, text: impl Fn(&T) -> String) {
        self.items.extend(items);
        self.loading = false;
        if let Some(query) = self.query().map(str::to_owned) {
            let cursor = self.state.selected();
            self.narrow(&query, text);
            // `narrow` puts the cursor back on the item it was on, which is the row it was
            // already on here, since nothing before this page moved. Said outright anyway:
            // a page landing must not move the cursor, and the arithmetic that says so is
            // in another method.
            if let Some(row) = cursor {
                self.state.select(Some(row));
            }
        }
        if self.state.selected().is_none() && self.rows() > 0 {
            self.state.select(Some(0));
        }
    }

    /// The first row the list drew and how many rows it is showing - everything the
    /// pointer needs to turn a row of the screen into one of the column's. The offset is
    /// what the list widget wrote back as it drew, so it is where the list actually was
    /// rather than where it was asked to be, and the count is what the widget was handed:
    /// a narrowed column answers for what it narrowed to, which is what is on the screen.
    pub fn window(&self) -> (usize, usize) {
        (self.state.offset(), self.rows())
    }
}

/// How far through its list the catalogue column has read, and how much further there is
/// to go.
///
/// The offsets are kept here rather than counted off the rows on screen, because the two
/// do not always agree: a watchlist page of a hundred rows is a hundred offsets whether or
/// not two of them were rows this column has nowhere to open, and a page the simulcast
/// filter has sieved is shorter still. Asking for the next page at `items.len()` would
/// hand back rows already on screen a page later.
pub struct Paging {
    /// Where the next page begins, or `None` once the list has been read to the end -
    /// which a listing that is not paged at all says of its very first page.
    pub next: Option<usize>,
    /// The offset of the page asked for and not yet answered, if there is one. It is
    /// both the flag that keeps a second request from going out while the first is in
    /// flight and the name the answer is recognised by: a page whose offset is not this
    /// one belongs to a list the user has left or to a page already appended, and is
    /// dropped the way [`Pane::owner`] drops an answer about the wrong series.
    pub asked: Option<usize>,
    /// How many series the whole list holds, where the endpoint counts the same things
    /// the column shows. The header prints it against what is loaded; see
    /// [`crate::api::Page`] for why most of the lists have nothing to put here.
    pub total: Option<usize>,
}

impl Paging {
    /// A list about to be read from the top. Nothing is known about its length yet, and
    /// the first page is the one at offset zero.
    fn first() -> Self {
        Self {
            next: Some(0),
            asked: None,
            total: None,
        }
    }
}

/// Which list is open over the interface, and so what choosing a row from it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picking {
    Audio,
    Subtitles,
    /// One of the two filters that take their value from a list Crunchyroll keeps.
    Filter(FilterKind),
}

impl Picking {
    /// The list `next-column` looks at from this one. The four are two pairs that are
    /// read together - the two languages, and the two filters - and flipping between a
    /// pair without going back out first is what that key was doing when there was only
    /// one pair of them.
    const fn other(self) -> Self {
        match self {
            Self::Audio => Self::Subtitles,
            Self::Subtitles => Self::Audio,
            Self::Filter(FilterKind::Genre) => Self::Filter(FilterKind::Season),
            Self::Filter(FilterKind::Season) => Self::Filter(FilterKind::Genre),
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Audio => " Audio language ",
            Self::Subtitles => " Subtitle language ",
            Self::Filter(which) => which.title(),
        }
    }
}

/// A list of values while it is open: which list it is, and what it is offering.
///
/// It was the language list and is now four lists, because they are the same thing to
/// look at and the same thing to drive: a column of values with the one in force marked,
/// the keys of a pane, escape to leave it alone. Each row carries the value that gets
/// applied beside the words it is shown as, so nothing about the list itself depends on
/// where its rows came from - a locale this program has a name for, or a category
/// Crunchyroll named in the account's own language.
pub struct Picker {
    pub kind: Picking,
    pub pane: Pane<Choice>,
}

impl Picker {
    pub const fn title(&self) -> &'static str {
        self.kind.title()
    }
}

/// The box along the top while something is being typed into it, and what a return
/// will mean when it is.
///
/// One box rather than two fields that must never both be filled in: a letter typed
/// while either of them is open belongs in that box and nowhere else, and the event
/// loop has one question to ask rather than two that could disagree.
pub enum Editing {
    /// `search`: a query for Crunchyroll. Nothing goes anywhere until the return, and
    /// then the answer replaces the catalogue column.
    Search(String),
    /// `filter`: a narrowing of the column named here, which happens as each letter
    /// lands and asks nobody anything. Which column is written down when the key is
    /// pressed rather than read off the focus as each letter arrives, so the box and
    /// the column it is narrowing cannot come apart.
    Narrow { focus: Focus, query: String },
}

impl Editing {
    /// What is in the box.
    pub fn query(&self) -> &str {
        match self {
            Self::Search(query) | Self::Narrow { query, .. } => query,
        }
    }

    /// Takes a letter, or a backspace, and says whether the box reads differently
    /// afterwards - which is when a narrowing has to be worked out again.
    fn typed(&mut self, key: KeyEvent) -> bool {
        let query = match self {
            Self::Search(query) | Self::Narrow { query, .. } => query,
        };
        match key.code {
            KeyCode::Backspace => query.pop().is_some(),
            KeyCode::Char(letter) => {
                query.push(letter);
                true
            }
            _ => false,
        }
    }
}

/// What a column is showing and what it holds, where it has been narrowed at all.
/// Nothing when it has not: there is nothing to report about a column showing
/// everything it has.
fn narrowed_to<T>(pane: &Pane<T>) -> Option<(usize, usize)> {
    pane.query()
        .is_some()
        .then(|| (pane.rows(), pane.items.len()))
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
    /// Which episodes of the open season are already on this disk, by content id. Only
    /// the ones that are: a row with no entry here is a row with no file. Emptied with
    /// the episodes for the same reason the playheads are, and looked up again whenever
    /// the answer could have changed - a change of quality, since the quality is part of
    /// the file name, and the moments a queued download changes what is on the disk.
    pub downloaded: HashMap<String, OnDisk>,
    /// The episodes `download` is to queue, by id, rather than the one under the cursor.
    ///
    /// By id and not by index, because the one thing a mark has to survive is the list
    /// being handed back again - a reload, or the same season asked for in another
    /// language - and those can come back a different length, where an index would have
    /// slid onto whichever episode had moved into its place. An id the open list does
    /// not have marks nothing at all, which is what makes carrying the set over those
    /// two safe when carrying an index over them would not be.
    ///
    /// This is the one of the three maps beside the episodes that is carried across a
    /// reload: the playheads and the disk markers are answers about the season, and are
    /// asked again, while a mark is something the user put there and nobody else can
    /// give back.
    pub marked: HashSet<String>,
    pub listing: Listing,
    /// Where the catalogue column has got to in that listing.
    pub paging: Paging,
    /// What the browse listings are narrowed to. Carried on the interface rather than
    /// inside the listing, because it survives the column being cycled round the ring
    /// and put back - see [`App::choose_filter`].
    pub filters: Filters,
    /// The values behind the two filters that take one, once they have been asked for.
    ///
    /// Kept for the run rather than fetched each time a list is opened. Crunchyroll's
    /// categories change a few times a year and its seasons four times, which is not
    /// between two presses of a key, and the list is one a user narrowing a catalogue
    /// opens, closes and opens again. A list that failed to arrive leaves nothing here,
    /// so the next press asks afresh rather than offering an empty list for ever.
    filter_values: HashMap<FilterKind, Vec<Choice>>,
    /// The box along the top while it is being typed into: the search, or a column
    /// being narrowed.
    pub editing: Option<Editing>,
    /// The list of values while one is open.
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
            downloaded: HashMap::new(),
            marked: HashSet::new(),
            // The most useful first screen a video client has is the thing that was
            // being watched last, so that is what the interface opens on. An account
            // with no history, or a request that fails, falls back to the catalogue
            // when the answer arrives.
            listing: Listing::History,
            paging: Paging::first(),
            filters: Filters::default(),
            filter_values: HashMap::new(),
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
    ///
    /// All three of the maps beside the episodes go, and for the same reason: they
    /// belong to the season being put away rather than to the column they were drawn in.
    /// Whether any of them comes back afterwards is not settled here, because this is
    /// also the path a different season takes - the playheads and the disk markers are
    /// asked for again, and the marks are carried by the two callers that ask for the
    /// same season over again. See [`App::request_episodes_again`].
    fn clear_episodes(&mut self) {
        self.episodes.clear();
        self.playheads.clear();
        self.downloaded.clear();
        self.marked.clear();
    }

    /// Asks the disk which of the episodes now in the column are already here.
    ///
    /// Once per list rather than once per row per frame. The answer is two `stat` calls
    /// for each episode, the interface redraws ten times a second, and a row that asked
    /// as it was drawn would put a few hundred of them a second between the user and a
    /// screen that says the same thing every time. It is asked again at the moments the
    /// answer can change instead: a season arriving, a change of quality - the quality is
    /// written into the file name, and a 720p copy is not the 1080p one the downloader
    /// would write - and a queued download getting somewhere, which `download_moved`
    /// decides the moments of.
    fn look_on_disk(&mut self) {
        let quality = &self.options.video_quality;
        self.downloaded = self
            .episodes
            .items
            .iter()
            .filter_map(|episode| {
                let held = on_disk(&episode_info(episode), quality);
                (held != OnDisk::Missing).then(|| (episode.id.clone(), held))
            })
            .collect();
    }

    /// Starts the catalogue column over on the list it is showing.
    ///
    /// Every caller of this is a new list or the same list from scratch - the first
    /// screen, the fallback out of an empty history, a reload, a change of order, a
    /// search and the way back out of one - and all of them mean the first page. An
    /// offset carried over from the list being put away would open the next one halfway
    /// down, and its total would have the header counting against somebody else's list.
    /// Narrows a column to the rows that hold `query`, or widens it again when the
    /// query is empty.
    ///
    /// What a row reads as is decided here, because this is the only place that knows
    /// what each column draws. A series is its title. A season is the words its row
    /// shows, which are not always Crunchyroll's - a season with no title of its own is
    /// drawn as `Season 2`, and typing `season 2` has to find it. An episode is its
    /// number and its title, since `e12` and `journey` are both things someone would
    /// type at a season. A queue row is its number, its title and the series it came
    /// from: the queue outlives the column it was filled from, so the series is the
    /// only thing left on the row tying it to where it came from.
    fn narrow(&mut self, focus: Focus, query: &str) {
        match focus {
            Focus::Series => self.series.narrow(query, series_text),
            Focus::Seasons => {
                // Copied out first, because the row this is matching against is drawn
                // against the selected series and the two have to read alike.
                let series_title = self
                    .series
                    .selected()
                    .map_or_else(String::new, |series| series.title.clone());
                self.seasons
                    .narrow(query, |season| season_title(season, &series_title));
            }
            Focus::Episodes => self.episodes.narrow(query, |episode| {
                format!("E{} {}", episode_number(episode), episode.title)
            }),
            Focus::Downloads => self.downloads.narrow(query, |download| {
                format!("{} {} {}", download.number, download.title, download.series)
            }),
        }
    }

    /// Works the queue's narrowing out again against the list as it is now.
    ///
    /// The queue is the one column whose list is not handed back whole by a request but
    /// grows and shrinks a row at a time under the user's hands, so the rows a narrowing
    /// is holding are about to be pointing at the wrong episodes - or past the end. The
    /// seasons and the episodes have no equivalent: every list they ever get comes through
    /// [`Pane::set`], which drops the narrowing outright. The catalogue is the one column
    /// in between, since a page appended to it is the same list going on rather than
    /// another list, and [`Pane::append`] holds the new rows to the narrowing itself.
    fn renarrow_downloads(&mut self) {
        if let Some(query) = self.downloads.query().map(str::to_owned) {
            self.narrow(Focus::Downloads, &query);
        }
    }

    fn request_catalog(&mut self) {
        self.series.clear();
        self.seasons.clear();
        self.clear_episodes();
        self.paging = Paging::first();
        self.focus = Focus::Series;
        self.request_catalog_page(0);
    }

    /// Asks for one page of the list the column is showing, and writes down which one is
    /// being waited for.
    fn request_catalog_page(&mut self, start: usize) {
        self.series.loading = true;
        self.paging.asked = Some(start);
        self.worker.send(Request::Catalog {
            listing: self.listing.clone(),
            start,
            filters: self.filters.clone(),
        });
    }

    /// Asks for the page after the rows the column already holds.
    ///
    /// Nothing happens while an answer is outstanding, so holding `down` against the
    /// bottom of the list asks once rather than once per repeat; and nothing happens once
    /// the list has been read to the end, which is what a listing that cannot be paged
    /// says of its first page. A page that failed leaves `next` where it was, so the
    /// next time the cursor comes to rest on the last row it is asked for again - the
    /// user's own retry, rather than a loop that would hammer an endpoint that is down.
    fn request_next_catalog_page(&mut self) {
        if self.paging.asked.is_some() {
            return;
        }
        let Some(start) = self.paging.next else {
            return;
        };
        self.request_catalog_page(start);
    }

    /// Asks for the next page once the cursor has come to rest on the last row the
    /// catalogue column holds.
    ///
    /// Every way of moving a cursor ends up here rather than a key of its own: `down`
    /// clamped against the bottom, a `page-down` that overshot, `bottom`, the wheel and a
    /// drag all put the cursor somewhere through [`App::pane_move`], [`App::put_cursor`]
    /// or [`App::focused_pane_edge`], and the list growing when you reach the end of it
    /// is not a thing anyone should have to ask for by name. Only the catalogue: the
    /// seasons and the episodes of one season arrive whole, and the queue is this run's
    /// own doing.
    fn catalog_page_if_at_the_end(&mut self, focus: Focus) {
        if focus != Focus::Series {
            return;
        }
        let Some(cursor) = self.series.state.selected() else {
            return;
        };
        // Rows rather than items, which is what the cursor counts. A narrowed catalogue is
        // the case that makes the difference: the last row a narrowing left is the end of
        // what the user can walk to, and it is exactly where the next page is worth
        // asking for - a query that kept four rows out of a hundred has hidden ninety-six
        // series the user is not looking for and says nothing about the hundred behind
        // them.
        if cursor + 1 >= self.series.rows() {
            self.request_next_catalog_page();
        }
    }

    /// Opens the row the catalogue column is on into the middle column.
    ///
    /// A series opens into its seasons, which is the request this has always been. A film
    /// has no seasons endpoint and no episodes endpoint behind it, and a concert has
    /// nothing underneath it at all, so for those the middle column holds one row standing
    /// for the thing itself and the right column is asked for behind it straight away.
    /// There is nothing to choose in a column with one row in it, and leaving the cursor
    /// there would put a keypress between every film and playing it for the sake of a
    /// decision nobody has to make. The shape still holds - `back` comes out of a film
    /// into the catalogue exactly as it comes out of a season - and the column says what
    /// it is holding rather than pretending to a season: see [`App::open_single`].
    fn request_seasons(&mut self) {
        let Some(series) = self.series.selected().cloned() else {
            return;
        };
        match series.opens() {
            Some(Opens::Seasons) => {
                let series_id = series.id;
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
            Some(Opens::Films | Opens::Itself) => self.open_single(&series),
            // Every list that fills this column drops what it cannot open, so nothing
            // should reach here. A row that arrived by some road nobody has thought of
            // yet is still better answered than left to do nothing when it is opened.
            None => self.complain(format!(
                "Crunchyroll calls {} a {}, which this client has nothing to open.",
                series.title, series.kind
            )),
        }
    }

    /// The middle column for a catalogue row that is not a series: one row, standing for
    /// the film or the concert itself, with the right column asked for behind it.
    ///
    /// The row carries the catalogue entry's type, and that is the whole of what anything
    /// afterwards needs to know about it: it is why the column is titled `Film`, and why
    /// opening that row asks for the films inside a listing rather than the episodes of a
    /// season. Carried on the row rather than read off the catalogue cursor a second time,
    /// so that it is still the right answer once the cursor has moved on.
    fn open_single(&mut self, item: &CatalogItem) {
        self.seasons.clear();
        self.clear_episodes();
        self.seasons.owner = item.id.clone();
        self.seasons.set(vec![Season {
            id: item.id.clone(),
            kind: item.kind.clone(),
            title: item.title.clone(),
            ..Season::default()
        }]);
        self.request_episodes();
    }

    /// The one row a concert or a music video fills the episodes column with.
    ///
    /// Built here rather than asked for: what the catalogue row names is already the thing
    /// that plays, and a column left sitting on `Loading...` waiting for an answer nobody
    /// is going to send would be a poor way of saying so. The blurb and the still come off
    /// the catalogue entry while it is still there to be read - a catalogue replaced by
    /// another list in the meantime costs the panel its description and nothing that
    /// plays.
    ///
    /// The numbers are the ones `api::films` settles on for a film, and for the same
    /// reasons: season one, episode one, and the thing named twice - once as what the file
    /// goes under, once as its own title.
    fn single_row(&self, season: &Season) -> SeasonEpisode {
        let listed = self.series.items.iter().find(|item| item.id == season.id);
        SeasonEpisode {
            id: season.id.clone(),
            kind: season.kind.clone(),
            season_number: 1,
            episode_number: 1,
            series_title: season.title.clone(),
            title: season.title.clone(),
            // Nothing here says what a concert's audio is, and the downloader needs a
            // locale to hang its one stream on: the locale it was asked for is the only
            // answer there is, and an empty one would make every concert unplayable.
            audio_locale: self.audio(),
            description: listed
                .map(|item| item.description.clone())
                .unwrap_or_default(),
            images: listed.map(|item| item.images.clone()).unwrap_or_default(),
            ..SeasonEpisode::default()
        }
    }

    fn request_episodes(&mut self) {
        let Some(season) = self.seasons.selected().cloned() else {
            return;
        };
        let season_id = season.id.clone();
        self.clear_episodes();
        self.episodes.owner = season_id.clone();
        self.focus = Focus::Episodes;
        match opens(&season.kind) {
            // A film's listing, whose films are a question of their own and come back as
            // an episodes answer like any other.
            Some(Opens::Films) => {
                self.episodes.loading = true;
                self.worker.send(Request::Movies {
                    listing_id: season_id,
                    audio: self.audio(),
                    subs: self.subs(),
                });
            }
            // Already the thing that plays, so the column is filled from here. The
            // playheads have to be asked for by hand: that request usually rides on the
            // back of an answer, and without it this would be the one row in the interface
            // that could not say where the account had got to in it.
            Some(Opens::Itself) => {
                let row = self.single_row(&season);
                let episode_ids = vec![row.id.clone()];
                self.episodes.set(vec![row]);
                self.look_on_disk();
                self.worker.send(Request::Playheads {
                    season_id,
                    episode_ids,
                });
            }
            Some(Opens::Seasons) | None => {
                self.episodes.loading = true;
                self.worker.send(Request::Episodes {
                    season_id,
                    audio: self.audio(),
                    subs: self.subs(),
                });
            }
        }
    }

    /// Asks for the season that is already open a second time, keeping what the user put
    /// on it.
    ///
    /// A reload and a language change are the two ways the episode list is replaced by
    /// another list of the same season, and the cursor is already carried across both
    /// because it is still the same season being looked at. The marks are the same
    /// argument, only louder: the cursor is one keypress to put back and five marks are
    /// five, and someone who marks half a season and then notices it is offering the sub
    /// has asked for another dub rather than for their marks to be swept up. They are
    /// safe to carry because they name episodes by id, so a list that comes back without
    /// one of them simply has nothing marked there.
    fn request_episodes_again(&mut self) {
        let cursor = self.episodes.selected_index();
        let marks = std::mem::take(&mut self.marked);
        self.request_episodes();
        self.episodes.pending_cursor = cursor;
        self.marked = marks;
    }

    /// A page of the catalogue, and the one decision the opening screen still has to
    /// make.
    ///
    /// The interface asks for Continue watching first, which is the best thing to open
    /// on right up until the account has never watched anything, or the request fails:
    /// an empty column, or an error where the catalogue should be, is a worse first
    /// screen than Popular. So this is where that is noticed - the only place the answer
    /// is known - and the catalogue is asked for instead, once, with the status line
    /// saying so, since a column showing a different list from the one that was asked
    /// for has no business doing it quietly.
    ///
    /// The page is checked against the list on screen and against the offset the column
    /// is waiting for, which are two different questions. The first is the pane owner's
    /// job done for a list rather than a series: an answer for a list the user has since
    /// cycled past has nothing to do with what is drawn now. The second is what keeps a
    /// page from being appended twice - a request that was still in flight when the list
    /// was started over comes back looking like page one of a list that is indeed the
    /// one on screen, and appending it would show the first hundred series twice.
    ///
    /// Split out of [`App::drain`] so a test can hand the interface an answer without a
    /// worker behind it.
    fn catalog_arrived(
        &mut self,
        listing: Listing,
        start: usize,
        filters: Filters,
        result: Result<Page, String>,
    ) {
        // Three things have to match, and they are the three the request carried. The
        // filters are half of what was asked, so they are half of what makes an answer
        // stale: a page of the whole catalogue arriving after a genre has been chosen is
        // as much the answer to a question nobody is asking any more as a page of the
        // watchlist would be. The offset is the other half - see [`Paging::asked`].
        if listing != self.listing || filters != self.filters || self.paging.asked != Some(start) {
            return;
        }
        self.paging.asked = None;
        // The first page replaces what the column held and every page after it is added
        // to the end, which is also why only the first one restores a cursor and only
        // the first one can send the interface looking for another list: a later page is
        // an addition to a column that already has something worth reading in it.
        let first = start == 0;
        let nothing_to_show =
            first && (result.as_ref().is_ok_and(|page| page.items.is_empty()) || result.is_err());
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
            Ok(page) => {
                self.paging.total = page.total;
                self.paging.next = page.next;
                let count = page.items.len();
                if first {
                    self.series.set(page.items);
                    if count == 0 {
                        self.say("No series found.");
                    }
                } else {
                    self.series.append(page.items, series_text);
                }
            }
            Err(error) => {
                self.series.loading = false;
                if first {
                    self.series.error = Some(error.clone());
                    self.complain(error);
                } else {
                    // The rows already loaded are still perfectly good, and throwing
                    // away a hundred series the user has been walking through because
                    // the hundred-and-first could not be fetched would be a strange way
                    // to report it. So the column is left as it is and the status line
                    // carries the news, which is where everything else that fails
                    // quietly in the background says so.
                    self.complain(format!("Could not load more series ({error})"));
                }
            }
        }
    }

    /// The values behind one of the two list filters.
    ///
    /// Kept whatever is on screen by the time they arrive: the categories are a fact
    /// about Crunchyroll rather than about the column, and an answer thrown away because
    /// the list had been closed would only be asked for again the next time it was
    /// opened. What the answer may not do is reach into a list the user has moved on
    /// from - the discipline every other answer here keeps - so the open list is
    /// refilled only where it is the one that asked, and a list since closed, or swapped
    /// for the other filter, is left exactly as it was found.
    ///
    /// A list that failed leaves the popup showing All, which still clears the filter,
    /// and says why on the status line. The alternative - closing the popup out from
    /// under the user - would take away the one row it did have to offer. A failure for
    /// a list nobody is looking at any more is dropped like any other stale answer,
    /// since there is nothing left on screen for it to explain.
    fn filter_values_arrived(&mut self, which: FilterKind, result: Result<Vec<Choice>, String>) {
        let open = self.picker.as_ref().map(|picker| picker.kind) == Some(Picking::Filter(which));
        match result {
            Ok(values) => {
                self.filter_values.insert(which, values);
                if open {
                    self.open_picker(Picking::Filter(which));
                }
            }
            Err(error) if open => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.pane.loading = false;
                }
                self.complain(error);
            }
            Err(_) => {}
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
            Response::Catalog {
                listing,
                start,
                filters,
                result,
            } => self.catalog_arrived(listing, start, filters, result),
            Response::FilterValues { which, result } => self.filter_values_arrived(which, result),
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
                        self.look_on_disk();
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
    ///
    /// The disk changes under the episodes column while this is going on, though, and
    /// without asking it again the marker would be telling the truth about the moment the
    /// season was opened and nothing since: an episode queued here would sit unmarked
    /// until the column was reloaded, which is the reload this whole feature exists to
    /// save. Three of these answers are worth the question. The end of a download leaves
    /// the episode under its finished name. `Started` is sent as the thread takes the
    /// episode up, before a byte has been written, so it catches what an earlier run left
    /// and nothing else - but the first part to report has got far enough to have written
    /// the state file, which is what makes a download that began from nothing read as
    /// partial while it runs. Every report after that is the same file under the same
    /// name getting bigger, and they arrive several times a second.
    ///
    /// The whole of the open season is asked again rather than the one episode. The queue
    /// outlives the column it was filled from - a row carries what it is rather than which
    /// episode it is, on purpose - so finding the one row would mean an episode id on the
    /// queue, paid for in the queue's own design to save a season's worth of `stat` calls
    /// three times per download. An episode downloaded from a season nobody is looking at
    /// finds nothing of itself in the column, which is the right answer rather than a
    /// missing one.
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
        let touched_the_disk = match &update {
            Update::Started | Update::Finished(_) => true,
            Update::Stage { .. } => download.stages.is_empty(),
        };
        download.update(update);
        if touched_the_disk {
            self.look_on_disk();
        }
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
        self.catalog_page_if_at_the_end(focus);
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
        self.catalog_page_if_at_the_end(focus);
    }

    /// Which row of a column the pointer is on, if it is on one rather than on a border,
    /// a title, or the sentence an empty column draws. A row rather than an item, since
    /// the column may be narrowed and the cursor counts in rows - see [`Pane`].
    fn row_at(&self, focus: Focus, row: u16) -> Option<usize> {
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
        self.catalog_page_if_at_the_end(self.focus);
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
    ///
    /// The rest of the season is the rest of what the column is showing. A narrowing is
    /// what the user is looking at, and a key pressed against a screen holding three
    /// episodes must not hand mpv the twenty-one it is hiding - which is the same
    /// reason the cursor may only ever be on a row that is showing.
    fn selection(&mut self, to_end: bool) -> Vec<SeasonEpisode> {
        let Some(row) = self.episodes.state.selected() else {
            self.nothing_to_act_on();
            return Vec::new();
        };
        let shown = self.episodes.shown();
        let taken = if to_end {
            shown.get(row..)
        } else {
            shown.get(row..=row)
        };
        taken
            .unwrap_or_default()
            .iter()
            .map(|episode| (*episode).clone())
            .collect()
    }

    /// What to say when the episodes column has nothing under the cursor. A column with
    /// no season in it and a column narrowed until nothing is left are two different
    /// problems, and only one of them is answered by opening a season.
    fn nothing_to_act_on(&mut self) {
        self.complain(if self.episodes.items.is_empty() {
            "Open a season first."
        } else {
            "Nothing in this season matches what the column was narrowed to."
        });
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
            self.nothing_to_act_on();
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

    /// Puts a mark on the episode under the cursor, or takes the one that is there off.
    ///
    /// The keyboard has to be in the episodes column for this, where `play`, `download`
    /// and `mark-watched` all act on the episode under the cursor from wherever it
    /// happens to be. The difference is that those three do something and say so on the
    /// status line, while this one leaves a mark behind on a row that a user reading the
    /// catalogue column cannot see: a key that quietly decorated a list three columns
    /// away would be a poor thing to have to discover. The Downloads panel is the same
    /// argument twice over, since its rows are episodes too and a mark landing there
    /// would look as though it meant something about the queue. So pressed anywhere but
    /// the episodes column it says which column it belongs to, rather than being a key
    /// that does nothing on three panels out of four.
    fn toggle_mark(&mut self) {
        if self.focus != Focus::Episodes {
            self.complain("Marking is the episodes column's key.");
            return;
        }
        let Some(episode) = self.episodes.selected() else {
            self.nothing_to_act_on();
            return;
        };
        let episode_id = episode.id.clone();
        let label = episode_label(episode);
        let verb = if self.marked.remove(&episode_id) {
            "Unmarked"
        } else {
            self.marked.insert(episode_id);
            "Marked"
        };
        // Counted through the open list rather than off the set, so that a mark carried
        // across a list that came back without its episode is not counted for a row
        // nobody can see.
        let tally = marks_tally(self.marked_episodes().len());
        self.say(format!("{verb} {label} - {tally}."));
    }

    /// The marked episodes, in the order the season lists them.
    ///
    /// The set behind them remembers that a mark was put on an id and nothing else, and
    /// this is the reason it needs to remember nothing else: what comes back is the
    /// column read top to bottom, so four episodes marked in whatever order they caught
    /// the eye are queued in the order they are meant to be watched in. Queueing them in
    /// the order they were pressed would put that order into the Downloads panel, where
    /// it would sit for the rest of the hour as the only account of what was asked for,
    /// reading back something no longer visible anywhere on screen.
    ///
    /// The whole season rather than what a narrowing is showing of it, which is the
    /// opposite of what `download-season` does and is right for the opposite reason. A
    /// mark is something the user put on an episode by hand, and a narrowing typed
    /// afterwards is a way of looking at the column: hiding a marked row does not
    /// unmark it, and a `d` that queued only the marks that happened to be on the
    /// screen would let a query typed later decide what four deliberate presses meant.
    fn marked_episodes(&self) -> Vec<SeasonEpisode> {
        self.episodes
            .items
            .iter()
            .filter(|episode| self.marked.contains(&episode.id))
            .cloned()
            .collect()
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
    ///
    /// What the selection is, is the only thing marking changes here. `download` means
    /// the marks when there are any and the episode under the cursor when there are
    /// none, so the key does not have to be learnt twice: nothing is marked until
    /// somebody marks something, and until then it is the key it always was.
    /// `download-season` is left alone - the whole season is the one request that cannot
    /// be meant by a handful of marks, and it stays the way to ask for it.
    fn queue_downloads(&mut self, whole_season: bool) {
        let (episodes, from_marks) = if whole_season {
            // What is showing rather than what the season holds. `download-season` says
            // the column, and while a narrowing is on it the column is what the
            // narrowing left: queueing two dozen episodes off a screen showing three is
            // the one mistake this key must not make. The marks below are the opposite
            // case and are left alone - see [`App::marked_episodes`].
            let showing: Vec<SeasonEpisode> = self.episodes.shown().into_iter().cloned().collect();
            if showing.is_empty() {
                self.nothing_to_act_on();
            }
            (showing, false)
        } else {
            let marked = self.marked_episodes();
            if marked.is_empty() {
                (self.selection(false), false)
            } else {
                (marked, true)
            }
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
                // The number slot takes the word the episodes column puts in the same
                // place, for a row that has no number: the title is drawn beside it
                // either way, so `Film  Suzume` reads as the queue saying what the row
                // is, where `S01E1 Suzume` would be claiming a number nobody gave it.
                number: single_name(&episode.kind).map_or_else(
                    || format!("S{:02}E{}", episode.season_number, episode_number(&episode)),
                    str::to_owned,
                ),
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
        self.renarrow_downloads();
        // A panel with a list in it and no cursor anywhere has nothing the arrow keys
        // could move, so the first row queued takes one - unless a narrowing is hiding
        // every row there is, in which case there is still nothing to put it on.
        if self.downloads.state.selected().is_none() {
            self.downloads.select(0);
        }
        // One sentence rather than two. Marking already says how many are marked as each
        // one goes on, so the only thing left to say here is what the key took, and
        // saying "downloading the marked episodes" a moment before "queued 3 episodes
        // for download" would be the same news twice and the second half of it wrong.
        // The order they were queued in is not in it either: the panel underneath is
        // showing that, which is what the panel is for.
        self.say(match (queued, from_marks) {
            (1, true) => "Queued the marked episode for download".to_owned(),
            (queued, true) => format!("Queued the {queued} marked episodes for download"),
            (1, false) => "Queued 1 episode for download".to_owned(),
            (queued, false) => format!("Queued {queued} episodes for download"),
        });
    }

    /// Takes the row under the cursor out of the queue.
    ///
    /// Anything but the download that is running, which is inside an hour of segments
    /// on a thread of its own and cannot be called back. One that has not started is
    /// dropped at both ends: the row goes, and the number goes to the worker so that
    /// the thread passes over the request when it reaches it.
    fn drop_download(&mut self) {
        let (Some(row), Some(index)) = (
            self.downloads.state.selected(),
            self.downloads.selected_index(),
        ) else {
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
        self.renarrow_downloads();
        // The row the cursor was on has gone, so it lands on whatever took its place -
        // or on the last row, where it was the last row that went. Rows rather than
        // episodes, because it is the hole in the list the eye is resting on.
        let left = self.downloads.rows();
        self.downloads
            .state
            .select((left > 0).then(|| row.min(left - 1)));
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

    /// Opens one of the four lists, on the value in force.
    ///
    /// The two filter lists come off the network, so one whose values have not been
    /// fetched yet opens holding nothing but All and asks for them. That is the right
    /// way round: the popup is up the moment the key is pressed, and it fills in a beat
    /// later, rather than the key doing nothing visible while a request is out.
    fn open_picker(&mut self, picking: Picking) {
        let rows = self.picker_rows(picking);
        let current = self.chosen(picking);
        let mut pane = Pane {
            pending_cursor: rows.iter().position(|row| row.value == current),
            ..Pane::default()
        };
        pane.set(rows);
        if let Picking::Filter(which) = picking
            && !self.filter_values.contains_key(&which)
        {
            pane.loading = true;
            self.worker.send(Request::FilterValues(which));
        }
        self.picker = Some(Picker {
            kind: picking,
            pane,
        });
    }

    /// What one of the four lists offers.
    ///
    /// Both filter lists open with an All row of their own making, at the top where the
    /// eye lands, because taking a filter off has to be as easy as putting one on and
    /// there is no other key for it. Its value is the empty string, which is what no
    /// filter set reads as everywhere else - so All is the row the list opens on and
    /// marks while nothing is chosen, without anything having to know that it is
    /// special.
    fn picker_rows(&self, picking: Picking) -> Vec<Choice> {
        match picking {
            Picking::Audio | Picking::Subtitles => self
                .locales(picking == Picking::Audio)
                .into_iter()
                .map(|locale| Choice {
                    label: language_name(&locale).to_owned(),
                    value: locale,
                })
                .collect(),
            Picking::Filter(which) => {
                let mut rows = vec![Choice {
                    value: String::new(),
                    label: "All".to_owned(),
                }];
                rows.extend(self.filter_values.get(&which).cloned().unwrap_or_default());
                rows
            }
        }
    }

    /// The value in force for one of the four lists: the row it opens on, and the row it
    /// marks. A filter nobody has set is the empty string, which is the All row's value.
    pub fn chosen(&self, picking: Picking) -> String {
        match picking {
            Picking::Audio => self.audio(),
            Picking::Subtitles => self.subs(),
            Picking::Filter(which) => self
                .filters
                .chosen(which)
                .map(|chosen| chosen.value.clone())
                .unwrap_or_default(),
        }
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
            self.request_episodes_again();
        } else if !self.seasons.owner.is_empty() {
            let cursor = self.seasons.selected_index();
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
        // The quality names the file, so the column was until this moment answering for
        // a file the downloader would no longer write. Nothing has to be fetched again -
        // Crunchyroll picks the quality when the stream is asked for, not when the
        // season is listed - so only this one question is put afresh.
        self.look_on_disk();
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
        self.say_filters_are_off_here();
    }

    /// Puts a filter on the catalogue, or takes it off again when the row chosen is All.
    ///
    /// This is where the one decision the whole feature turns on lives. A filter narrows
    /// the browse listings and nothing else - see [`Filters`] - so one set while the
    /// column is showing the watchlist, the history or a search would be a word in the
    /// header about a list it has nothing to do with. Rather than let the header say
    /// something untrue, or refuse the key and leave the user working out why nothing
    /// happened, the column goes back to the browse order it was last on and the filter
    /// shows its effect there at once. That is the move `back` out of a search already
    /// makes, it is said on the status line as it happens, and it costs one press of
    /// `order` to undo.
    fn choose_filter(&mut self, which: FilterKind, chosen: Choice) {
        let changed = self.chosen(Picking::Filter(which)) != chosen.value;
        self.filters
            .set(which, (!chosen.value.is_empty()).then_some(chosen));
        self.filters_changed(changed);
    }

    /// The simulcast filter, which is a yes or a no and so has no list to pick from.
    fn toggle_simulcast(&mut self) {
        self.filters.simulcast = !self.filters.simulcast;
        self.filters_changed(true);
    }

    /// What every change to a filter does once the change itself has been made.
    ///
    /// The catalogue is asked for again from the start rather than narrowed where it
    /// sits: a filtered catalogue is a different hundred series, not the same hundred
    /// with rows hidden, and Crunchyroll is the only thing that knows which. The status
    /// line then says what is on screen, filters and all, because the column changing
    /// under the cursor without a word is the one thing that would read as a glitch.
    ///
    /// A row chosen a second time changes nothing and is not asked for again - the
    /// answer is already in the column - but it is still said, since the key was pressed
    /// and silence is the same answer a broken key gives.
    fn filters_changed(&mut self, changed: bool) {
        let moved = !matches!(self.listing, Listing::Browse(_));
        if moved {
            self.listing = Listing::Browse(self.sort);
        }
        if changed || moved {
            self.request_catalog();
        }
        let showing = format!("Showing {}", self.showing());
        self.say(if moved {
            only_the_catalogue(&showing)
        } else {
            format!("{showing}.")
        });
    }

    /// The list on screen and what it is narrowed by: `Popular · Genre: Action`.
    fn showing(&self) -> String {
        let label = self.listing.label();
        match self.filters.summary() {
            summary if summary.is_empty() => label,
            summary => format!("{label} · {summary}"),
        }
    }

    /// Says so when the column has arrived somewhere the filters do not reach.
    ///
    /// The header shows them only while a browse listing is on screen, which is the only
    /// way it can describe what is under it. A word that disappears without anything
    /// being said reads as a filter that has been forgotten rather than one that is out
    /// of force for as long as this list is up, so leaving the catalogue says which it
    /// is. Nothing is said on the way back: the header saying it again is the whole
    /// news.
    fn say_filters_are_off_here(&mut self) {
        if self.filters.any() && !matches!(self.listing, Listing::Browse(_)) {
            let label = self.listing.label();
            self.say(only_the_catalogue(&label));
        }
    }

    fn reload(&mut self) {
        match self.focus {
            Focus::Series => {
                let cursor = self.series.selected_index();
                self.request_catalog();
                self.series.pending_cursor = cursor;
            }
            Focus::Seasons => {
                let cursor = self.seasons.selected_index();
                self.request_seasons();
                self.seasons.pending_cursor = cursor;
            }
            Focus::Episodes => self.request_episodes_again(),
            // The queue is not a list anything answers with: it is what this run has
            // asked for, and there is nowhere to ask for it again.
            Focus::Downloads => {}
        }
    }

    fn edit_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.editing = None,
            KeyCode::Enter => {
                let query = self
                    .editing
                    .take()
                    .map_or_else(String::new, |editing| editing.query().trim().to_owned());
                self.listing = if query.is_empty() {
                    Listing::Browse(self.sort)
                } else {
                    Listing::Search(query)
                };
                self.request_catalog();
                self.say_filters_are_off_here();
            }
            _ => {
                if let Some(editing) = self.editing.as_mut() {
                    editing.typed(key);
                }
            }
        }
    }

    /// A key while the filter box is open.
    ///
    /// The column is narrowed again as each letter lands rather than when the box is
    /// closed, which is the whole difference between this and the search: nothing is
    /// being asked of anybody, so there is nothing to wait for and no reason to make
    /// the user guess at what a query will leave standing. Backspace widens by the same
    /// road, since the rows are worked out from the query afresh every time and never
    /// whittled down from what the last letter left.
    ///
    /// The return keeps what the box built and gets out of the way: the column goes on
    /// showing what it was narrowed to, and its title goes on saying so. Escape throws
    /// it away and the whole list comes back. Since the box always opens empty, those
    /// two mean exactly what they mean in the search box - keep this, or forget it.
    fn edit_narrow(&mut self, key: KeyEvent) {
        let Some(Editing::Narrow { focus, .. }) = self.editing.as_ref() else {
            return;
        };
        let focus = *focus;
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.narrow(focus, "");
            }
            KeyCode::Enter => {
                self.editing = None;
                // A narrowing is quiet by nature - the rows it hid are simply not there
                // - so the one moment it is settled on is the moment to say how much of
                // the column is left and how to get the rest back. The key is read off
                // the bindings rather than written down, since it is one someone may
                // have moved.
                if let Some((showing, held)) = self.narrowed_to(focus) {
                    let key = self.keys.first(Command::Filter);
                    self.say(format!(
                        "Showing {showing} of {held} - {key} then esc puts the list back."
                    ));
                }
            }
            _ => {
                if self
                    .editing
                    .as_mut()
                    .is_some_and(|editing| editing.typed(key))
                {
                    let query = self
                        .editing
                        .as_ref()
                        .map_or_else(String::new, |editing| editing.query().to_owned());
                    self.narrow(focus, &query);
                }
            }
        }
    }

    /// Opens the box that narrows the column the keyboard is in.
    ///
    /// It opens empty, and whatever the column was narrowed to goes as it opens, so the
    /// whole list is back on the screen while the first letter is typed. Opening on the
    /// narrowing already there - so that one could be refined rather than retyped - was
    /// the other way round, and it would have left escape meaning one thing on a fresh
    /// box and another on a reopened one. This way escape always ends with the full
    /// list back, and the key twice over - `f` then `esc` - is how a narrowing is taken
    /// off, which is one thing to remember rather than two.
    fn open_narrow(&mut self) {
        let focus = self.focus;
        self.narrow(focus, "");
        self.editing = Some(Editing::Narrow {
            focus,
            query: String::new(),
        });
    }

    /// What a column is showing and what it holds, where it is narrowed at all.
    fn narrowed_to(&self, focus: Focus) -> Option<(usize, usize)> {
        match focus {
            Focus::Series => narrowed_to(&self.series),
            Focus::Seasons => narrowed_to(&self.seasons),
            Focus::Episodes => narrowed_to(&self.episodes),
            Focus::Downloads => narrowed_to(&self.downloads),
        }
    }

    /// An open list has the keys of a column, plus the one that cycles the columns - which
    /// looks at the other list of the pair without going back out first - and the keys
    /// that open each of the four, so a list opened by mistake is one key from the right
    /// one. Anything else does nothing while it is open, `simulcast` included: it is the
    /// filter with no list behind it, and taking it as a keypress about the catalogue
    /// from inside a popup about the catalogue would be the one place where a key changed
    /// what is underneath the thing being looked at.
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
                let other = picker.kind.other();
                self.open_picker(other);
            }
            Command::AudioLanguage => self.open_picker(Picking::Audio),
            Command::SubtitleLanguage => self.open_picker(Picking::Subtitles),
            Command::Genre => self.open_picker(Picking::Filter(FilterKind::Genre)),
            Command::AnimeSeason => self.open_picker(Picking::Filter(FilterKind::Season)),
            Command::Open => {
                let kind = picker.kind;
                let chosen = picker.pane.selected().cloned();
                self.picker = None;
                if let Some(chosen) = chosen {
                    match kind {
                        Picking::Audio => self.choose_locale(true, chosen.value),
                        Picking::Subtitles => self.choose_locale(false, chosen.value),
                        Picking::Filter(which) => self.choose_filter(which, chosen),
                    }
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
        // The box along the top is typing rather than commands: every letter belongs in
        // it, whatever it would otherwise do.
        match self.editing {
            Some(Editing::Search(_)) => {
                self.edit_search(key);
                return Action::None;
            }
            Some(Editing::Narrow { .. }) => {
                self.edit_narrow(key);
                return Action::None;
            }
            None => {}
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
            Command::Search => self.editing = Some(Editing::Search(String::new())),
            Command::Filter => self.open_narrow(),
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
            Command::Mark => self.toggle_mark(),
            Command::Download => self.queue_downloads(false),
            Command::DownloadSeason => self.queue_downloads(true),
            Command::Watchlist => self.toggle_watchlist(),
            Command::MarkWatched => self.mark(true),
            Command::MarkUnwatched => self.mark(false),
            Command::AudioLanguage => self.open_picker(Picking::Audio),
            Command::SubtitleLanguage => self.open_picker(Picking::Subtitles),
            Command::Genre => self.open_picker(Picking::Filter(FilterKind::Genre)),
            Command::AnimeSeason => self.open_picker(Picking::Filter(FilterKind::Season)),
            Command::Simulcast => self.toggle_simulcast(),
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
        // The box along the top owns the pointer as it owns the keyboard: a click is a
        // way out of it and nothing else. It is the return rather than escape, though -
        // the rows under the pointer are the ones the narrowing left, and taking them
        // out from under a finger that was aiming at one would be a poor answer to a
        // click. A search box has nothing to keep, so for that one the two are the same.
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
                let Some(index) = self.row_at(focus, at.y) else {
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
            && let Some(index) = self.row_at(self.focus, at.y)
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

    /// The row under the pointer, while a list is open over the interface.
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

    /// The columns' rule again: a click chooses a row, and a click on the one already
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
    use crate::model::{CatalogItem, Season};
    use crate::tui::art::Gallery;
    use crate::tui::keys::{Bindings, Command};
    use crate::tui::theme::Theme;
    use crate::tui::worker::{
        Choice, FilterKind, Filters, Listing, Request, Response, Update, Worker,
    };

    use super::{
        Action, App, Editing, Focus, OnDisk, Page, Pane, SeasonEpisode, State, episode_label,
        matches, whole_seconds,
    };

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

    /// One page of a catalogue list as the worker hands it over: `count` series starting
    /// at `start`, with what the endpoint said about the length of the list and about
    /// where the next page begins. The ids carry the offset they came from, so a page
    /// appended in the wrong place or appended twice shows up as an id in the wrong row
    /// rather than only as a count.
    fn page(
        start: usize,
        count: usize,
        total: Option<usize>,
        next: Option<usize>,
    ) -> Result<Page, String> {
        Ok(Page {
            items: (start..start + count)
                .map(|index| series(&format!("G{index}")))
                .collect(),
            total,
            next,
        })
    }

    /// Hands the interface a catalogue answer the way the worker does, naming the list and
    /// the offset it was asked for and the filters the interface is holding - which is
    /// what the interface checks all three against. Every test that is not about the
    /// filters themselves means this by a page arriving.
    fn catalog(app: &mut App, listing: Listing, start: usize, result: Result<Page, String>) {
        let filters = app.filters.clone();
        app.accept(Response::Catalog {
            listing,
            start,
            filters,
            result,
        });
    }

    /// A catalogue column part-way through a long list: `loaded` series in it, `total` in
    /// the list, and a page waiting behind them. The order is changed first because the
    /// interface opens on the history, which is the one list that is not paged.
    fn with_catalogue(app: &mut App, loaded: usize, total: usize) {
        app.run(Command::Order);
        let listing = app.listing.clone();
        catalog(app, listing, 0, page(0, loaded, Some(total), Some(loaded)));
        app.sent();
    }

    /// A catalogue row of some other kind: a film's listing, a concert, or something this
    /// client has never heard of.
    fn catalogued(id: &str, kind: &str, title: &str) -> CatalogItem {
        CatalogItem {
            id: id.to_owned(),
            kind: kind.to_owned(),
            title: title.to_owned(),
            description: "A door opens.".to_owned(),
            ..CatalogItem::default()
        }
    }

    /// An open season, so that the download keys have something to act on, with the
    /// seasons column behind it - which is the only way a season is ever open, and what
    /// the keys that ask for the same season again need in order to have something to
    /// ask for. The first season carries two dubs so that the language key has somewhere
    /// to go. The catalogue the interface asked for as it opened is taken off the
    /// worker's hands here too, since none of these tests is about that.
    fn with_episodes(app: &mut App, count: usize) {
        app.seasons.owner = "GY8VEQ95Y".to_owned();
        app.seasons.set(vec![
            Season {
                id: "S1".to_owned(),
                audio_locales: vec!["ja-JP".to_owned(), "en-US".to_owned()],
                ..Season::default()
            },
            Season {
                id: "S2".to_owned(),
                ..Season::default()
            },
        ]);
        app.episodes.owner = "S1".to_owned();
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

    /// The episodes the queue has just been asked for, by id, and in the order it was
    /// asked. The panel shows them by number rather than by id and the ids are what a
    /// test about which episodes were picked wants, so this reads the requests as they
    /// went out - and empties them, so each press can be read on its own.
    fn queued(app: &App) -> Vec<String> {
        app.sent()
            .into_iter()
            .filter_map(|request| match request {
                Request::Download(job) => Some(job.episode.id),
                _ => None,
            })
            .collect()
    }

    /// What the status line is saying.
    fn said(app: &App) -> String {
        app.notice
            .as_ref()
            .map(|notice| notice.text.clone())
            .unwrap_or_default()
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

        catalog(&mut app, Listing::History, 0, page(0, 0, None, None));
        assert_eq!(app.listing, Listing::Browse(0));
        assert!(app.series.loading, "the catalogue was asked for");
        assert!(
            app.notice.is_some(),
            "the column is showing a list nobody asked for and said nothing about it"
        );

        // And the answer to that request is taken as the list it is.
        catalog(&mut app, Listing::Browse(0), 0, page(0, 1, None, None));
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
        catalog(
            &mut app,
            Listing::History,
            0,
            Err("no account id".to_owned()),
        );
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
        catalog(&mut app, Listing::History, 0, page(0, 0, None, None));
        assert_eq!(app.listing, Listing::Browse(0));

        // An empty catalogue is not a reason to go looking for another list.
        catalog(&mut app, Listing::Browse(0), 0, page(0, 0, None, None));
        assert_eq!(app.listing, Listing::Browse(0));

        // Nor is the history, once it has been chosen deliberately.
        while app.listing != Listing::History {
            app.run(Command::Order);
        }
        catalog(&mut app, Listing::History, 0, page(0, 0, None, None));
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

        app.editing = Some(Editing::Search("frieren".to_owned()));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.listing, Listing::Search("frieren".to_owned()));

        app.run(Command::Back);
        assert_eq!(app.listing, Listing::Browse(2));
        // And the ring carries on from there rather than starting over.
        app.run(Command::Order);
        assert_eq!(app.listing, Listing::Watchlist);
    }

    /// The whole point of the feature: the catalogue used to be the first hundred series
    /// and nothing else, and walking to the end of it now asks for the next hundred and
    /// puts them after the ones already there. Appending rather than replacing is what
    /// makes the list one list - a page that replaced what was on screen would leave the
    /// user at the top of a column they had just walked to the bottom of.
    #[test]
    fn reaching_the_bottom_of_the_catalogue_asks_for_the_next_page() {
        let mut app = app();
        with_catalogue(&mut app, 100, 250);
        assert_eq!(app.paging.total, Some(250));

        app.run(Command::Bottom);
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 100,
                filters: Filters::default(),
            }],
            "the end of the loaded list asked for nothing"
        );

        let cursor = app.series.state.selected();
        catalog(
            &mut app,
            Listing::Browse(0),
            100,
            page(100, 100, Some(250), Some(200)),
        );
        assert_eq!(app.series.items.len(), 200);
        assert_eq!(
            app.series.items[0].id, "G0",
            "the first page was thrown away"
        );
        assert_eq!(app.series.items[100].id, "G100");
        assert_eq!(
            app.series.state.selected(),
            cursor,
            "the cursor was moved off the row the user was standing on"
        );
        assert_eq!(app.paging.next, Some(200));
    }

    /// One request in flight at a time. Holding `down` against the bottom of the list is
    /// several keypresses at the last row, and each of them asks the same question: a
    /// column that sent one request per repeat would fetch the same page a dozen times
    /// and append every copy of it.
    #[test]
    fn only_one_page_is_asked_for_at_a_time() {
        let mut app = app();
        with_catalogue(&mut app, 100, 250);

        app.run(Command::Bottom);
        app.run(Command::Down);
        app.run(Command::Down);
        app.run(Command::PageDown);
        assert_eq!(
            app.sent().len(),
            1,
            "the same page was asked for repeatedly"
        );

        // And once it has arrived, the one after it can be asked for.
        catalog(
            &mut app,
            Listing::Browse(0),
            100,
            page(100, 100, Some(250), Some(200)),
        );
        app.run(Command::Bottom);
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 200,
                filters: Filters::default(),
            }]
        );
    }

    /// A list that has been read to the end is not asked about again, however long the
    /// user stands at the bottom of it. Without that the last row of every finished list
    /// would be a request per keypress, into a list that has nothing left to give.
    #[test]
    fn a_list_read_to_its_end_is_not_asked_for_again() {
        let mut app = app();
        app.run(Command::Order);
        let listing = app.listing.clone();
        catalog(&mut app, listing, 0, page(0, 40, Some(40), None));
        app.sent();
        app.run(Command::Bottom);
        assert!(
            app.sent().is_empty(),
            "a list with nothing behind it was asked for more"
        );
    }

    /// The history is the one list in the ring that is not paged, and it says so by
    /// answering with no page after its first: it is a page of episodes boiled down to
    /// the series behind them, so an offset into it means nothing to the rows the column
    /// shows - `api::CrunchyrollClient::history` sets out why. The interface needs no
    /// rule of its own about which lists page; it asks for what the last answer said was
    /// there, and the history says there is nothing.
    #[test]
    fn the_history_is_shown_whole_and_not_paged() {
        let mut app = app();
        assert_eq!(app.listing, Listing::History);
        catalog(&mut app, Listing::History, 0, page(0, 20, None, None));
        app.sent();

        app.run(Command::Bottom);
        assert!(
            app.sent().is_empty(),
            "the history was asked for a second page of series it cannot offer"
        );
        assert_eq!(
            app.paging.total, None,
            "the header would be counting episodes against series"
        );
    }

    /// Two ways a page can arrive that must not be drawn, and they are different
    /// questions. A page of a list the user has cycled past belongs to a column that is
    /// no longer on screen - the pane owner's argument, made about a list rather than a
    /// series. A second copy of a page already appended belongs to the list on screen,
    /// and appending it would show the same hundred series twice.
    #[test]
    fn a_page_nobody_is_waiting_for_is_dropped() {
        let mut app = app();
        with_catalogue(&mut app, 100, 250);
        app.run(Command::Bottom);
        app.run(Command::Order);
        app.sent();

        catalog(
            &mut app,
            Listing::Browse(0),
            100,
            page(100, 100, Some(250), Some(200)),
        );
        assert!(
            app.series.items.is_empty(),
            "a page of the list the user left was drawn into the one they went to"
        );

        let listing = app.listing.clone();
        catalog(
            &mut app,
            listing.clone(),
            0,
            page(0, 100, Some(250), Some(100)),
        );
        assert_eq!(app.series.items.len(), 100);
        catalog(&mut app, listing, 0, page(0, 100, Some(250), Some(100)));
        assert_eq!(
            app.series.items.len(),
            100,
            "the same page was appended a second time"
        );
    }

    /// A page that fails is a page that fails, not a catalogue that fails. The hundred
    /// series already loaded are still perfectly good and the user is standing among
    /// them, so they stay where they are and the status line carries the news - the same
    /// place everything else that goes wrong in the background says so. Asking again is
    /// the user's own move: the cursor is still on the last row, so the next press at the
    /// bottom retries.
    #[test]
    fn a_page_that_fails_leaves_the_column_alone() {
        let mut app = app();
        with_catalogue(&mut app, 100, 250);
        app.run(Command::Bottom);
        app.sent();

        catalog(
            &mut app,
            Listing::Browse(0),
            100,
            Err("503 Service Unavailable".to_owned()),
        );
        assert_eq!(app.series.items.len(), 100, "the column was emptied");
        assert_eq!(
            app.series.error, None,
            "the column put an error where its rows were"
        );
        assert!(
            said(&app).contains("503 Service Unavailable"),
            "{}",
            said(&app)
        );
        assert!(app.notice.as_ref().is_some_and(|notice| notice.error));

        app.run(Command::Bottom);
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 100,
                filters: Filters::default(),
            }],
            "a page that failed could not be asked for again"
        );
    }

    /// One way of getting to the bottom of a column, under the name to put in the
    /// failure if it does not.
    type Way = (&'static str, fn(&mut App));

    /// Whichever way the user gets to the last row is the way that asks for more. `down`
    /// clamped against the bottom, a `page-down` that overshoots, `bottom`, and the
    /// pointer - the wheel moves a column's cursor and a drag puts it on a row, both
    /// through the same two helpers the keys use. A trigger hung off one key would leave
    /// the list refusing to grow for anyone who scrolls with the wheel.
    #[test]
    fn every_way_to_the_last_row_asks_for_the_next_page() {
        let ways: [Way; 5] = [
            ("down", |app| {
                for _ in 0..12 {
                    app.run(Command::Down);
                }
            }),
            ("page-down", |app| {
                app.run(Command::PageDown);
                app.run(Command::PageDown);
            }),
            ("bottom", |app| {
                app.run(Command::Bottom);
            }),
            ("the wheel", |app| app.pane_move(Focus::Series, 12)),
            ("a drag", |app| app.put_cursor(Focus::Series, 11)),
        ];
        for (how, reach_the_end) in ways {
            let mut app = app();
            with_catalogue(&mut app, 12, 40);
            reach_the_end(&mut app);
            assert_eq!(app.series.state.selected(), Some(11), "{how} stopped short");
            assert_eq!(
                app.sent(),
                vec![Request::Catalog {
                    listing: Listing::Browse(0),
                    start: 12,
                    filters: Filters::default(),
                }],
                "{how} reached the end of the list and asked for nothing"
            );
        }
    }

    /// Every way of starting a list over reads it from the top again. The offsets belong
    /// to the list that was open, so one carried into a reload, a change of order, a
    /// search or the way out of one would open the new list a hundred series in - and
    /// leave the header counting against a total that was somebody else's.
    #[test]
    fn a_list_started_over_asks_for_its_first_page() {
        let mut app = app();
        with_catalogue(&mut app, 100, 250);
        app.run(Command::Bottom);
        catalog(
            &mut app,
            Listing::Browse(0),
            100,
            page(100, 100, Some(250), Some(200)),
        );
        assert_eq!(app.series.items.len(), 200);
        app.sent();

        app.run(Command::Reload);
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 0,
                filters: Filters::default(),
            }]
        );
        assert_eq!(app.paging.next, Some(0));
        assert_eq!(
            app.paging.total, None,
            "the length of the list before the reload outlived it"
        );

        app.run(Command::Order);
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(1),
                start: 0,
                filters: Filters::default(),
            }]
        );

        app.editing = Some(Editing::Search("frieren".to_owned()));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Search("frieren".to_owned()),
                start: 0,
                filters: Filters::default(),
            }]
        );

        app.run(Command::Back);
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(1),
                start: 0,
                filters: Filters::default(),
            }]
        );
    }

    /// A narrowed catalogue goes on being a paged one, which is where the two most
    /// invasive things in this branch meet.
    ///
    /// The cursor counts rows, so the end of a narrowed column is the last row the
    /// narrowing left - and that is where the next page is worth asking for, since a query
    /// keeping two rows out of a hundred says nothing at all about the hundred behind
    /// them. The page that arrives is held to the narrowing rather than dropping it: a
    /// hundred unmatched rows appended to a column whose title reads `Series "on" 2/100`
    /// would be the title and the column disagreeing, and the query is still a claim about
    /// the same words on the same screen. And the cursor stays on the row it was on, since
    /// nothing above it moved.
    #[test]
    fn a_narrowed_catalogue_goes_on_paging_and_holds_the_new_rows_to_the_query() {
        let mut app = app();
        app.run(Command::Order);
        let listing = app.listing.clone();
        catalog(
            &mut app,
            listing,
            0,
            Ok(Page {
                items: ["Frieren", "Dandadan", "Grand Blue"]
                    .iter()
                    .map(|title| CatalogItem {
                        id: format!("G{title}"),
                        kind: "series".to_owned(),
                        title: (*title).to_owned(),
                        ..CatalogItem::default()
                    })
                    .collect(),
                total: Some(9),
                next: Some(3),
            }),
        );
        app.sent();

        app.on_key(KeyEvent::from(KeyCode::Char('f')));
        for letter in "an".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(letter)));
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.series.rows(), 2, "Dandadan and Grand Blue, not Frieren");
        assert!(
            app.sent().is_empty(),
            "narrowing a column went and asked Crunchyroll something"
        );

        app.run(Command::Bottom);
        assert_eq!(app.series.state.selected(), Some(1));
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 3,
                filters: Filters::default(),
            }],
            "the last row a narrowing left is the end of the column, and asked for nothing"
        );

        catalog(
            &mut app,
            Listing::Browse(0),
            3,
            Ok(Page {
                items: ["Bocchi the Rock", "Vinland Saga", "Ranma"]
                    .iter()
                    .map(|title| CatalogItem {
                        id: format!("G{title}"),
                        kind: "series".to_owned(),
                        title: (*title).to_owned(),
                        ..CatalogItem::default()
                    })
                    .collect(),
                total: Some(9),
                next: Some(6),
            }),
        );
        assert_eq!(app.series.items.len(), 6, "the page was not appended");
        assert_eq!(
            app.series
                .shown()
                .iter()
                .map(|series| series.title.clone())
                .collect::<Vec<_>>(),
            ["Dandadan", "Grand Blue", "Vinland Saga", "Ranma"],
            "the new rows were let past the narrowing, or held back too hard"
        );
        assert_eq!(
            app.series.state.selected(),
            Some(1),
            "a page landing moved the cursor off the row the user was standing on"
        );
        assert_eq!(app.series.query(), Some("an"));
    }

    /// Where the two halves of this branch meet: a narrowed catalogue is paged like any
    /// other, and every page of it has to carry the same filters as the first.
    ///
    /// A second page asked for unnarrowed would append a hundred series from the whole
    /// catalogue to the bottom of a column that says `Genre: Action` at the top, which is
    /// the one way a pager and a filter can lie to each other. Setting a filter also
    /// starts the list over: the narrowed catalogue is a different hundred series rather
    /// than the same hundred with rows taken out, and only Crunchyroll knows which - so
    /// the offset the column had walked to means nothing under the new question.
    #[test]
    fn the_next_page_of_a_narrowed_catalogue_is_narrowed_the_same_way() {
        let mut app = app();
        with_catalogue(&mut app, 100, 250);
        app.run(Command::Bottom);
        app.sent();

        app.choose_filter(
            FilterKind::Genre,
            Choice {
                value: "action".to_owned(),
                label: "Action".to_owned(),
            },
        );
        let narrowed = app.filters.clone();
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 0,
                filters: narrowed.clone(),
            }],
            "the filter left the column where the unnarrowed list had got to"
        );

        app.accept(Response::Catalog {
            listing: Listing::Browse(0),
            start: 0,
            filters: narrowed.clone(),
            result: page(0, 100, Some(120), Some(100)),
        });
        app.sent();
        app.run(Command::Bottom);
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 100,
                filters: narrowed,
            }],
            "the second page of a narrowed catalogue was asked for unnarrowed"
        );
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
        // A film has no number at all, and the row the sentence is about is the only row
        // in the column, so it is called by its title rather than by a number nobody gave
        // it.
        assert_eq!(
            episode_label(&SeasonEpisode {
                kind: "movie".to_owned(),
                title: "Suzume".to_owned(),
                episode_number: 1,
                ..SeasonEpisode::default()
            }),
            "Suzume"
        );
        assert_eq!(
            episode_label(&SeasonEpisode {
                episode: String::new(),
                episode_number: 4,
                ..SeasonEpisode::default()
            }),
            "E4"
        );
    }

    /// The key has to put a mark on and take it off again with the same press, because
    /// the only other way to undo one would be to leave the season - which is a long way
    /// to go to correct a slip, and takes the other four marks with it.
    #[test]
    fn a_mark_goes_on_and_comes_off_the_episode_under_the_cursor() {
        let mut app = app();
        with_episodes(&mut app, 4);

        app.run(Command::Mark);
        assert!(app.marked.contains("E1"));
        assert_eq!(said(&app), "Marked E1 - 1 episode marked.");

        app.run(Command::Down);
        app.run(Command::Mark);
        assert_eq!(said(&app), "Marked E2 - 2 episodes marked.");

        app.run(Command::Mark);
        assert!(!app.marked.contains("E2"), "the second press did nothing");
        assert_eq!(said(&app), "Unmarked E2 - 1 episode marked.");

        app.run(Command::Up);
        app.run(Command::Mark);
        assert!(app.marked.is_empty());
        assert_eq!(
            said(&app),
            "Unmarked E1 - nothing marked.",
            "a count of zero is a thing to work out rather than an answer"
        );
    }

    /// The point of the whole exercise: `d` is the key it always was until something is
    /// marked, and once something is it queues the marks rather than the row the cursor
    /// happens to be resting on. `D` is not drawn into it - the whole season is the one
    /// request a handful of marks cannot be asking for. The status line has to say which
    /// of the two just happened, since the queue looks the same either way.
    #[test]
    fn download_takes_the_marks_when_there_are_any_and_the_cursor_when_there_are_none() {
        let mut app = app();
        with_episodes(&mut app, 4);
        app.run(Command::Download);
        assert_eq!(queued(&app), ["E1"]);
        assert_eq!(said(&app), "Queued 1 episode for download");

        app.episodes.select(2);
        app.run(Command::Mark);
        app.episodes.select(0);
        app.run(Command::Download);
        assert_eq!(
            queued(&app),
            ["E3"],
            "the cursor is not the question once something is marked"
        );
        assert_eq!(said(&app), "Queued the marked episode for download");

        app.episodes.select(1);
        app.run(Command::Mark);
        app.run(Command::Download);
        assert_eq!(queued(&app), ["E2", "E3"]);
        assert_eq!(said(&app), "Queued the 2 marked episodes for download");

        app.run(Command::DownloadSeason);
        assert_eq!(
            queued(&app),
            ["E1", "E2", "E3", "E4"],
            "the whole season still means the whole season"
        );
        assert_eq!(said(&app), "Queued 4 episodes for download");
    }

    /// Marks are put on in whatever order the eye finds them, and a season is watched in
    /// the order it is listed in. Queueing E5 ahead of E1 because the cursor got there
    /// first would make the shape of the queue depend on something that is no longer
    /// visible anywhere on screen - and the panel, which is the only account of what was
    /// asked for, would be showing it.
    #[test]
    fn marked_episodes_are_queued_in_the_order_the_season_lists_them() {
        let mut app = app();
        with_episodes(&mut app, 5);
        for index in [3, 0, 4] {
            app.episodes.select(index);
            app.run(Command::Mark);
        }
        app.run(Command::Download);
        assert_eq!(queued(&app), ["E1", "E4", "E5"]);
        assert_eq!(
            app.downloads
                .items
                .iter()
                .map(|download| download.number.clone())
                .collect::<Vec<_>>(),
            ["S01E1", "S01E4", "S01E5"],
            "the panel is showing the order they were pressed in"
        );
    }

    /// A mark belongs to the season it was put on. Carried into another one it would be
    /// pointing at an episode that is not there, and the season that replaced it would
    /// arrive with rows already marked that nobody had touched.
    #[test]
    fn the_marks_do_not_follow_the_column_to_another_season() {
        let mut app = app();
        with_episodes(&mut app, 3);
        app.run(Command::Mark);
        assert_eq!(app.marked.len(), 1);

        app.focus = Focus::Seasons;
        app.seasons.select(1);
        app.run(Command::Open);
        assert!(app.marked.is_empty(), "the marks came along to S2");
        assert_eq!(
            app.sent(),
            [Request::Episodes {
                season_id: "S2".to_owned(),
                audio: "ja-JP".to_owned(),
                subs: "en-US".to_owned(),
            }]
        );
    }

    /// A reload and a language change are the same season asked for over again, which is
    /// why the cursor is put back across both. Five marks are more work to put back than
    /// one cursor, and someone who marks half a season and then notices it is offering
    /// the sub has asked for another dub rather than for their marks to be swept up.
    #[test]
    fn the_marks_survive_the_season_being_asked_for_again() {
        let mut app = app();
        with_episodes(&mut app, 4);
        app.episodes.select(2);
        app.run(Command::Mark);

        app.run(Command::Reload);
        assert!(app.marked.contains("E3"), "a reload threw the marks away");
        assert_eq!(app.episodes.pending_cursor, Some(2));
        let _ = app.sent();

        app.run(Command::NextAudio);
        assert_eq!(app.audio(), "en-US", "the language did not change");
        assert!(
            app.marked.contains("E3"),
            "a change of dub threw the marks away"
        );
        assert_eq!(
            app.sent(),
            [Request::Episodes {
                season_id: "S1".to_owned(),
                audio: "en-US".to_owned(),
                subs: "en-US".to_owned(),
            }]
        );
    }

    /// The other episode keys act on the cursor from wherever the keyboard is, because
    /// there is only one thing they could mean. A mark is different: it is left behind on
    /// a row that someone reading the catalogue column cannot see. So it belongs to the
    /// episodes column, and pressed anywhere else - the Downloads panel included, where
    /// the rows are episodes too and a mark would look as though it meant something - it
    /// says so rather than being a key that does nothing on three panels out of four.
    #[test]
    fn marking_answers_only_in_the_episodes_column() {
        // With no season open at all the answer is the one every other episode key
        // gives, because it is the same answer.
        let mut empty = app();
        empty.focus = Focus::Episodes;
        empty.run(Command::Mark);
        assert_eq!(said(&empty), "Open a season first.");

        let mut app = app();
        with_episodes(&mut app, 3);
        for elsewhere in [Focus::Series, Focus::Seasons, Focus::Downloads] {
            app.focus = elsewhere;
            app.run(Command::Mark);
            assert!(app.marked.is_empty(), "{elsewhere:?} marked an episode");
            assert_eq!(said(&app), "Marking is the episodes column's key.");
            assert!(app.notice.as_ref().is_some_and(|notice| notice.error));
        }
    }

    /// The crux of putting films in the catalogue: `open` on one has to end at a row that
    /// `p` plays and `d` queues, down the paths an episode already goes. The middle column
    /// is not a stop on the way, since there is nothing to choose in a column holding one
    /// row, so opening the film fills it with the row that says what the thing is and asks
    /// for the films behind the listing in the same breath.
    #[test]
    fn a_film_opens_into_a_row_that_plays_and_queues() {
        let mut app = app();
        app.series
            .set(vec![catalogued("GM5V7XW1Q", "movie_listing", "Suzume")]);
        app.sent();
        app.run(Command::Open);

        assert_eq!(
            app.seasons.items.len(),
            1,
            "the middle column was left empty"
        );
        assert_eq!(app.seasons.items[0].kind, "movie_listing");
        assert_eq!(
            app.focus,
            Focus::Episodes,
            "the cursor stopped in a column with one row in it"
        );
        assert!(app.episodes.loading);
        assert_eq!(
            app.sent(),
            [Request::Movies {
                listing_id: "GM5V7XW1Q".to_owned(),
                audio: "ja-JP".to_owned(),
                subs: "en-US".to_owned(),
            }]
        );

        // The films the listing holds, in the shape `api::films` builds them.
        app.accept(Response::Episodes {
            season_id: "GM5V7XW1Q".to_owned(),
            result: Ok(vec![SeasonEpisode {
                id: "GY8DVXWZ1".to_owned(),
                kind: "movie".to_owned(),
                season_number: 1,
                episode_number: 1,
                series_title: "Suzume".to_owned(),
                title: "Suzume".to_owned(),
                audio_locale: "ja-JP".to_owned(),
                duration_ms: 7_212_000,
                ..SeasonEpisode::default()
            }]),
        });
        assert_eq!(app.episodes.items.len(), 1);
        // The playheads that ride along with any episodes answer are of no interest here.
        let _ = app.sent();

        let Action::Play(playing) = app.run(Command::Play) else {
            panic!("the film is a row that does nothing when it is played");
        };
        assert_eq!(playing.len(), 1);
        assert_eq!(playing[0].id, "GY8DVXWZ1");

        app.run(Command::Download);
        assert_eq!(queued(&app), ["GY8DVXWZ1"]);
        assert_eq!(
            app.downloads.items[0].number, "Film",
            "the queue numbered a film as though it were an episode of something"
        );
        assert_eq!(said(&app), "Queued 1 episode for download");
    }

    /// A concert and a music video are already the thing that plays, so opening one asks
    /// Crunchyroll for nothing: there is no listing behind it to fetch, and a column left
    /// on `Loading...` would be waiting for an answer nobody was ever going to send. It
    /// still has to end at a row that plays and queues, and it still has to be able to say
    /// where the account had got to in it, which is the one request it does send.
    #[test]
    fn a_concert_opens_into_a_row_without_asking_for_anything() {
        let mut app = app();
        app.series
            .set(vec![catalogued("MC413F1E4B", "musicConcert", "LiSA LiVE")]);
        app.sent();
        app.run(Command::Open);

        assert_eq!(app.focus, Focus::Episodes);
        assert!(
            !app.episodes.loading,
            "the column is waiting for an answer nobody is going to send"
        );
        assert_eq!(app.episodes.items.len(), 1);
        let row = &app.episodes.items[0];
        assert_eq!(row.id, "MC413F1E4B", "the concert is what plays");
        assert_eq!(row.title, "LiSA LiVE");
        assert_eq!(
            row.description, "A door opens.",
            "the blurb came off the catalogue entry it was opened from"
        );
        assert_eq!(
            row.audio_locale, "ja-JP",
            "the downloader has no locale to hang its one stream on"
        );
        assert_eq!(
            app.sent(),
            [Request::Playheads {
                season_id: "MC413F1E4B".to_owned(),
                episode_ids: vec!["MC413F1E4B".to_owned()],
            }]
        );

        let Action::Play(playing) = app.run(Command::Play) else {
            panic!("the concert is a row that does nothing when it is played");
        };
        assert_eq!(playing[0].id, "MC413F1E4B");
        app.run(Command::Download);
        assert_eq!(app.downloads.items[0].number, "Music");
    }

    /// Every list that fills the catalogue column drops what it cannot open, so nothing
    /// should reach this - but a row that did and then did nothing when it was opened
    /// would look like the interface having stopped answering.
    #[test]
    fn a_row_nothing_knows_how_to_open_says_so() {
        let mut app = app();
        app.series
            .set(vec![catalogued("GARTIST1", "artist", "LiSA")]);
        app.sent();
        app.run(Command::Open);

        assert!(app.seasons.items.is_empty());
        assert_eq!(
            app.focus,
            Focus::Series,
            "the cursor left for a column with nothing in it"
        );
        assert!(said(&app).contains("artist"), "said {:?}", said(&app));
        assert!(app.notice.as_ref().is_some_and(|notice| notice.error));
        assert!(app.sent().is_empty(), "something was asked for anyway");
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

    /// Types a narrowing into the column the keyboard is in, the way a user does it: the
    /// key that opens the box, and then letters. The box is left open, since what
    /// happens while it is open is most of what these tests are about.
    fn typed(app: &mut App, query: &str) {
        app.on_key(KeyEvent::from(KeyCode::Char('f')));
        for letter in query.chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(letter)));
        }
    }

    /// The episodes the column is showing, by id.
    fn showing(app: &App) -> Vec<String> {
        app.episodes
            .shown()
            .iter()
            .map(|episode| episode.id.clone())
            .collect()
    }

    /// A substring rather than fzf's scattered letters, and case ignored until the query
    /// carries one. Both are decisions someone will want to revisit, so they are written
    /// down here: `tje` finding `The Journey Ends` is the fzf behaviour this deliberately
    /// does not have, because these lists cannot be reordered by how well a row matched.
    #[test]
    fn matching_ignores_case_until_the_query_carries_one() {
        assert!(matches("The Journey Ends", "journey"));
        assert!(matches("The Journey Ends", "The Journey"));
        assert!(
            !matches("The Journey Ends", "tje"),
            "a subsequence match would keep most of a season"
        );
        assert!(!matches("The Journey Ends", "journeys"));

        // Smart case: lower case is someone typing quickly, and a capital is someone
        // being specific.
        assert!(matches("Ed", "ed") && matches("wanted", "ed"));
        assert!(matches("Ed", "Ed"));
        assert!(
            !matches("wanted", "Ed"),
            "the capital was supposed to mean something"
        );
        // An empty query matches everything, which is what stops a box with nothing
        // typed into it yet from emptying the column - though `Pane::narrow` never gets
        // this far with one.
        assert!(matches("anything", ""));
    }

    /// The whole of what `f` is for: the column narrows as each letter lands, backspace
    /// widens it again, nothing is asked of Crunchyroll at any point, and escape leaves
    /// the list as it was found. Waiting for the return - which is what `/` does - would
    /// make the user guess at what a query was going to leave standing.
    #[test]
    fn a_narrowing_happens_as_each_letter_lands_and_esc_puts_the_list_back() {
        let mut app = app();
        with_episodes(&mut app, 12);
        app.episodes.select(10);

        app.on_key(KeyEvent::from(KeyCode::Char('f')));
        assert!(app.editing.is_some(), "the box did not open");
        assert_eq!(
            app.episodes.rows(),
            12,
            "the box opens on the whole list, so that escape has one meaning"
        );

        app.on_key(KeyEvent::from(KeyCode::Char('1')));
        assert_eq!(showing(&app), ["E1", "E10", "E11", "E12"]);
        assert_eq!(
            app.episodes.state.selected(),
            Some(2),
            "the cursor did not follow the episode it was on"
        );

        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(showing(&app), ["E12"]);
        assert_eq!(
            app.episodes.selected().map(|episode| episode.id.clone()),
            Some("E12".to_owned()),
            "the cursor was left on a row nobody can see"
        );

        app.on_key(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(
            showing(&app),
            ["E1", "E10", "E11", "E12"],
            "backspace did not widen it again"
        );
        assert!(
            app.sent().is_empty(),
            "a narrowing went and asked Crunchyroll something"
        );

        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.editing.is_none());
        assert_eq!(app.episodes.query(), None);
        assert_eq!(app.episodes.rows(), 12);
        assert_eq!(
            app.episodes.selected().map(|episode| episode.id.clone()),
            Some("E12".to_owned()),
            "escape put the list back and took the row the user had chosen with it"
        );
    }

    /// The return keeps what the box built, and that is the one moment worth a sentence:
    /// a narrowing is quiet by nature - the rows it hid are simply not there - so the
    /// status line says how much of the column is left and how to get the rest back.
    /// Pressing the key again is that way back, since the box opens on the whole list.
    #[test]
    fn the_return_keeps_the_narrowing_and_says_how_much_is_left() {
        let mut app = app();
        with_episodes(&mut app, 12);
        typed(&mut app, "1");
        app.on_key(KeyEvent::from(KeyCode::Enter));

        assert!(app.editing.is_none());
        assert_eq!(app.episodes.query(), Some("1"));
        assert_eq!(app.episodes.rows(), 4);
        assert_eq!(
            said(&app),
            "Showing 4 of 12 - f then esc puts the list back."
        );

        app.on_key(KeyEvent::from(KeyCode::Char('f')));
        assert_eq!(app.episodes.rows(), 12);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(
            app.episodes.query(),
            None,
            "f then esc left the narrowing on"
        );
    }

    /// Every cursor operation counts in rows, so none of them can put the cursor on a
    /// row the narrowing is hiding - and `selected` turns a row back into the episode
    /// the user is looking at. This is the invariant the whole design exists for: what
    /// is played, queued and marked is read off that one answer.
    #[test]
    fn the_cursor_can_only_land_on_a_row_the_narrowing_left() {
        let mut app = app();
        with_episodes(&mut app, 12);
        typed(&mut app, "1");
        app.on_key(KeyEvent::from(KeyCode::Enter));

        app.run(Command::Bottom);
        assert_eq!(app.episodes.state.selected(), Some(3));
        assert_eq!(
            app.episodes.selected_index(),
            Some(11),
            "the last row of four is the twelfth episode"
        );
        app.run(Command::Down);
        assert_eq!(
            app.episodes.state.selected(),
            Some(3),
            "the cursor walked off the end of the narrowing"
        );
        app.run(Command::Up);
        assert_eq!(
            app.episodes.selected().map(|episode| episode.id.clone()),
            Some("E11".to_owned())
        );
        app.run(Command::PageUp);
        assert_eq!(app.episodes.state.selected(), Some(0));
        assert_eq!(app.episodes.window(), (0, 4), "the pointer was told twelve");

        // And a row past the end of what is showing moves nothing, the way a click aimed
        // at a frame the list has since changed under moves nothing.
        app.episodes.select(9);
        assert_eq!(app.episodes.state.selected(), Some(0));
    }

    /// Each column keeps its own, so walking between them carries nothing along and
    /// drops nothing either. A narrowing that followed the keyboard would be a narrowing
    /// the user had to undo before looking at anything else.
    #[test]
    fn each_column_keeps_its_own_narrowing() {
        let mut app = app();
        app.series.set(vec![series("GY1"), series("GY2")]);
        with_episodes(&mut app, 4);
        // The seasons are numbered here because that is what their rows read as: a
        // season with no title of its own is drawn as `Season 2`.
        app.seasons.set(vec![
            Season {
                id: "S1".to_owned(),
                season_number: 1,
                ..Season::default()
            },
            Season {
                id: "S2".to_owned(),
                season_number: 2,
                ..Season::default()
            },
        ]);

        typed(&mut app, "3");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(showing(&app), ["E3"]);

        app.run(Command::Back);
        assert_eq!(app.focus, Focus::Seasons);
        assert_eq!(
            app.seasons.rows(),
            2,
            "the episodes' narrowing came along to the seasons"
        );
        typed(&mut app, "2");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            app.seasons.rows(),
            1,
            "a season is matched against the words its row shows"
        );
        assert_eq!(
            app.seasons.selected().map(|season| season.id.clone()),
            Some("S2".to_owned())
        );
        assert_eq!(
            showing(&app),
            ["E3"],
            "the seasons' narrowing reached the episodes"
        );

        app.run(Command::Back);
        assert_eq!(app.focus, Focus::Series);
        assert_eq!(app.series.rows(), 2);
        assert_eq!(app.episodes.query(), Some("3"));
        assert_eq!(app.seasons.query(), Some("2"));
    }

    /// A narrowing belongs to the rows it was typed against, so it goes whenever the
    /// column is filled again - another season, a reload, a change of language. The
    /// cursor is carried across a reload and the narrowing is not, which is the same
    /// argument in both directions: a cursor names a row that can be found again, and a
    /// query names words that are about to be replaced.
    #[test]
    fn a_narrowing_does_not_survive_the_column_being_filled_again() {
        let mut app = app();
        with_episodes(&mut app, 12);
        app.episodes.select(2);
        typed(&mut app, "3");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(showing(&app), ["E3"]);

        app.run(Command::Reload);
        assert_eq!(
            app.episodes.pending_cursor,
            Some(2),
            "the cursor came back as a row of the narrowing rather than as an episode"
        );
        let _ = app.sent();
        with_episodes(&mut app, 12);
        assert_eq!(app.episodes.query(), None, "a reload kept the narrowing");
        assert_eq!(app.episodes.rows(), 12);
        assert_eq!(
            app.episodes.selected().map(|episode| episode.id.clone()),
            Some("E3".to_owned()),
            "the cursor did not come back to the episode it was on"
        );

        // And another season is the same answer arrived at from the other direction.
        typed(&mut app, "3");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.focus = Focus::Seasons;
        app.seasons.select(1);
        app.run(Command::Open);
        assert_eq!(app.episodes.query(), None);
        assert_eq!(app.episodes.rows(), 0);
    }

    /// The two keys that mean a whole column take what the column is showing. A screen
    /// with three episodes on it must not hand mpv the twenty-one it is hiding, which is
    /// the same rule that keeps the cursor on a visible row - the narrowing is what the
    /// user is looking at, and these two keys are about what is in front of them.
    #[test]
    fn the_keys_for_a_whole_column_take_the_rows_that_are_showing() {
        let mut app = app();
        with_episodes(&mut app, 12);
        typed(&mut app, "1");
        app.on_key(KeyEvent::from(KeyCode::Enter));

        app.run(Command::Down);
        match app.run(Command::PlayRest) {
            Action::Play(episodes) => assert_eq!(
                episodes
                    .iter()
                    .map(|episode| episode.id.clone())
                    .collect::<Vec<_>>(),
                ["E10", "E11", "E12"],
                "the rest of the season reached past the narrowing"
            ),
            _ => panic!("nothing was played"),
        }

        app.run(Command::DownloadSeason);
        assert_eq!(queued(&app), ["E1", "E10", "E11", "E12"]);
        assert_eq!(said(&app), "Queued 4 episodes for download");

        // Narrowed to nothing there is nothing to act on, and saying to open a season
        // would be answering a question nobody asked: there is one open.
        typed(&mut app, "zzz");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.run(Command::DownloadSeason);
        assert!(queued(&app).is_empty());
        assert_eq!(
            said(&app),
            "Nothing in this season matches what the column was narrowed to."
        );
    }

    /// A narrowing hides rows; it does not touch what the user has put on them. The
    /// marks stay where they are, `d` goes on queueing all of them, and an episode that
    /// cannot be seen cannot be marked or unmarked by accident either, because the
    /// cursor cannot reach it.
    #[test]
    fn a_narrowing_hides_a_marked_row_without_touching_the_mark() {
        let mut app = app();
        with_episodes(&mut app, 12);
        app.episodes.select(1);
        app.run(Command::Mark);
        assert!(app.marked.contains("E2"));

        typed(&mut app, "1");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(showing(&app), ["E1", "E10", "E11", "E12"]);
        assert!(app.marked.contains("E2"), "the narrowing took the mark off");

        app.run(Command::Download);
        assert_eq!(
            queued(&app),
            ["E2"],
            "a mark is put on by hand, and a query typed afterwards does not undo it"
        );
        assert_eq!(said(&app), "Queued the marked episode for download");

        // And with nothing showing, the key that marks has nothing to mark rather than
        // the first row of the season.
        typed(&mut app, "zzz");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.run(Command::Mark);
        assert_eq!(app.marked.len(), 1, "a hidden row was marked");
        assert_eq!(
            said(&app),
            "Nothing in this season matches what the column was narrowed to."
        );
    }

    /// The queue is the one column whose list changes a row at a time under the user's
    /// hands rather than arriving whole, so its narrowing has to be worked out again as
    /// rows are queued and dropped. Rows pointing at the episodes they used to point at
    /// would drop the wrong download, which is the one mistake the panel cannot recover
    /// from.
    #[test]
    fn the_queue_keeps_its_narrowing_as_rows_come_and_go() {
        let mut app = app();
        with_episodes(&mut app, 4);
        app.run(Command::DownloadSeason);
        app.focus = Focus::Downloads;

        typed(&mut app, "e2");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.downloads.rows(), 1);
        assert_eq!(app.downloads.selected().map(|row| row.id), Some(1));

        app.focus = Focus::Episodes;
        app.episodes.select(1);
        app.run(Command::Download);
        assert_eq!(app.downloads.items.len(), 5);
        assert_eq!(
            app.downloads.rows(),
            2,
            "the row that arrived was never measured against the query"
        );

        app.focus = Focus::Downloads;
        app.downloads.select(1);
        assert_eq!(app.downloads.selected().map(|row| row.id), Some(4));
        app.run(Command::Open);
        assert_eq!(
            app.downloads.items.len(),
            4,
            "a row was dropped by row number"
        );
        assert_eq!(app.downloads.rows(), 1);
        assert_eq!(
            app.downloads.selected().map(|row| row.id),
            Some(1),
            "the cursor was left somewhere the narrowing is not"
        );
    }

    /// The quality is part of the file name, so `v` changes which file each row is
    /// asking about. A marker left over from the quality before it would be describing a
    /// file the downloader would no longer write: the row would be saying the episode is
    /// here while pressing download started it from nothing.
    #[test]
    fn a_change_of_quality_asks_the_disk_again() {
        let mut app = app();
        with_episodes(&mut app, 1);
        // Nothing of this series is anywhere near the directory the tests run in, so
        // whatever the column was told before, the answer now is that there is no file.
        app.downloaded.insert("E1".to_owned(), OnDisk::Complete);

        app.run(Command::Quality);
        assert_eq!(app.options.video_quality, "720p");
        assert!(
            app.downloaded.is_empty(),
            "the marker outlived the quality it was looked up for"
        );
    }

    /// The queue is what makes the marker worth having while the program is open: an
    /// episode downloaded from the interface has to show as downloaded without the season
    /// being fetched again, since that reload is the thing this saves. Three answers move
    /// the disk and are asked about - the episode being taken up, the first part of it
    /// reporting, and the end - and the hundreds of percentages in between are not.
    #[test]
    fn a_download_moves_the_marker_without_the_season_being_reloaded() {
        let mut app = app();
        with_episodes(&mut app, 1);
        app.run(Command::Download);

        let stage = |done| Update::Stage {
            stage: "video".to_owned(),
            done,
            total: 12,
        };
        // Nothing of this series is anywhere near the directory the tests run in, so a
        // column that was asked again is a column with nothing left in it.
        for update in [Update::Started, stage(3)] {
            app.downloaded.insert("E1".to_owned(), OnDisk::Complete);
            answer(&mut app, 0, update);
            assert!(
                app.downloaded.is_empty(),
                "the column kept an answer from before the download changed the disk"
            );
        }

        app.downloaded.insert("E1".to_owned(), OnDisk::Complete);
        answer(&mut app, 0, stage(9));
        assert_eq!(
            app.downloaded.len(),
            1,
            "a percentage is the same file getting bigger, and the panel draws hundreds"
        );

        app.downloaded.insert("E1".to_owned(), OnDisk::Complete);
        answer(&mut app, 0, Update::Finished(Ok(())));
        assert!(
            app.downloaded.is_empty(),
            "the episode landed and the column was never asked"
        );
    }
}
