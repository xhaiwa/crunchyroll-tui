use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, HighlightSpacing, List, ListItem, Paragraph, Wrap};

use crate::model::{CatalogItem, Season, SeasonEpisode};
use crate::util::language_name;

use super::app::{App, Focus, Picker};
use super::keys::{Bindings, HELP, HINTS};
use super::theme::Theme;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The narrowest the columns may be squeezed to before the poster is given up: below this
/// the three lists are already fighting for room and a picture is a luxury.
const COLUMNS_NEED: u16 = 62;

/// The most columns the poster may take. A poster is a nice thing to look at; it is not
/// what the interface is for.
const POSTER_LIMIT: u16 = 34;

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

/// A run of cells turned into the pixels behind it, which is what the CDN is asked for:
/// a poster twenty columns wide on a ten-pixel font is a two-hundred-pixel poster.
fn pixels(cells: u16, cell_width: u16) -> u32 {
    u32::from(cells) * u32::from(cell_width)
}

/// How many columns the poster gets: enough for a two-by-three picture to fill the height
/// the lists have, and none at all when the artwork is off or the terminal cannot spare
/// the room.
///
/// `cell` is the terminal's character size in pixels, which is the only thing that turns
/// a number of rows into the number of columns a given shape needs.
fn poster_width(body: Rect, cell: Size, enabled: bool) -> u16 {
    let room = body.width.saturating_sub(COLUMNS_NEED);
    if !enabled || cell.width == 0 || room == 0 {
        return 0;
    }
    // The inside of the panel in pixels, at two by three, back into columns; plus the two
    // the border takes.
    let tall = u32::from(body.height.saturating_sub(2)) * u32::from(cell.height);
    let wide = (tall * 2 / 3).div_ceil(u32::from(cell.width));
    let wanted = u16::try_from(wide)
        .unwrap_or(POSTER_LIMIT)
        .saturating_add(2);
    // A sliver of poster is worse than none: it is a picture nobody can make out sitting
    // where a list could have been.
    match wanted.min(POSTER_LIMIT).min(room) {
        0..=9 => 0,
        width => width,
    }
}

/// How many columns the episode still gets: sixteen by nine, as tall as the details panel
/// is, and never more than a third of it so the description keeps somewhere to go.
fn thumbnail_width(inner: Rect, cell: Size, enabled: bool) -> u16 {
    if !enabled || cell.width == 0 || inner.height == 0 {
        return 0;
    }
    let tall = u32::from(inner.height) * u32::from(cell.height);
    let wide = (tall * 16 / 9).div_ceil(u32::from(cell.width));
    let wanted = u16::try_from(wide).unwrap_or(u16::MAX);
    match wanted.min(inner.width / 3) {
        0..=7 => 0,
        width => width,
    }
}

/// One row across the middle of an area, for the word that stands in for a picture that
/// has not arrived. An empty bordered box reads as a bug.
fn middle_row(area: Rect) -> Rect {
    Rect {
        y: area.y + area.height / 2,
        height: area.height.min(1),
        ..area
    }
}

fn waiting(theme: &Theme) -> Paragraph<'static> {
    Paragraph::new(Line::from(theme.dim("..."))).centered()
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

/// The help overlay, built from the bindings in force rather than from a list written
/// out here: a rebound key is no help at all if the overlay still names the old one. A
/// row whose commands have all been unbound is left out.
fn help_overlay(frame: &mut Frame, area: Rect, theme: &Theme, keys: &Bindings) {
    let rows: Vec<(String, &str)> = HELP
        .iter()
        .filter_map(|(group, what)| Some((keys.label(group, true)?, *what)))
        .collect();
    // Wide enough for the keys someone actually bound, within reason: past that the
    // description is worth more of the line than a fourth spelling of "down".
    let column = rows
        .iter()
        .map(|(label, _)| Span::raw(label).width())
        .max()
        .unwrap_or(0)
        .clamp(13, 26);
    let popup = popup(area, column as u16 + 53, rows.len() as u16 + 2);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(label, what)| {
            let padding = " ".repeat(column.saturating_sub(Span::raw(label).width()));
            Line::from(vec![
                theme.accent(format!(" {label}{padding}")),
                theme.text(*what),
            ])
        })
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(theme.bordered(true).title(theme.title(" Keys "))),
        popup,
    );
}

