use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, HighlightSpacing, List, ListItem, Paragraph, Wrap};

use crate::model::{CatalogItem, Season, SeasonEpisode};
use crate::util::language_name;

use super::app::{App, Focus, Picker};
use super::theme::Theme;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// `1461000` becomes `24:21`, and an hour-long special `1:02:03`.
fn duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn pane_block(theme: &Theme, title: &str, focused: bool) -> Block<'static> {
    let heading = if focused {
        theme.title(format!(" {title} "))
    } else {
        Span::styled(format!(" {title} "), Style::new().fg(theme.heading))
    };
    theme.bordered(focused).title(heading)
}

/// What a column shows when it holds nothing: why it is empty, or that it is still
/// waiting for an answer.
fn placeholder(
    theme: &Theme,
    loading: bool,
    error: Option<&String>,
    idle: &str,
    tick: usize,
) -> Vec<ListItem<'static>> {
    let line = if loading {
        Line::from(vec![
            theme.accent(SPINNER[tick % SPINNER.len()]),
            theme.text(" Loading..."),
        ])
    } else if let Some(error) = error {
        Line::from(theme.error(error.clone()))
    } else {
        Line::from(theme.dim(idle.to_owned()))
    };
    vec![ListItem::new(line)]
}

fn series_row(theme: &Theme, series: &CatalogItem) -> ListItem<'static> {
    let metadata = &series.series_metadata;
    let mut tags = Vec::new();
    if metadata.season_count > 1 {
        tags.push(format!("{} seasons", metadata.season_count));
    }
    if metadata.is_dubbed {
        tags.push("dub".to_owned());
    }
    let mut spans = vec![theme.text(series.title.clone())];
    if !tags.is_empty() {
        spans.push(theme.dim(format!("  {}", tags.join(" · "))));
    }
    ListItem::new(Line::from(spans))
}

fn season_row(theme: &Theme, season: &Season, series_title: &str) -> ListItem<'static> {
    // A season usually carries the title of the series, which the column to the left
    // is already showing.
    let title = if season.title.is_empty() || season.title == series_title {
        format!("Season {}", season.season_number)
    } else {
        season.title.clone()
    };
    let mut spans = vec![theme.text(title)];
    if season.number_of_episodes > 0 {
        spans.push(theme.dim(format!("  {} ep", season.number_of_episodes)));
    }
    ListItem::new(Line::from(spans))
}

fn episode_row(theme: &Theme, episode: &SeasonEpisode) -> ListItem<'static> {
    let number = if episode.episode.is_empty() {
        episode.episode_number.to_string()
    } else {
        episode.episode.clone()
    };
    let mut spans = vec![theme.accent(format!("E{number:<3}"))];
    if episode.duration_ms > 0 {
        spans.push(theme.dim(format!("{:>6}  ", duration(episode.duration_ms))));
    }
    spans.push(theme.text(episode.title.clone()));
    ListItem::new(Line::from(spans))
}

fn header(app: &App) -> Paragraph<'static> {
    let theme = &app.theme;
    let left = match &app.editing {
        Some(query) => Line::from(vec![
            theme.accent("Search: "),
            theme.text(query.clone()),
            theme.accent("▏"),
        ]),
        None => Line::from(vec![
            theme.strong(app.listing.label()),
            theme.dim(format!("   {} series", app.series.items.len())),
        ]),
    };
    let right = Line::from(vec![
        theme.dim("audio "),
        theme.accent(language_name(&app.audio()).to_owned()),
        theme.dim("  subs "),
        theme.accent(language_name(&app.subs()).to_owned()),
        theme.dim("  video "),
        theme.accent(app.options.video_quality.clone()),
        theme.text(" "),
    ])
    .right_aligned();
    let block = theme
        .bordered(false)
        .title(theme.title(" Crunchyroll "))
        .title_top(right);
    Paragraph::new(left).block(block)
}

