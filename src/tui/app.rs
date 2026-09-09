use std::sync::{Arc, Mutex};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::download::DownloadOptions;
use crate::model::{CatalogItem, Season, SeasonEpisode};
use crate::util::{LANGUAGES, language_name};

use super::QUALITIES;
use super::art::Gallery;
use super::keys::{Bindings, Command};
use super::theme::Theme;
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
    /// What each key does.
    pub keys: Bindings,
    /// The posters and episode stills, and the terminal's ability to draw them.
    pub art: Gallery,
    pub focus: Focus,
    pub series: Pane<CatalogItem>,
    pub seasons: Pane<Season>,
    pub episodes: Pane<SeasonEpisode>,
    pub listing: Listing,
    /// The search box while it is being typed into.
    pub editing: Option<String>,
    /// The language list while it is open.
    pub picker: Option<Picker>,
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
            listing: Listing::Browse(0),
            editing: None,
            picker: None,
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

    fn cycle_sort(&mut self) {
        self.sort = (self.sort + 1) % super::SORTS.len();
        self.listing = Listing::Browse(self.sort);
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
    /// to look at the other list without going back out first.
    fn edit_picker(&mut self, command: Option<Command>) {
        let (Some(picker), Some(command)) = (self.picker.as_mut(), command) else {
            return;
        };
        match command {
            // Quitting out of the list leaves the list, not the interface.
            Command::Back | Command::Quit => self.picker = None,
            Command::Up => picker.pane.move_by(-1),
            Command::Down => picker.pane.move_by(1),
            Command::PageUp => picker.pane.move_by(-10),
            Command::PageDown => picker.pane.move_by(10),
            Command::First => picker.pane.select_edge(false),
            Command::Last => picker.pane.select_edge(true),
            Command::NextPane => {
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
        // ctrl-c is the terminal's own way out, and stays wired to quitting whatever the
        // config file says - including while the search box has the keyboard.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        // The search box takes letters as letters, so nothing is looked up while it is
        // open.
        if self.editing.is_some() {
            self.edit_search(key);
            return Action::None;
        }
        let command = self.keys.command(key);
        if self.picker.is_some() {
            self.notice = None;
            self.edit_picker(command);
            return Action::None;
        }
        // Any key at all puts the help away, bound or not.
        if self.show_help {
            self.show_help = false;
            return Action::None;
        }
        self.notice = None;
        let Some(command) = command else {
            return Action::None;
        };
        match command {
            Command::Quit => return Action::Quit,
            Command::Help => self.show_help = true,
            Command::Search => self.editing = Some(String::new()),
            Command::Up => self.focused_pane_move(-1),
            Command::Down => self.focused_pane_move(1),
            Command::PageUp => self.focused_pane_move(-10),
            Command::PageDown => self.focused_pane_move(10),
            Command::First => self.focused_pane_edge(false),
            Command::Last => self.focused_pane_edge(true),
            Command::Open => return self.descend(),
            Command::Back => self.ascend(),
            Command::NextPane => {
                self.focus = match self.focus {
                    Focus::Series => Focus::Seasons,
                    Focus::Seasons => Focus::Episodes,
                    Focus::Episodes => Focus::Series,
                }
            }
            Command::Play => return self.play(false),
            Command::PlaySeason => return self.play(true),
            Command::Download => return self.download(false),
            Command::DownloadSeason => return self.download(true),
            Command::AudioLanguage => self.open_picker(true),
            Command::SubtitleLanguage => self.open_picker(false),
            Command::NextAudio => self.cycle_locale(true),
            Command::NextSubtitle => self.cycle_locale(false),
            Command::Quality => self.cycle_quality(),
            Command::Images => {
                let message = self.art.toggle();
                self.say(message);
            }
            Command::Sort => self.cycle_sort(),
            Command::Reload => self.reload(),
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
}