/// The reminder along the bottom, in the same words as the overlay and off the same
/// bindings, cut to the first key of each command so it stays one line.
fn hint_line(keys: &Bindings) -> String {
    HINTS
        .iter()
        .filter_map(|(group, what)| Some(format!("{} {what}", keys.label(group, false)?)))
        .collect::<Vec<_>>()
        .join("   ")
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // Copied out before the panes borrow the app to draw themselves.
    let theme = app.theme;
    let tick = app.tick;
    let focus = app.focus;
    let art = app.art.enabled();
    let cell = app.art.cell();

    let details_height = match area.height {
        0..=15 => 0,
        16..=21 => 5,
        22..=27 => 7,
        // Two more rows once there is a still to put in the panel: seven rows of picture
        // is about as small as a sixteen-by-nine frame gets and stays a picture.
        _ if art => 9,
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

    // The poster takes a column off the left of the body and the three lists share what
    // is left, in the proportions they had the whole width in.
    let panel = poster_width(body, cell, art);
    let poster = app
        .series
        .selected()
        .filter(|_| panel > 0)
        .and_then(|series| {
            series
                .images
                .poster(pixels(panel.saturating_sub(2), cell.width))
        })
        .map(str::to_owned);
    let [poster_area, body] = Layout::horizontal([
        Constraint::Length(if poster.is_some() { panel } else { 0 }),
        Constraint::Min(0),
    ])
    .areas(body);

    if let Some(url) = poster {
        let block = theme.bordered(false).title(theme.dim(" Poster "));
        let inner = block.inner(poster_area);
        frame.render_widget(block, poster_area);
        if !app.art.draw(frame, inner, &url) {
            frame.render_widget(waiting(&theme), middle_row(inner));
        }
    }

    let [left, middle, right] = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Percentage(22),
        Constraint::Percentage(44),
    ])
    .areas(body);

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
        let block = theme.bordered(false).title(theme.dim(" Details "));
        let inner = block.inner(bottom);
        frame.render_widget(block, bottom);

        // The still belongs to the episode under the cursor, so it appears alongside the
        // episode's own facts and not while a series is being looked at.
        let panel = thumbnail_width(inner, cell, art && focus == Focus::Episodes);
        let still = app
            .episodes
            .selected()
            .filter(|_| panel > 0)
            .and_then(|episode| episode.images.thumbnail(pixels(panel, cell.width)))
            .map(str::to_owned);
        let [still_area, _gap, text] = Layout::horizontal([
            Constraint::Length(if still.is_some() { panel } else { 0 }),
            Constraint::Length(if still.is_some() { 2 } else { 0 }),
            Constraint::Min(0),
        ])
        .areas(inner);

        let lines = details(app);
        if let Some(url) = still
            && !app.art.draw(frame, still_area, &url)
        {
            frame.render_widget(waiting(&theme), middle_row(still_area));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), text);
    }

    let line = match &app.notice {
        Some(notice) if notice.error => Line::from(theme.error(format!(" {}", notice.text))),
        Some(notice) => Line::from(theme.text(format!(" {}", notice.text))),
        None => Line::from(theme.dim(" Ready.")),
    };
    frame.render_widget(Paragraph::new(line), status);

    frame.render_widget(
        Paragraph::new(Line::from(theme.dim(format!(" {}", hint_line(&app.keys))))),
        keys,
    );

    if app.show_help {
        help_overlay(frame, area, &theme, &app.keys);
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
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::{Rect, Size};
    use ratatui::style::Color;

    use image::{DynamicImage, Rgb, RgbImage};

    use crate::download::DownloadOptions;
    use crate::model::{Artwork, CatalogItem, Images, Season, SeasonEpisode, SeriesMetadata};
    use crate::tui::app::{App, Focus};
    use crate::tui::art::Gallery;
    use crate::tui::keys::{self, Bindings};
    use crate::tui::theme::{self, Theme};
    use crate::tui::worker::Worker;

    use super::{draw, duration, poster_width, thumbnail_width};

    #[test]
    fn formats_a_running_time() {
        assert_eq!(duration(1_461_000), "24:21");
        assert_eq!(duration(3_723_000), "1:02:03");
        assert_eq!(duration(9_000), "0:09");
    }

    /// Where the test artwork lives. Nothing ever fetches these: the pictures are put
    /// into the gallery by hand.
    const POSTER: &str = "https://img.example/poster.jpg";
    const STILL: &str = "https://img.example/still.jpg";

    /// One set of artwork, shaped the way the API sends it: a list of lists.
    fn artwork(url: &str, width: u32) -> Vec<Vec<Artwork>> {
        vec![vec![Artwork {
            width,
            source: url.to_owned(),
        }]]
    }

    fn app() -> App {
        themed(Theme::default(), Gallery::detached(false))
    }

    /// A picture with something in it. A flat colour would draw as blank cells: the
    /// half-block protocol only puts a character down where the top and bottom halves of
    /// a cell differ, so the test picture is a gradient rather than one shade.
    fn picture(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        }))
    }

    /// An interface with the artwork switched on and both pictures already in hand, so a
    /// draw has everything it needs without a byte going over the wire.
    fn illustrated() -> App {
        let mut gallery = Gallery::detached(true);
        gallery.preload(POSTER, picture(240, 360));
        gallery.preload(STILL, picture(320, 180));
        themed(Theme::default(), gallery)
    }

    fn themed(theme: Theme, art: Gallery) -> App {
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
            Bindings::default(),
            Arc::new(Mutex::new(Vec::new())),
            art,
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
            images: Images {
                poster_tall: artwork(POSTER, 360),
                ..Images::default()
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
            images: Images {
                thumbnail: artwork(STILL, 320),
                ..Images::default()
            },
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
        let mut app = themed(latte, Gallery::detached(false));
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

    fn press_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.on_key(KeyEvent::new(code, modifiers));
    }

    /// Puts a `[keys]` section in force, the way the config file would.
    fn bound(app: &mut App, section: &str) {
        let settings: keys::Settings = toml::from_str(section).expect("valid keys");
        let (bindings, warnings) = keys::resolve(&settings);
        assert!(warnings.is_empty(), "{warnings:?}");
        app.keys = bindings;
    }

    /// The overlay and the line along the bottom are where someone finds out what a key
    /// does, so both have to follow the config file rather than a list written out
    /// beside them.
    #[test]
    fn the_help_follows_the_bindings() {
        let mut app = app();
        app.show_help = true;
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("↑ k / ↓ j"), "{screen}");
        assert!(screen.contains("d / D"), "{screen}");
        assert!(screen.contains("d download"), "the reminder line: {screen}");

        bound(&mut app, "download = \"ctrl-s\"\nquality = []\n");
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("ctrl-s / D"), "{screen}");
        assert!(screen.contains("ctrl-s download"), "{screen}");
        assert!(
            !screen.contains("cycle the video quality"),
            "an unbound command has no row: {screen}"
        );
    }

    /// A key that was moved reaches its command, the one it left behind does nothing,
    /// and the search box still takes letters as letters.
    #[test]
    fn a_rebound_key_drives_the_interface() {
        let mut app = app();
        bound(
            &mut app,
            "open = \"space\"\nsearch = \"ctrl-f\"\nback = \"esc\"\n",
        );

        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.focus, Focus::Seasons, "space opens the selection");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Seasons, "enter was given up");

        press_with(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(app.editing.as_deref(), Some(""));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            app.editing.as_deref(),
            Some(" "),
            "a bound key is still a letter inside the search box"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.editing, None);
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.editing, None, "the default search key was given up");

        // The language list follows the same bindings, and leaving it leaves the list
        // rather than the interface.
        press(&mut app, KeyCode::Char('a'));
        assert!(app.picker.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.picker.is_none());
        assert!(!app.quit);
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

    /// How many cells in `rows` are part of a picture. Half-blocks are what the fallback
    /// protocol draws with, and the only protocol a test can count on being able to use.
    fn picture_cells(buffer: &Buffer, rows: std::ops::Range<u16>) -> usize {
        rows.flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .filter(|(x, y)| matches!(buffer[(*x, *y)].symbol(), "\u{2580}" | "\u{2584}"))
            .count()
    }

    /// The whole point: the poster of the selected series in a column of its own, drawn
    /// as pixels, with the three lists still there beside it.
    #[test]
    fn draws_the_poster_beside_the_columns() {
        let mut app = illustrated();
        let buffer = buffer(120, 30, &mut app);
        let screen: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(screen.contains("Poster"), "no poster panel");
        // Inside the poster panel's border, which is the body less its own top and bottom.
        assert!(
            picture_cells(&buffer, 4..18) > 100,
            "the poster panel was left empty"
        );
        for expected in ["Series", "Seasons", "Episodes", "The Journey Ends"] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
    }

    /// The still belongs to the episode, so it turns up when the episode column has the
    /// keyboard and not while a series is being looked at.
    #[test]
    fn draws_the_still_only_beside_the_episode_details() {
        let mut app = illustrated();
        let details = 20..28;
        assert_eq!(
            picture_cells(&buffer(120, 30, &mut app), details.clone()),
            0,
            "a series has no still to show"
        );

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Episodes);
        let buffer = buffer(120, 30, &mut app);
        assert!(picture_cells(&buffer, details) > 40, "no still was drawn");
    }

    /// Nobody wants a picture they cannot turn off, and the answer to "why can I not see
    /// the posters" should be one keypress away.
    #[test]
    fn i_puts_the_artwork_away_and_says_what_drew_it() {
        let mut app = illustrated();
        assert!(rendered(120, 30, &mut app).contains("Poster"));

        press(&mut app, KeyCode::Char('i'));
        let screen = rendered(120, 30, &mut app);
        assert!(!screen.contains("Poster"), "the panel is still there");
        assert!(screen.contains("Artwork off"));

        press(&mut app, KeyCode::Char('i'));
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("Poster"), "the panel did not come back");
        assert!(screen.contains("half-blocks"), "no word on what drew it");
    }

    /// The poster keeps a two-by-three shape whatever the terminal's font is, and gives
    /// itself up rather than squeeze the three lists into nothing.
    #[test]
    fn the_poster_column_keeps_its_shape() {
        let cell = Size::new(10, 20);
        // Fourteen rows inside the border are 280 pixels, so 186 across, so 19 columns
        // and the two the border takes.
        assert_eq!(poster_width(Rect::new(0, 3, 120, 16), cell, true), 21);
        assert_eq!(poster_width(Rect::new(0, 3, 120, 16), cell, false), 0);
        // A tall, narrow cell needs more columns for the same picture.
        assert_eq!(
            poster_width(Rect::new(0, 3, 120, 16), Size::new(7, 21), true),
            30
        );
        // And a terminal with nothing to spare keeps its columns and loses the picture.
        assert_eq!(poster_width(Rect::new(0, 3, 70, 16), cell, true), 0);
        assert_eq!(poster_width(Rect::new(0, 3, 40, 16), cell, true), 0);
    }

    /// The still is sixteen by nine and never takes more than a third of the panel, so
    /// the description always has somewhere to go.
    #[test]
    fn the_still_leaves_room_for_the_description() {
        let cell = Size::new(10, 20);
        // Seven rows are 140 pixels, so 249 across, so 25 columns.
        assert_eq!(thumbnail_width(Rect::new(1, 20, 118, 7), cell, true), 25);
        assert_eq!(thumbnail_width(Rect::new(1, 20, 118, 7), cell, false), 0);
        assert_eq!(thumbnail_width(Rect::new(1, 20, 60, 7), cell, true), 20);
        assert_eq!(thumbnail_width(Rect::new(1, 20, 20, 7), cell, true), 0);
    }

    /// Every panel is optional except the columns, so a terminal too short for the
    /// details or too narrow for the help popup still has to draw rather than panic.
    #[test]
    fn survives_a_cramped_terminal() {
        let mut illustrated = illustrated();
        for (width, height) in [(120, 14), (40, 10), (20, 6), (8, 4), (1, 1)] {
            let screen = rendered(width, height, &mut illustrated);
            assert!(!screen.is_empty());
        }

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