/// The panel under the columns: everything about the item the cursor is on that does
/// not fit on its one line.
fn details(app: &App) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines = Vec::new();
    match app.focus {
        Focus::Series | Focus::Seasons => {
            let Some(series) = app.series.selected() else {
                return lines;
            };
            let metadata = &series.series_metadata;
            lines.push(Line::from(theme.strong(series.title.clone())));
            let mut facts = Vec::new();
            if metadata.series_launch_year > 0 {
                facts.push(metadata.series_launch_year.to_string());
            }
            if metadata.episode_count > 0 {
                facts.push(format!("{} episodes", metadata.episode_count));
            }
            if !metadata.audio_locales.is_empty() {
                facts.push(format!("{} audio tracks", metadata.audio_locales.len()));
            }
            if !metadata.subtitle_locales.is_empty() {
                facts.push(format!("{} subtitles", metadata.subtitle_locales.len()));
            }
            facts.extend(metadata.maturity_ratings.iter().cloned());
            if metadata.is_simulcast {
                facts.push("simulcast".to_owned());
            }
            lines.push(Line::from(theme.dim(facts.join(" · "))));
            lines.push(Line::from(theme.text(series.description.clone())));
        }
        Focus::Episodes => {
            let Some(episode) = app.episodes.selected() else {
                return lines;
            };
            lines.push(Line::from(theme.strong(episode.title.clone())));
            let mut facts = vec![format!(
                "S{}E{}",
                episode.season_number,
                if episode.episode.is_empty() {
                    episode.episode_number.to_string()
                } else {
                    episode.episode.clone()
                }
            )];
            if episode.duration_ms > 0 {
                facts.push(duration(episode.duration_ms));
            }
            if !episode.audio_locale.is_empty() {
                facts.push(language_name(&episode.audio_locale).to_owned());
            }
            if episode.versions.len() > 1 {
                facts.push(format!("{} dubs", episode.versions.len()));
            }
            if let Some(date) = episode.availability_starts.split('T').next()
                && !date.is_empty()
            {
                facts.push(date.to_owned());
            }
            lines.push(Line::from(theme.dim(facts.join(" · "))));
            lines.push(Line::from(theme.text(episode.description.clone())));
        }
    }
    lines
}

/// A box in the middle of the screen, as tall as it needs to be and no taller than the
/// terminal allows.
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// The language list: every locale the selection offers, the one in use marked, and its
/// code beside the name for anyone who thinks in locales rather than in languages.
fn picker_overlay(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    picker: &mut Picker,
    current: &str,
) {
    let column = picker
        .pane
        .items
        .iter()
        .map(|locale| Span::raw(language_name(locale)).width())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = picker
        .pane
        .items
        .iter()
        .map(|locale| {
            let name = language_name(locale);
            let padding = " ".repeat(column - Span::raw(name).width() + 2);
            ListItem::new(Line::from(vec![
                theme.text(if locale == current { "● " } else { "  " }),
                theme.text(format!("{name}{padding}")),
                theme.dim(locale.clone()),
            ]))
        })
        .collect();
    // Wide enough for the longest language name, and never so narrow that the hint
    // along the bottom edge is cut in half.
    let area = popup(
        area,
        (column as u16 + 20).max(42),
        picker.pane.items.len() as u16 + 2,
    );
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                theme
                    .bordered(true)
                    .title(theme.title(picker.title()))
                    .title_bottom(theme.dim(" ⏎ apply · tab other list · esc cancel ")),
            )
            .highlight_style(theme.highlight(true))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        area,
        &mut picker.pane.state,
    );
}

