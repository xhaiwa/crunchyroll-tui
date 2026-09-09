use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, HighlightSpacing, List, ListItem, Paragraph, Wrap};

use crate::model::{CatalogItem, Season, SeasonEpisode};
use crate::util::language_name;

use super::app::{App, Focus};

const ACCENT: Color = Color::Rgb(244, 117, 33);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(Color::DarkGray))
}

fn accent(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(ACCENT))
}

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

fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let (border, heading) = if focused {
        (
            Style::new().fg(ACCENT),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            Style::new().fg(Color::DarkGray),
            Style::new().fg(Color::Gray),
        )
    };
    Block::bordered()
        .border_style(border)
        .title(Span::styled(format!(" {title} "), heading))
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    }
}

/// What a column shows when it holds nothing: why it is empty, or that it is still
/// waiting for an answer.
fn placeholder(
    loading: bool,
    error: Option<&String>,
    idle: &str,
    tick: usize,
) -> Vec<ListItem<'static>> {
    let line = if loading {
        Line::from(vec![
            accent(SPINNER[tick % SPINNER.len()]),
            Span::raw(" Loading..."),
        ])
    } else if let Some(error) = error {
        Line::from(Span::styled(
            error.clone(),
            Style::new().fg(Color::LightRed),
        ))
    } else {
        Line::from(dim(idle.to_owned()))
    };
    vec![ListItem::new(line)]
}

fn series_row(series: &CatalogItem) -> ListItem<'static> {
    let metadata = &series.series_metadata;
    let mut tags = Vec::new();
    if metadata.season_count > 1 {
        tags.push(format!("{} seasons", metadata.season_count));
    }
    if metadata.is_dubbed {
        tags.push("dub".to_owned());
    }
    let mut spans = vec![Span::raw(series.title.clone())];
    if !tags.is_empty() {
        spans.push(dim(format!("  {}", tags.join(" · "))));
    }
    ListItem::new(Line::from(spans))
}

fn season_row(season: &Season, series_title: &str) -> ListItem<'static> {
    // A season usually carries the title of the series, which the column to the left
    // is already showing.
    let title = if season.title.is_empty() || season.title == series_title {
        format!("Season {}", season.season_number)
    } else {
        season.title.clone()
    };
    let mut spans = vec![Span::raw(title)];
    if season.number_of_episodes > 0 {
        spans.push(dim(format!("  {} ep", season.number_of_episodes)));
    }
    ListItem::new(Line::from(spans))
}

fn episode_row(episode: &SeasonEpisode) -> ListItem<'static> {
    let number = if episode.episode.is_empty() {
        episode.episode_number.to_string()
    } else {
        episode.episode.clone()
    };
    let mut spans = vec![accent(format!("E{number:<3}"))];
    if episode.duration_ms > 0 {
        spans.push(dim(format!("{:>6}  ", duration(episode.duration_ms))));
    }
    spans.push(Span::raw(episode.title.clone()));
    ListItem::new(Line::from(spans))
}

