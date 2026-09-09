use std::sync::{Arc, Mutex};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::download::DownloadOptions;
use crate::model::{CatalogItem, Season, SeasonEpisode};

use super::QUALITIES;
use super::worker::{Listing, Request, Response, Worker};

/// Which column the keyboard is pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Series,
    Seasons,
    Episodes,
}

/// What the event loop has to leave the interface to do, because it needs the terminal
/// back: mpv draws over it, and a download prints progress bars.
pub enum Action {
    None,
    Quit,
    /// Played one after another, so quitting mpv moves on to the next episode.
    Play(Vec<SeasonEpisode>),
    Download(Vec<SeasonEpisode>),
}

pub struct Notice {
    pub text: String,
    pub error: bool,
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
}

impl<T> Default for Pane<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            state: ListState::default(),
            loading: false,
            error: None,
            owner: String::new(),
        }
    }
}

impl<T> Pane<T> {
    pub fn selected(&self) -> Option<&T> {
        self.state.selected().and_then(|index| self.items.get(index))
    }

    pub fn set(&mut self, items: Vec<T>) {
        self.state
            .select((!items.is_empty()).then_some(0));
        self.items = items;
        self.loading = false;
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.state.select(None);
        self.loading = false;
        self.error = None;
        self.owner.clear();
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() as isize - 1;
        let current = self.state.selected().unwrap_or(0) as isize;
        self.state.select(Some(current.saturating_add(delta).clamp(0, last) as usize));
    }

    pub fn select_edge(&mut self, last: bool) {
        if self.items.is_empty() {
            return;
        }
        self.state
            .select(Some(if last { self.items.len() - 1 } else { 0 }));
    }
}

pub struct App {
    worker: Worker,
    pub options: DownloadOptions,
    pub focus: Focus,
    pub series: Pane<CatalogItem>,
    pub seasons: Pane<Season>,
    pub episodes: Pane<SeasonEpisode>,
    pub listing: Listing,
    /// The search box while it is being typed into.
    pub editing: Option<String>,
    pub notice: Option<Notice>,
    /// What the API client would have printed had the interface not owned the screen.
    notices: Arc<Mutex<Vec<String>>>,
    pub show_help: bool,
    pub quit: bool,
    pub tick: usize,
    sort: usize,
}

impl App {
    pub fn new(
        worker: Worker,
        options: DownloadOptions,
        notices: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        let mut app = Self {
            worker,
            options,
            focus: Focus::Series,
            series: Pane::default(),
            seasons: Pane::default(),
            episodes: Pane::default(),
            listing: Listing::Browse(0),
            editing: None,
            notice: None,
            notices,
            show_help: false,
            quit: false,
            tick: 0,
            sort: 0,
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

    pub fn complain(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: true,
        });
    }

    fn request_catalog(&mut self) {
        self.series.clear();
        self.seasons.clear();
        self.episodes.clear();
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
        self.episodes.clear();
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
        self.episodes.clear();
        self.episodes.loading = true;
        self.episodes.owner = season_id.clone();
        self.focus = Focus::Episodes;
        self.worker.send(Request::Episodes {
            season_id,
            audio: self.audio(),
            subs: self.subs(),
        });
    }