fn help_overlay(frame: &mut Frame, area: Rect, theme: &Theme) {
    let keys = [
        ("↑ ↓ / j k", "move the cursor"),
        ("⏎ / → / l", "open the selection, and play an episode"),
        ("← / h / esc", "go back a column, and leave a search"),
        ("tab", "cycle the columns"),
        ("/", "search the catalogue"),
        ("o", "change the browse order"),
        ("p / P", "play the episode / the rest of the season"),
        ("d / D", "download the episode / the whole season"),
        ("a / s", "pick the audio / subtitle language"),
        ("A / S", "next audio / subtitle language, without the list"),
        ("v", "cycle the video quality"),
        ("r", "reload the current column"),
        ("g / G", "jump to the first or last item"),
        ("q", "quit"),
    ];
    let popup = popup(area, 66, keys.len() as u16 + 2);
    let lines: Vec<Line> = keys
        .iter()
        .map(|(key, what)| Line::from(vec![theme.accent(format!(" {key:<13}")), theme.text(*what)]))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(theme.bordered(true).title(theme.title(" Keys "))),
        popup,
    );
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let details_height = match area.height {
        0..=15 => 0,
        16..=21 => 5,
        _ => 7,
    };
    let [top, body, bottom, status, keys] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(details_height),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(header(app), top);

    let [left, middle, right] = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Percentage(22),
        Constraint::Percentage(44),
    ])
    .areas(body);

    // Copied out before the panes borrow the app to draw themselves.
    let theme = app.theme;
    let tick = app.tick;
    let focus = app.focus;

    let items: Vec<ListItem> = if app.series.items.is_empty() {
        placeholder(
            &theme,
            app.series.loading,
            app.series.error.as_ref(),
            "Nothing here.",
            tick,
        )
    } else {
        app.series
            .items
            .iter()
            .map(|series| series_row(&theme, series))
            .collect()
    };
    let focused = focus == Focus::Series;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&theme, "Series", focused))
            .highlight_style(theme.highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        left,
        &mut app.series.state,
    );

    let items: Vec<ListItem> = if app.seasons.items.is_empty() {
        placeholder(
            &theme,
            app.seasons.loading,
            app.seasons.error.as_ref(),
            "Pick a series.",
            tick,
        )
    } else {
        let series_title = app
            .series
            .selected()
            .map_or("", |series| series.title.as_str());
        app.seasons
            .items
            .iter()
            .map(|season| season_row(&theme, season, series_title))
            .collect()
    };
    let focused = focus == Focus::Seasons;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&theme, "Seasons", focused))
            .highlight_style(theme.highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        middle,
        &mut app.seasons.state,
    );

    let items: Vec<ListItem> = if app.episodes.items.is_empty() {
        placeholder(
            &theme,
            app.episodes.loading,
            app.episodes.error.as_ref(),
            "Pick a season.",
            tick,
        )
    } else {
        app.episodes
            .items
            .iter()
            .map(|episode| episode_row(&theme, episode))
            .collect()
    };
    let focused = focus == Focus::Episodes;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&theme, "Episodes", focused))
            .highlight_style(theme.highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        right,
        &mut app.episodes.state,
    );

    if details_height > 0 {
        frame.render_widget(
            Paragraph::new(details(app))
                .wrap(Wrap { trim: true })
                .block(theme.bordered(false).title(theme.dim(" Details "))),
            bottom,
        );
    }

    let line = match &app.notice {
        Some(notice) if notice.error => Line::from(theme.error(format!(" {}", notice.text))),
        Some(notice) => Line::from(theme.text(format!(" {}", notice.text))),
        None => Line::from(theme.dim(" Ready.")),
    };
    frame.render_widget(Paragraph::new(line), status);

    frame.render_widget(
        Paragraph::new(Line::from(theme.dim(
            " ↑↓ move   ⏎ open/play   ← back   / search   d download   a/s language   v quality   ? keys   q quit",
        ))),
        keys,
    );

    if app.show_help {
        help_overlay(frame, area, &theme);
    }

    // Read what is in use before the list borrows the app to draw itself.
    let current = app.picker.as_ref().map(|picker| {
        if picker.audio {
            app.audio()
        } else {
            app.subs()
        }
    });
    if let (Some(current), Some(picker)) = (current, app.picker.as_mut()) {
        picker_overlay(frame, area, &theme, picker, &current);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::style::Color;

    use crate::download::DownloadOptions;
    use crate::model::{CatalogItem, Season, SeasonEpisode, SeriesMetadata};
    use crate::tui::app::App;
    use crate::tui::theme::{self, Theme};
    use crate::tui::worker::Worker;

    use super::{draw, duration};

    #[test]
    fn formats_a_running_time() {
        assert_eq!(duration(1_461_000), "24:21");
        assert_eq!(duration(3_723_000), "1:02:03");
        assert_eq!(duration(9_000), "0:09");
    }

    fn app() -> App {
        themed(Theme::default())
    }

    fn themed(theme: Theme) -> App {
        let options = DownloadOptions {
            audio_langs: vec!["ja-JP".to_owned()],
            subtitles_langs: vec!["en-US".to_owned()],
            cc_langs: Vec::new(),
            video_quality: "1080p".to_owned(),
            audio_quality: "192k".to_owned(),
            play: false,
            mpv_args: Vec::new(),
        };
        let mut app = App::new(
            Worker::detached(),
            options,
            theme,
            Arc::new(Mutex::new(Vec::new())),
        );
        app.series.set(vec![CatalogItem {
            id: "GY5P48XEY".to_owned(),
            kind: "series".to_owned(),
            title: "Frieren".to_owned(),
            description: "An elf outlives her party.".to_owned(),
            series_metadata: SeriesMetadata {
                episode_count: 28,
                season_count: 1,
                series_launch_year: 2023,
                ..SeriesMetadata::default()
            },
        }]);
        app.seasons.set(vec![Season {
            id: "S1".to_owned(),
            season_number: 1,
            title: "Frieren".to_owned(),
            number_of_episodes: 28,
            audio_locales: vec!["ja-JP".to_owned(), "en-US".to_owned(), "fr-FR".to_owned()],
            subtitle_locales: vec!["en-US".to_owned(), "fr-FR".to_owned()],
        }]);
        app.episodes.set(vec![SeasonEpisode {
            id: "E1".to_owned(),
            episode: "1".to_owned(),
            episode_number: 1,
            season_number: 1,
            title: "The Journey Ends".to_owned(),
            duration_ms: 1_461_000,
            ..SeasonEpisode::default()
        }]);
        app
    }

    fn buffer(width: u16, height: u16, app: &mut App) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        terminal.backend().buffer().clone()
    }

    /// The first cell of the cursor row of the focused column: the arrow the list draws
    /// in front of the selected item.
    fn cursor_cell(buffer: &Buffer) -> ratatui::buffer::Cell {
        buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "\u{203a}")
            .expect("a cursor on the focused column")
            .clone()
    }

    fn rendered(width: u16, height: u16, app: &mut App) -> String {
        buffer(width, height, app)
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn draws_every_column() {
        let mut app = app();
        let screen = rendered(120, 30, &mut app);
        for expected in [
            "Crunchyroll",
            "Series",
            "Seasons",
            "Episodes",
            "Frieren",
            "The Journey Ends",
            "24:21",
            "An elf outlives her party.",
            "1080p",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
    }

    /// Nothing is worth less to someone with a colourscheme than an app that ignores it,
    /// so out of the box every colour has to be one the terminal resolves.
    #[test]
    fn asks_for_no_colour_of_its_own() {
        let mut app = app();
        app.show_help = true;
        for cell in buffer(120, 30, &mut app).content() {
            for color in [cell.fg, cell.bg] {
                assert!(
                    !matches!(color, Color::Rgb(..)),
                    "the default theme named {color:?} instead of leaving it to the terminal"
                );
            }
        }
    }

    /// The cursor row on a light scheme is the one place a hard-coded black would show:
    /// it has to be the theme's own page colour, whatever that is.
    #[test]
    fn paints_the_cursor_row_in_the_theme_background() {
        let latte = theme::named("catppuccin-latte").expect("a shipped theme");
        let mut app = themed(latte);
        // The series column has the keyboard, and its one row is under the cursor.
        let buffer = buffer(120, 30, &mut app);
        let cell = cursor_cell(&buffer);
        assert_eq!(cell.bg, latte.accent);
        assert_eq!(cell.fg, latte.background);
        assert_ne!(cell.fg, Color::Black);

        // Including the text of the row, which carries colours of its own until the
        // cursor lands on it.
        let title = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "F")
            .expect("the title of the selected series");
        assert_eq!(title.bg, latte.accent);
        assert_eq!(title.fg, latte.background);
    }

    /// Without a theme there is no background colour to name, so the terminal is asked to
    /// swap the two round itself rather than being told to paint anything.
    #[test]
    fn leaves_the_cursor_row_to_the_terminal_by_default() {
        let mut app = app();
        let cell = cursor_cell(&buffer(120, 30, &mut app));
        assert_eq!(cell.fg, Color::Yellow);
        assert_eq!(cell.bg, Color::Reset);
        assert!(cell.modifier.contains(ratatui::style::Modifier::REVERSED));
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::from(code));
    }

    /// The language list is the way a locale gets changed, so it has to offer what the
    /// season has, say which one is in use, and hand the choice back to the options.
    #[test]
    fn picks_a_language_from_the_list() {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        let screen = rendered(120, 30, &mut app);
        for expected in ["Audio language", "English", "en-US", "Français", "fr-FR"] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }

        // The list is sorted and opens on what is in use - ja-JP, last of the three - so
        // one step up lands on fr-FR.
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert!(app.picker.is_none(), "choosing closes the list");
        assert_eq!(app.audio(), "fr-FR");
        assert!(rendered(120, 30, &mut app).contains("Audio: Français"));

        // Tab looks at the other list without going back out, and esc changes nothing.
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Tab);
        assert!(app.picker.as_ref().is_some_and(|picker| picker.audio));
        press(&mut app, KeyCode::Esc);
        assert!(app.picker.is_none(), "esc closes the list");
        assert_eq!(app.audio(), "fr-FR");
    }

    /// A locale nothing lists is still the one in use, so the list has to keep offering
    /// it rather than opening on someone else's language.
    #[test]
    fn offers_the_locale_in_use_whatever_the_season_says() {
        let mut app = app();
        app.options.audio_langs = vec!["de-DE".to_owned()];
        press(&mut app, KeyCode::Char('a'));
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("Deutsch"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.audio(), "de-DE", "the list opens on what is in use");
    }

    /// Every panel is optional except the columns, so a terminal too short for the
    /// details or too narrow for the help popup still has to draw rather than panic.
    #[test]
    fn survives_a_cramped_terminal() {
        let mut app = app();
        for (width, height) in [(120, 14), (40, 10), (20, 6), (8, 4)] {
            let screen = rendered(width, height, &mut app);
            assert!(!screen.is_empty());
        }
        app.show_help = true;
        for (width, height) in [(120, 30), (20, 6), (8, 4)] {
            let screen = rendered(width, height, &mut app);
            assert!(!screen.is_empty());
        }
        app.show_help = false;
        press(&mut app, KeyCode::Char('a'));
        for (width, height) in [(120, 30), (20, 6), (8, 4)] {
            let screen = rendered(width, height, &mut app);
            assert!(!screen.is_empty());
        }
    }
}