fn header(app: &App) -> Paragraph<'static> {
    let left = match &app.editing {
        Some(query) => Line::from(vec![
            Span::styled("Search: ", Style::new().fg(ACCENT)),
            Span::raw(query.clone()),
            Span::styled("▏", Style::new().fg(ACCENT)),
        ]),
        None => Line::from(vec![
            Span::styled(
                app.listing.label(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            dim(format!("   {} series", app.series.items.len())),
        ]),
    };
    let right = Line::from(vec![
        dim("audio "),
        accent(language_name(&app.audio()).to_owned()),
        dim("  subs "),
        accent(language_name(&app.subs()).to_owned()),
        dim("  video "),
        accent(app.options.video_quality.clone()),
        Span::raw(" "),
    ])
    .right_aligned();
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::DarkGray))
        .title(Span::styled(
            " Crunchyroll ",
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .title_top(right);
    Paragraph::new(left).block(block)
}

/// The panel under the columns: everything about the item the cursor is on that does
/// not fit on its one line.
fn details(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match app.focus {
        Focus::Series | Focus::Seasons => {
            let Some(series) = app.series.selected() else {
                return lines;
            };
            let metadata = &series.series_metadata;
            lines.push(Line::from(Span::styled(
                series.title.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            )));
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
            lines.push(Line::from(dim(facts.join(" · "))));
            lines.push(Line::from(series.description.clone()));
        }
        Focus::Episodes => {
            let Some(episode) = app.episodes.selected() else {
                return lines;
            };
            lines.push(Line::from(Span::styled(
                episode.title.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            )));
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
            lines.push(Line::from(dim(facts.join(" · "))));
            lines.push(Line::from(episode.description.clone()));
        }
    }
    lines
}

fn help_overlay(frame: &mut Frame, area: Rect) {
    let keys = [
        ("↑ ↓ / j k", "move the cursor"),
        ("⏎ / → / l", "open the selection, and play an episode"),
        ("← / h / esc", "go back a column, and leave a search"),
        ("tab", "cycle the columns"),
        ("/", "search the catalogue"),
        ("o", "change the browse order"),
        ("p / P", "play the episode / the rest of the season"),
        ("d / D", "download the episode / the whole season"),
        ("a / s / v", "cycle audio, subtitles, video quality"),
        ("r", "reload the current column"),
        ("g / G", "jump to the first or last item"),
        ("q", "quit"),
    ];
    let width = 62.min(area.width.saturating_sub(4));
    let height = (keys.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let lines: Vec<Line> = keys
        .iter()
        .map(|(key, what)| Line::from(vec![accent(format!(" {key:<13}")), Span::raw(*what)]))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(ACCENT))
                .title(Span::styled(
                    " Keys ",
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
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

    let tick = app.tick;
    let focus = app.focus;

    let items: Vec<ListItem> = if app.series.items.is_empty() {
        placeholder(
            app.series.loading,
            app.series.error.as_ref(),
            "Nothing here.",
            tick,
        )
    } else {
        app.series.items.iter().map(series_row).collect()
    };
    let focused = focus == Focus::Series;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block("Series", focused))
            .highlight_style(highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        left,
        &mut app.series.state,
    );

    let items: Vec<ListItem> = if app.seasons.items.is_empty() {
        placeholder(
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
            .map(|season| season_row(season, series_title))
            .collect()
    };
    let focused = focus == Focus::Seasons;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block("Seasons", focused))
            .highlight_style(highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        middle,
        &mut app.seasons.state,
    );

    let items: Vec<ListItem> = if app.episodes.items.is_empty() {
        placeholder(
            app.episodes.loading,
            app.episodes.error.as_ref(),
            "Pick a season.",
            tick,
        )
    } else {
        app.episodes.items.iter().map(episode_row).collect()
    };
    let focused = focus == Focus::Episodes;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block("Episodes", focused))
            .highlight_style(highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        right,
        &mut app.episodes.state,
    );

    if details_height > 0 {
        frame.render_widget(
            Paragraph::new(details(app))
                .wrap(Wrap { trim: true })
                .block(
                    Block::bordered()
                        .border_style(Style::new().fg(Color::DarkGray))
                        .title(dim(" Details ")),
                ),
            bottom,
        );
    }

    let line = match &app.notice {
        Some(notice) if notice.error => Line::from(Span::styled(
            format!(" {}", notice.text),
            Style::new().fg(Color::LightRed),
        )),
        Some(notice) => Line::from(Span::raw(format!(" {}", notice.text))),
        None => Line::from(dim(" Ready.")),
    };
    frame.render_widget(Paragraph::new(line), status);

    frame.render_widget(
        Paragraph::new(Line::from(dim(
            " ↑↓ move   ⏎ open/play   ← back   / search   d download   a audio   s subs   ? keys   q quit",
        ))),
        keys,
    );

    if app.show_help {
        help_overlay(frame, area);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::download::DownloadOptions;
    use crate::model::{CatalogItem, Season, SeasonEpisode, SeriesMetadata};
    use crate::tui::app::App;
    use crate::tui::worker::Worker;

    use super::{draw, duration};

    #[test]
    fn formats_a_running_time() {
        assert_eq!(duration(1_461_000), "24:21");
        assert_eq!(duration(3_723_000), "1:02:03");
        assert_eq!(duration(9_000), "0:09");
    }

    fn app() -> App {
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
            ..Season::default()
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

    fn rendered(width: u16, height: u16, app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        terminal
            .backend()
            .buffer()
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
    }
}