    /// Takes in whatever the worker has finished, and whatever the client wanted to say.
    pub fn drain(&mut self) {
        while let Some(response) = self.worker.try_recv() {
            match response {
                Response::Catalog { listing, result } => {
                    if listing != self.listing {
                        continue;
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
                Response::Seasons { series_id, result } => {
                    if series_id != self.seasons.owner {
                        continue;
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
                        continue;
                    }
                    match result {
                        Ok(items) => self.episodes.set(items),
                        Err(error) => {
                            self.episodes.loading = false;
                            self.episodes.error = Some(error.clone());
                            self.complain(error);
                        }
                    }
                }
            }
        }
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

    fn focused_pane_move(&mut self, delta: isize) {
        match self.focus {
            Focus::Series => self.series.move_by(delta),
            Focus::Seasons => self.seasons.move_by(delta),
            Focus::Episodes => self.episodes.move_by(delta),
        }
    }

    fn focused_pane_edge(&mut self, last: bool) {
        match self.focus {
            Focus::Series => self.series.select_edge(last),
            Focus::Seasons => self.seasons.select_edge(last),
            Focus::Episodes => self.episodes.select_edge(last),
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
        }
    }

    fn ascend(&mut self) {
        match self.focus {
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

    fn play(&mut self, to_end: bool) -> Action {
        let episodes = self.selection(to_end);
        if episodes.is_empty() {
            return Action::None;
        }
        Action::Play(episodes)
    }

    fn download(&mut self, whole_season: bool) -> Action {
        let episodes = if whole_season {
            if self.episodes.items.is_empty() {
                self.complain("Open a season first.");
            }
            self.episodes.items.clone()
        } else {
            self.selection(false)
        };
        if episodes.is_empty() {
            return Action::None;
        }
        Action::Download(episodes)
    }

    /// The locales worth offering for the current selection, most specific first: what
    /// the season lists, else what the series lists, else what was asked for on the
    /// command line.
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
        for candidate in [from_season, from_series] {
            if let Some(locales) = candidate.filter(|locales| !locales.is_empty()) {
                let mut locales = locales.clone();
                locales.sort();
                return locales;
            }
        }
        configured.clone()
    }

    fn cycle_locale(&mut self, audio: bool) {
        let locales = self.locales(audio);
        if locales.is_empty() {
            self.complain(if audio {
                "No audio locale is listed for this selection."
            } else {
                "No subtitle locale is listed for this selection."
            });
            return;
        }
        let current = if audio { self.audio() } else { self.subs() };
        let next = locales
            .iter()
            .position(|locale| *locale == current)
            .map_or(0, |index| (index + 1) % locales.len());
        let chosen = locales[next].clone();
        if audio {
            self.options.audio_langs = vec![chosen.clone()];
            self.say(format!("Audio: {chosen}"));
        } else {
            self.options.subtitles_langs = vec![chosen.clone()];
            self.say(format!("Subtitles: {chosen}"));
        }
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

    fn cycle_sort(&mut self) {
        self.sort = (self.sort + 1) % super::SORTS.len();
        self.listing = Listing::Browse(self.sort);
        self.request_catalog();
    }

    fn reload(&mut self) {
        match self.focus {
            Focus::Series => self.request_catalog(),
            Focus::Seasons => self.request_seasons(),
            Focus::Episodes => self.request_episodes(),
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

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.editing.is_some() {
            self.edit_search(key);
            return Action::None;
        }
        if self.show_help {
            self.show_help = false;
            return Action::None;
        }
        self.notice = None;
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('/') => self.editing = Some(String::new()),
            KeyCode::Up | KeyCode::Char('k') => self.focused_pane_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.focused_pane_move(1),
            KeyCode::PageUp => self.focused_pane_move(-10),
            KeyCode::PageDown => self.focused_pane_move(10),
            KeyCode::Home | KeyCode::Char('g') => self.focused_pane_edge(false),
            KeyCode::End | KeyCode::Char('G') => self.focused_pane_edge(true),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => return self.descend(),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => self.ascend(),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Series => Focus::Seasons,
                    Focus::Seasons => Focus::Episodes,
                    Focus::Episodes => Focus::Series,
                }
            }
            KeyCode::Char('p') => return self.play(false),
            KeyCode::Char('P') => return self.play(true),
            KeyCode::Char('d') => return self.download(false),
            KeyCode::Char('D') => return self.download(true),
            KeyCode::Char('a') => self.cycle_locale(true),
            KeyCode::Char('s') => self.cycle_locale(false),
            KeyCode::Char('v') => self.cycle_quality(),
            KeyCode::Char('o') => self.cycle_sort(),
            KeyCode::Char('r') => self.reload(),
            _ => {}
        }
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::Pane;

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
        assert_eq!(pane.state.selected(), Some(2), "a page down stops at the end");
        assert_eq!(pane.selected(), Some(&"c"));

        pane.select_edge(false);
        assert_eq!(pane.state.selected(), Some(0));
        pane.clear();
        assert_eq!(pane.selected(), None);
    }
}
