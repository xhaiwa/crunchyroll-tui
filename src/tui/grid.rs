//! The catalogue as a wall of covers.
//!
//! The columns are a good way to walk down into a season and a poor way to choose one:
//! a title in a list says nothing about a series that the poster would not say faster.
//! So the catalogue can also be drawn as tiles - poster on top, title and a word about
//! what it is underneath - as many across as the terminal has room for.
//!
//! It is the same list as the Series column, not a copy of it. The cursor is the
//! column's cursor, the narrowing is the column's narrowing, and the next page is asked
//! for the same way, so switching between the two views never loses a place. What is
//! kept here is only what a wall has and a list does not: how many tiles fit across, and
//! which row of them is at the top.
//!
//! The arithmetic is pure functions of rectangles and whole numbers, like `mouse.rs`, so
//! that it can be tested without a terminal; the drawing is at the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use serde::Deserialize;

use crate::model::{CatalogItem, single_name};

use super::app::{App, Focus};
use super::art::Gallery;
use super::theme::Theme;

/// How the catalogue is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum View {
    /// Series, seasons and episodes side by side, with the poster of the selected series
    /// beside them. The default, because it is the view everything can be done from.
    #[default]
    Columns,
    /// The catalogue as a wall of posters. Opening one goes to the columns with that
    /// series open, and backing out of its seasons comes back here.
    Covers,
}

/// The narrowest a tile is made. Below this a poster is a smudge and a title is three
/// letters and an ellipsis.
pub const TILE_MIN: u16 = 16;

/// The widest a tile is made. Wider than this and a screen holds three posters, which is
/// a list with pictures rather than a wall; the room left over goes to the margins.
pub const TILE_MAX: u16 = 22;

/// The columns between two tiles. The borders already keep them apart; one more column
/// keeps the selected tile's border from touching its neighbour's.
const GAP: u16 = 1;

/// The rows under the picture: the title, and a word about what it is.
const CAPTION: u16 = 2;

/// What the last frame's wall looked like, for the keys and the pointer to count with.
///
/// Written by [`draw`] and read by the next keypress, the way a list widget writes its
/// offset into its state: `down` has to know how many tiles make a row, and a wall that
/// has never been drawn behaves as a single column rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// How many tiles across.
    pub columns: usize,
    /// How many rows of tiles fit on the screen at once.
    pub rows: usize,
    /// The row of tiles at the top of the screen.
    pub offset: usize,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            columns: 1,
            rows: 1,
            offset: 0,
        }
    }
}

impl Shape {
    /// How many tiles across, and never none: a keypress arriving before the first frame
    /// has anything to divide by.
    pub fn columns(&self) -> usize {
        self.columns.max(1)
    }
}

/// How many tiles fit across `width`, and how wide each one is.
///
/// As many as fit at no wider than [`TILE_MAX`], which gives each the most room without
/// wasting any - unless that would squeeze them below [`TILE_MIN`], in which case one
/// fewer. A terminal too narrow for even one tile of the minimum still gets one tile, as
/// wide as it has: a wall with nothing on it would hide the catalogue altogether.
pub fn fit(width: u16) -> (usize, u16) {
    if width == 0 {
        return (0, 0);
    }
    let across = |columns: u16| (width - GAP * (columns - 1)) / columns;
    let mut columns = (width + GAP).div_ceil(TILE_MAX + GAP).max(1);
    if columns > 1 && across(columns) < TILE_MIN {
        columns -= 1;
    }
    (usize::from(columns), across(columns).min(TILE_MAX))
}

/// How tall a tile `width` columns wide has to be for a two-by-three poster, its caption
/// and its border.
///
/// `cell` is the terminal's character size in pixels, which is the only thing that
/// turns a number of columns into the number of rows the same picture needs. A terminal
/// that would not say is taken to have cells twice as tall as they are wide, which is
/// what most fonts are.
pub fn tile_height(width: u16, cell: Size) -> u16 {
    let (cell_width, cell_height) = if cell.width == 0 || cell.height == 0 {
        (1, 2)
    } else {
        (u32::from(cell.width), u32::from(cell.height))
    };
    let inside = u32::from(width.saturating_sub(2)) * cell_width;
    let picture = (inside * 3).div_ceil(2 * cell_height);
    u16::try_from(picture)
        .unwrap_or(u16::MAX)
        .saturating_add(CAPTION + 2)
}

/// The shortest a tile is squeezed to so that another row fits: the border, the caption
/// and three rows of picture, below which the picture is not worth drawing. The frame
/// reads it as well, to leave out the details card where keeping it would leave the
/// wall shorter than this.
pub const TILE_SHORTEST: u16 = CAPTION + 2 + 3;

/// How many rows of tiles `room` lines hold, and how tall each one is, for tiles that
/// would be `natural` lines tall given the space.
///
/// Rounded rather than cut down to what fits whole. A screen with room for a row and
/// two thirds would otherwise show one row and leave the rest of itself blank, which on
/// an ordinary terminal is the difference between six covers and twelve; squeezing the
/// tiles a little instead costs each poster a column either side, since a picture keeps
/// its own shape and sits in the middle of its tile. A tile is never stretched past its
/// natural height, though - a screen with room for a row and a quarter shows one row -
/// and never squeezed below [`TILE_SHORTEST`] to make room for another row. Room for
/// less than one such tile still gets one, as tall as the room is, which may leave no
/// picture in it at all: better a caption than an empty wall. The frame sees to it that
/// this only happens on a terminal too short for anything else.
pub fn stack(room: u16, natural: u16) -> (usize, u16) {
    if room == 0 || natural == 0 {
        return (0, 0);
    }
    let most = (room / TILE_SHORTEST).max(1);
    let rows = ((room + natural / 2) / natural).clamp(1, most);
    (usize::from(rows), natural.min(room / rows))
}

/// The row of tiles to put at the top of the screen so that the cursor's row is on it.
///
/// The row that was there last time wherever that still shows the cursor, so the wall
/// only moves when the cursor walks off its edge - the way a list scrolls, rather than
/// recentring on every keypress. It never leaves empty rows at the bottom that a row
/// further up could have filled, which is what a wall that has just grown taller, or a
/// list that has just been narrowed, would otherwise do.
pub fn scroll(offset: usize, cursor: usize, visible: usize, total: usize) -> usize {
    let visible = visible.max(1);
    let offset = offset.min(total.saturating_sub(visible));
    if cursor < offset {
        cursor
    } else if cursor >= offset + visible {
        cursor + 1 - visible
    } else {
        offset
    }
}

/// Where everything on the wall goes, for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wall {
    pub columns: usize,
    /// How many rows of tiles fit.
    pub rows: usize,
    pub tile: Size,
    /// The columns left over either side, so the wall sits in the middle of its box
    /// rather than against its left edge.
    margin: u16,
}

impl Wall {
    /// The wall that fits inside `inner`.
    pub fn of(inner: Rect, cell: Size) -> Self {
        let (columns, width) = fit(inner.width);
        let (rows, height) = stack(inner.height, tile_height(width, cell));
        let across = u16::try_from(columns).unwrap_or(u16::MAX);
        let used = across * width + GAP * across.saturating_sub(1);
        Self {
            columns,
            rows,
            tile: Size::new(width, height),
            margin: inner.width.saturating_sub(used) / 2,
        }
    }

    /// The box of the tile `row` rows down from the top of the screen and `column`
    /// across, kept inside `inner` whatever it is asked for.
    pub fn tile(&self, inner: Rect, row: usize, column: usize) -> Rect {
        let row = u16::try_from(row).unwrap_or(u16::MAX);
        let column = u16::try_from(column).unwrap_or(u16::MAX);
        Rect {
            x: inner
                .x
                .saturating_add(self.margin)
                .saturating_add(column.saturating_mul(self.tile.width + GAP)),
            y: inner.y.saturating_add(row.saturating_mul(self.tile.height)),
            width: self.tile.width,
            height: self.tile.height,
        }
        .intersection(inner)
    }
}

/// Up to two letters standing in for a poster that is not there: the first letter or
/// digit of each of the first two words, `Frieren: Beyond Journey's End` as `FB`.
pub fn initials(title: &str) -> String {
    let letters: String = title
        .split_whitespace()
        .filter_map(|word| word.chars().find(|letter| letter.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "?".to_owned()
    } else {
        letters
    }
}

/// How many columns `text` takes on the screen, which is not how many characters it is:
/// `日本語` is three characters and six columns.
fn cells(text: &str) -> usize {
    Span::raw(text).width()
}

/// `text` cut down to `width` columns, with an ellipsis where it was cut.
pub fn truncate(text: &str, width: usize) -> String {
    if cells(text) <= width {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut used = 0;
    for letter in text.chars() {
        let wide = cells(letter.encode_utf8(&mut [0; 4]));
        // One column is kept back for the ellipsis.
        if used + wide + 1 > width {
            break;
        }
        kept.push(letter);
        used += wide;
    }
    // Cutting after a space leaves `Attack on …`, which reads as a gap and then a mark.
    let mut kept = kept.trim_end().to_owned();
    if width > 0 {
        kept.push('…');
    }
    kept
}

/// The words under a title: what it is when it is not a series, how many seasons when
/// there are several, whether it is simulcasting and whether it is dubbed. The same
/// facts the Series column puts beside a title, for the same reason - a wall that
/// mixes films in with series has to say which is which.
///
/// Each word comes with whether it stands out, the way the Series column marks them:
/// what kind of thing a tile is where it is not a series, and that it is simulcasting,
/// are drawn in the accent, and the counts in dim.
pub fn tags(item: &CatalogItem) -> Vec<(bool, String)> {
    let metadata = &item.series_metadata;
    let mut tags = Vec::new();
    if let Some(word) = single_name(&item.kind) {
        tags.push((true, word.to_lowercase()));
    } else if metadata.season_count > 1 {
        tags.push((false, format!("{} seasons", metadata.season_count)));
    } else if metadata.episode_count > 0 {
        tags.push((false, format!("{} ep", metadata.episode_count)));
    }
    if metadata.is_simulcast {
        tags.push((true, "simulcast".to_owned()));
    }
    if metadata.is_dubbed || item.movie_listing_metadata.is_dubbed {
        tags.push((false, "dub".to_owned()));
    }
    tags
}

/// What the words under a title are held apart by: the dot the rest of the interface
/// puts between facts.
const DOT: &str = " \u{b7} ";

/// The words under a title as spans, cut to `width` columns. A word that does not fit
/// whole is cut with an ellipsis and ends the line, so a narrow tile still says what
/// the tile is before it runs out of room.
fn tag_line(theme: &Theme, tags: &[(bool, String)], width: usize) -> Vec<Span<'static>> {
    let mut spans = vec![theme.dim(" ")];
    let mut used = 0;
    for (index, (loud, word)) in tags.iter().enumerate() {
        if index > 0 {
            if used + cells(DOT) >= width {
                break;
            }
            spans.push(theme.dim(DOT));
            used += cells(DOT);
        }
        let word = truncate(word, width.saturating_sub(used));
        if word.is_empty() {
            break;
        }
        used += cells(&word);
        let cut = word.ends_with('\u{2026}');
        spans.push(if *loud {
            theme.accent(word)
        } else {
            theme.dim(word)
        });
        if cut {
            break;
        }
    }
    spans
}

/// What one tile needs, copied out of the catalogue so that the gallery can be borrowed
/// to draw it.
struct Tile {
    title: String,
    tags: Vec<(bool, String)>,
    poster: Option<String>,
}

/// Draws the catalogue as a wall of covers inside `block`, and gives back each tile it
/// drew with the row of the catalogue it stands for - which is what a click is aimed at.
///
/// The block comes from the caller so that the wall wears the same frame and the same
/// title as the Series column it stands in for.
pub fn draw(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    block: Block<'static>,
) -> Vec<(usize, Rect)> {
    let theme = app.theme;
    let focused = app.focus == Focus::Series;
    let count = app.series.rows();
    let cursor = app.series.state.selected();
    // Where the cursor is in the whole list, since a wall shows a screenful and says
    // nothing else about how far down it is. The same corner says when the next page
    // is on its way, which is otherwise a wait with nothing on screen explaining it.
    let position = match (cursor, app.series.loading) {
        (Some(cursor), false) => format!(" {}/{count} ", cursor + 1),
        (Some(cursor), true) => format!(" {}/{count} · loading more ", cursor + 1),
        (None, _) => String::new(),
    };
    let block = block.title_bottom(Line::from(theme.dim(position)).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cell = app.art.cell();
    let wall = Wall::of(inner, cell);
    if wall.columns == 0 || wall.rows == 0 {
        return Vec::new();
    }
    let total = count.div_ceil(wall.columns);
    let offset = scroll(
        app.grid.offset,
        cursor.unwrap_or(0) / wall.columns,
        wall.rows,
        total,
    );
    app.grid = Shape {
        columns: wall.columns,
        rows: wall.rows,
        offset,
    };

    // The poster is asked for at the size of the inside of the tile, which is what
    // keeps a wall of forty covers from pulling forty full-size posters over the wire.
    let pixels = u32::from(wall.tile.width.saturating_sub(2)) * u32::from(cell.width);
    let first = offset * wall.columns;
    let last = ((offset + wall.rows) * wall.columns).min(count);
    let tiles: Vec<Tile> = app
        .series
        .shown()
        .get(first..last)
        .unwrap_or_default()
        .iter()
        .map(|item| Tile {
            title: item.title.clone(),
            tags: tags(item),
            poster: item.images.poster(pixels).map(str::to_owned),
        })
        .collect();

    let mut drawn = Vec::with_capacity(tiles.len());
    for (shown, tile) in tiles.iter().enumerate() {
        let index = first + shown;
        let area = wall.tile(inner, shown / wall.columns, shown % wall.columns);
        if area.is_empty() {
            continue;
        }
        let selected = cursor == Some(index);
        draw_tile(frame, &mut app.art, &theme, area, tile, selected, focused);
        drawn.push((index, area));
    }
    drawn
}

/// One tile: the poster, or something standing in for it, and the caption underneath.
///
/// The selected tile is picked out three ways at once - a heavier border, in the accent,
/// round a title in the cursor row's own colours - because it is the one thing on the
/// wall that has to be found at a glance, and a colour alone is lost on a terminal whose
/// accent is close to its border colour.
fn draw_tile(
    frame: &mut Frame,
    art: &mut Gallery,
    theme: &Theme,
    area: Rect,
    tile: &Tile,
    selected: bool,
    focused: bool,
) {
    // The same rounded card as every other box, in the accent where the cursor is; the
    // one tile the keyboard is on gets the heavier line on top of that.
    let mut block = theme.bordered(selected);
    if selected && focused {
        block = block.border_type(BorderType::Thick);
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [picture, caption] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(CAPTION)]).areas(inner);
    let drawn = tile
        .poster
        .as_deref()
        .is_some_and(|url| art.draw(frame, picture, url));
    if !drawn {
        let waiting = art.enabled() && tile.poster.is_some();
        stand_in(frame, theme, picture, &tile.title, waiting);
    }

    // One column in from the border, the way a row of a list sits one column in from
    // its cursor, and padded out to the width of the tile, so the cursor's colours run
    // edge to edge rather than stopping where the words do.
    let width = usize::from(caption.width).saturating_sub(1);
    let title = truncate(&tile.title, width);
    let padding = " ".repeat(width.saturating_sub(cells(&title)));
    let style = if selected {
        theme.highlight(focused)
    } else {
        Style::new()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD)
    };
    let lines = vec![
        Line::from(Span::styled(format!(" {title}{padding}"), style)),
        Line::from(tag_line(theme, &tile.tags, width)),
    ];
    frame.render_widget(Paragraph::new(lines), caption);
}

/// What a tile shows where its poster is not: a shaded panel the shape of the poster,
/// with the title's initials in the middle of it.
///
/// A tile with a hole in it reads as a bug, and a wall of them - the artwork turned off,
/// or a first screenful still coming over the wire - reads as a broken interface. A
/// panel in the theme's own colours with two letters on it reads as a cover, and the
/// letters are enough to tell two of them apart. The dots under the letters are the
/// picture on its way; a tile without them has none coming.
fn stand_in(frame: &mut Frame, theme: &Theme, area: Rect, title: &str, waiting: bool) {
    let panel = area.inner(Margin::new(1, 0));
    if panel.is_empty() {
        return;
    }
    let shade = Style::new().fg(theme.border);
    let lines: Vec<Line> = (0..panel.height)
        .map(|_| Line::styled("░".repeat(usize::from(panel.width)), shade))
        .collect();
    frame.render_widget(Paragraph::new(lines), panel);

    let middle = panel.y + panel.height.saturating_sub(1) / 2;
    let letters = format!(" {} ", initials(title));
    let label = Rect {
        y: middle,
        height: 1,
        ..panel
    };
    frame.render_widget(
        Paragraph::new(Line::from(theme.title(letters))).centered(),
        label,
    );
    if waiting && middle + 1 < panel.bottom() {
        frame.render_widget(
            Paragraph::new(Line::from(theme.dim(" ··· "))).centered(),
            Rect {
                y: middle + 1,
                ..label
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::{Rect, Size};

    use crate::model::{CatalogItem, MovieListingMetadata, SeriesMetadata};

    use super::{
        Shape, TILE_MAX, TILE_MIN, Wall, fit, initials, scroll, stack, tag_line, tags, tile_height,
        truncate,
    };

    #[test]
    fn as_many_tiles_fit_across_as_the_width_allows() {
        // The widest tiles that still fill the row: five of nineteen and four gaps.
        assert_eq!(fit(100), (5, 19));
        assert_eq!(fit(118), (6, 18));
        // Exactly two of the widest.
        assert_eq!(fit(45), (2, 22));
        // Three would be fourteen wide, which is under the minimum, so two it is - and
        // the tiles stop at the maximum rather than stretching to fill the row.
        assert_eq!(fit(47), (2, TILE_MAX));
        // One tile on a narrow terminal, however narrow.
        assert_eq!(fit(TILE_MIN), (1, TILE_MIN));
        assert_eq!(fit(10), (1, 10));
        assert_eq!(fit(0), (0, 0));
        for width in 1..400 {
            let (columns, tile) = fit(width);
            assert!(columns >= 1 && tile <= TILE_MAX, "{width}");
            let used = columns as u16 * tile + (columns as u16 - 1);
            assert!(used <= width, "{width} columns hold {used}");
            if width >= TILE_MIN {
                assert!(tile >= TILE_MIN, "{width} makes tiles {tile} wide");
            }
        }
    }

    #[test]
    fn a_tile_is_as_tall_as_a_poster_its_width_needs() {
        // Sixteen columns inside the border on a ten-by-twenty font are 160 pixels
        // across, so 240 down, so twelve rows - plus the caption and the border.
        assert_eq!(tile_height(18, Size::new(10, 20)), 16);
        // A squarer font needs more rows for the same picture.
        assert_eq!(tile_height(18, Size::new(10, 10)), 28);
        // A terminal that would not say how big its font is is taken to be two to one.
        assert_eq!(tile_height(18, Size::new(0, 0)), 16);
    }

    #[test]
    fn the_rows_of_tiles_fill_the_screen() {
        // A row and two thirds of room is two rows, a little shorter than they would be.
        assert_eq!(stack(26, 16), (2, 13));
        // A row and a quarter is one, at its own height rather than stretched.
        assert_eq!(stack(20, 16), (1, 16));
        assert_eq!(stack(60, 16), (4, 15));
        // A screen shorter than one tile gets one tile as tall as it has.
        assert_eq!(stack(9, 16), (1, 9));
        assert_eq!(stack(3, 16), (1, 3));
        // Never squeezed below a picture worth drawing to fit one more row in.
        assert_eq!(stack(13, 7), (1, 7));
        assert_eq!(stack(0, 16), (0, 0));
    }

    #[test]
    fn the_wall_scrolls_only_when_the_cursor_walks_off_it() {
        // Three rows on screen, starting at the top: moving inside them moves nothing.
        assert_eq!(scroll(0, 0, 3, 10), 0);
        assert_eq!(scroll(0, 2, 3, 10), 0);
        // Walking off the bottom brings one row in.
        assert_eq!(scroll(0, 3, 3, 10), 1);
        // Jumping to the end brings the last screenful in.
        assert_eq!(scroll(1, 9, 3, 10), 7);
        // Walking off the top brings the cursor's row back to the top.
        assert_eq!(scroll(7, 4, 3, 10), 4);
        assert_eq!(scroll(7, 8, 3, 10), 7, "still on screen, so nothing moves");
        // A list that got shorter under a wall scrolled down does not leave empty rows.
        assert_eq!(scroll(7, 1, 3, 2), 0);
        assert_eq!(scroll(5, 3, 3, 4), 1);
        // No room for a whole row still shows the cursor's.
        assert_eq!(scroll(0, 5, 0, 10), 5);
    }

    #[test]
    fn tiles_are_laid_out_in_rows_in_the_middle_of_the_box() {
        let inner = Rect::new(1, 4, 100, 34);
        let wall = Wall::of(inner, Size::new(10, 20));
        assert_eq!(wall.columns, 5);
        assert_eq!(wall.tile, Size::new(19, 17));
        assert_eq!(wall.rows, 2);
        // Five tiles of nineteen and four gaps take ninety-nine columns, leaving one,
        // which is not enough to split and so goes to the right.
        assert_eq!(wall.tile(inner, 0, 0), Rect::new(1, 4, 19, 17));
        assert_eq!(wall.tile(inner, 0, 1), Rect::new(21, 4, 19, 17));
        assert_eq!(wall.tile(inner, 1, 4), Rect::new(81, 21, 19, 17));
        // A tile asked for past the edge of the box is cut to it rather than drawn
        // outside it.
        assert!(wall.tile(inner, 2, 0).is_empty());

        // A box wider than the widest tiles leaves the same margin either side.
        let inner = Rect::new(0, 0, 49, 20);
        let wall = Wall::of(inner, Size::new(10, 20));
        assert_eq!(wall.columns, 2);
        assert_eq!(wall.tile.width, TILE_MAX);
        assert_eq!(wall.tile(inner, 0, 0).x, 2);
        assert_eq!(wall.tile(inner, 0, 1).right(), 47);
    }

    #[test]
    fn a_wall_that_was_never_drawn_is_one_column() {
        assert_eq!(Shape::default().columns(), 1);
        let shape = Shape {
            columns: 0,
            rows: 0,
            offset: 0,
        };
        assert_eq!(shape.columns(), 1, "nothing to divide by is still one");
    }

    #[test]
    fn a_missing_poster_is_stood_in_for_by_initials() {
        assert_eq!(initials("Frieren: Beyond Journey's End"), "FB");
        assert_eq!(initials("one piece"), "OP");
        assert_eq!(initials("Bleach"), "B");
        assert_eq!(initials("[Oshi no Ko]"), "ON");
        assert_eq!(initials("86 EIGHTY-SIX"), "8E");
        assert_eq!(initials("  "), "?");
    }

    #[test]
    fn a_long_title_is_cut_with_an_ellipsis() {
        assert_eq!(truncate("Frieren", 10), "Frieren");
        assert_eq!(truncate("Frieren", 7), "Frieren");
        assert_eq!(truncate("Frieren", 6), "Frier…");
        // A cut after a space does not leave the space in front of the ellipsis.
        assert_eq!(truncate("Attack on Titan", 11), "Attack on…");
        // Wide characters are counted as the two columns they take.
        assert_eq!(truncate("日本語のタイトル", 7), "日本語…");
        assert_eq!(truncate("Frieren", 1), "…");
        assert_eq!(truncate("Frieren", 0), "");
    }

    /// The words under a title as they read, held apart by a dot.
    fn tag(item: &CatalogItem) -> String {
        tags(item)
            .into_iter()
            .map(|(_, word)| word)
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    }

    #[test]
    fn the_tag_says_what_a_tile_is() {
        let series = |seasons, episodes, simulcast, dubbed| CatalogItem {
            kind: "series".to_owned(),
            series_metadata: SeriesMetadata {
                season_count: seasons,
                episode_count: episodes,
                is_simulcast: simulcast,
                is_dubbed: dubbed,
                ..SeriesMetadata::default()
            },
            ..CatalogItem::default()
        };
        assert_eq!(tag(&series(3, 36, false, false)), "3 seasons");
        assert_eq!(tag(&series(1, 12, true, true)), "12 ep · simulcast · dub");
        assert_eq!(tag(&series(0, 0, false, false)), "");
        let film = CatalogItem {
            kind: "movie_listing".to_owned(),
            movie_listing_metadata: MovieListingMetadata {
                is_dubbed: true,
                ..MovieListingMetadata::default()
            },
            ..CatalogItem::default()
        };
        assert_eq!(tag(&film), "film · dub");
    }

    /// The words under a title are cut where the tile ends, and a word that is cut ends
    /// the line rather than being followed by a dot and half of the next one.
    #[test]
    fn the_tag_line_is_cut_to_the_tile() {
        let theme = crate::tui::theme::Theme::default();
        let words = vec![
            (false, "12 ep".to_owned()),
            (true, "simulcast".to_owned()),
            (false, "dub".to_owned()),
        ];
        let read = |width| {
            tag_line(&theme, &words, width)
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert_eq!(read(40), " 12 ep · simulcast · dub");
        assert_eq!(read(12), " 12 ep · sim…");
        assert_eq!(read(8), " 12 ep");
        assert_eq!(read(0), " ");
    }
}

/// The wall as it is used: drawn by `ui::draw` into a test terminal, driven by the keys
/// and the pointer the way the columns' own tests drive them.
#[cfg(test)]
mod drawn {
    use std::sync::{Arc, Mutex};

    use image::{DynamicImage, Rgb, RgbImage};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;

    use crate::api::Page;
    use crate::download::DownloadOptions;
    use crate::model::{Artwork, CatalogItem, Images, SeriesMetadata};
    use crate::tui::app::{Action, App, Focus};
    use crate::tui::art::Gallery;
    use crate::tui::keys::Bindings;
    use crate::tui::theme::Theme;
    use crate::tui::ui::draw;
    use crate::tui::worker::{Request, Response, Worker};

    use super::View;

    const POSTER: &str = "https://img.example/poster.jpg";

    fn app(art: Gallery) -> App {
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
            art,
        )
    }

    fn item(index: usize) -> CatalogItem {
        CatalogItem {
            id: format!("GY{index}"),
            kind: "series".to_owned(),
            title: format!("Series {index}"),
            series_metadata: SeriesMetadata {
                season_count: 2,
                ..SeriesMetadata::default()
            },
            images: Images {
                poster_tall: vec![vec![Artwork {
                    width: 360,
                    source: POSTER.to_owned(),
                }]],
                ..Images::default()
            },
            ..CatalogItem::default()
        }
    }

    /// A catalogue of `count` series, with the wall up and the request the interface
    /// opened with taken off the worker's hands.
    fn wall(count: usize, art: Gallery) -> App {
        let mut app = app(art);
        app.series.set((0..count).map(item).collect());
        app.sent();
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.view, View::Covers);
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::from(code))
    }

    fn buffer(width: u16, height: u16, app: &mut App) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        terminal.backend().buffer().clone()
    }

    fn screen(buffer: &Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn click(app: &mut App, area: Rect) -> Action {
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + area.width / 2,
            row: area.y + area.height / 2,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn tile(app: &App, row: usize) -> Rect {
        app.regions
            .tiles
            .iter()
            .find(|(drawn, _)| *drawn == row)
            .map_or_else(|| panic!("no cover drawn for row {row}"), |(_, area)| *area)
    }

    fn seasons_asked(app: &App) -> Vec<String> {
        app.sent()
            .into_iter()
            .filter_map(|request| match request {
                Request::Seasons { series_id, .. } => Some(series_id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_wall_shows_a_cover_for_each_series_in_place_of_the_columns() {
        let mut app = wall(8, Gallery::detached(false));
        let drawn = screen(&buffer(120, 40, &mut app));
        for index in 0..8 {
            assert!(
                drawn.contains(&format!("Series {index}")),
                "no cover for Series {index}"
            );
        }
        assert!(drawn.contains("2 seasons"), "no word on what a cover is");
        assert!(
            !drawn.contains("Episodes"),
            "the columns are still drawn beside the wall"
        );
        // The artwork is off, so every cover stands in with its initials rather than
        // leaving a hole.
        assert!(
            drawn.contains(" S0 ") && drawn.contains('░'),
            "a cover is a hole"
        );
        assert_eq!(app.grid.columns, 6, "118 columns hold six covers");
        assert_eq!(app.regions.tiles.len(), 8);
        assert!(app.regions.seasons.is_empty() && app.regions.episodes.is_empty());

        // And `t` again puts the columns back, on the same series.
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.view, View::Columns);
        let drawn = screen(&buffer(120, 40, &mut app));
        assert!(drawn.contains("Seasons") && drawn.contains("Episodes"));
        assert_eq!(app.series.state.selected(), Some(1));
    }

    #[test]
    fn the_posters_are_drawn_on_the_covers() {
        let mut gallery = Gallery::detached(true);
        gallery.preload(
            POSTER,
            DynamicImage::ImageRgb8(RgbImage::from_fn(240, 360, |x, y| {
                Rgb([(x % 256) as u8, (y % 256) as u8, 128])
            })),
        );
        let mut app = wall(3, gallery);
        let buffer = buffer(120, 40, &mut app);
        let area = tile(&app, 1);
        let pixels = (area.top()..area.bottom())
            .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
            .filter(|(x, y)| matches!(buffer[(*x, *y)].symbol(), "\u{2580}" | "\u{2584}"))
            .count();
        assert!(pixels > 50, "the cover was left without its poster");
        assert!(
            !screen(&buffer).contains("Poster"),
            "a poster column beside a wall of them"
        );
    }

    #[test]
    fn up_and_down_move_a_row_of_covers_and_left_and_right_one() {
        let mut app = wall(20, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);
        let across = app.grid.columns;
        assert_eq!(across, 6);

        press(&mut app, KeyCode::Down);
        assert_eq!(app.series.state.selected(), Some(across));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.series.state.selected(), Some(across + 2));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.series.state.selected(), Some(across + 1));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.series.state.selected(), Some(1));
        press(&mut app, KeyCode::Up);
        assert_eq!(
            app.series.state.selected(),
            Some(1),
            "up from the first row goes nowhere rather than to the first cover"
        );

        // The last row is short - twenty covers are three rows of six and one of two -
        // so down onto it from a column it does not reach lands on its last cover.
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.series.state.selected(), Some(4));
        for _ in 0..5 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(app.series.state.selected(), Some(19));
        press(&mut app, KeyCode::Home);
        assert_eq!(app.series.state.selected(), Some(0));
        press(&mut app, KeyCode::PageDown);
        let screenful = across * app.grid.rows;
        assert_eq!(app.series.state.selected(), Some(screenful));
        assert_eq!(app.focus, Focus::Series, "moving never left the wall");
    }

    #[test]
    fn the_wall_scrolls_to_keep_the_chosen_cover_on_screen() {
        let mut app = wall(60, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);
        assert_eq!(app.grid.offset, 0);
        press(&mut app, KeyCode::End);
        let drawn = screen(&buffer(120, 40, &mut app));
        assert!(
            drawn.contains("Series 59"),
            "the last cover is off the screen"
        );
        assert!(!drawn.contains("Series 0 "), "the first row is still up");
        assert!(app.grid.offset > 0);
        assert!(drawn.contains("60/60"), "the wall does not say where it is");
    }

    #[test]
    fn opening_a_cover_opens_the_series_into_the_columns_and_back_returns() {
        let mut app = wall(8, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.view, View::Columns, "the wall is still up");
        assert_eq!(app.focus, Focus::Seasons);
        assert_eq!(seasons_asked(&app), ["GY1"]);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Covers, "back did not come back to the wall");
        assert_eq!(app.focus, Focus::Series);
        assert_eq!(app.series.state.selected(), Some(1));

        // Arrived at by `t` rather than by a cover, the columns keep their own back.
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Columns);
    }

    /// A cover opened and then left for the Series column is the columns being used on
    /// their own terms: `back` out of what is opened next stays in them, rather than
    /// jumping to a wall the user left some time ago.
    #[test]
    fn back_only_returns_to_the_wall_from_the_cover_that_was_opened() {
        // Opened another series from the Series column.
        let mut app = wall(8, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);
        press(&mut app, KeyCode::Enter);
        for _ in 0..3 {
            press(&mut app, KeyCode::Tab);
        }
        assert_eq!(app.focus, Focus::Series);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.view, app.focus), (View::Columns, Focus::Series));

        // Tabbed round to the Series column and back to the seasons of the same one.
        let mut app = wall(8, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);
        press(&mut app, KeyCode::Enter);
        for _ in 0..4 {
            press(&mut app, KeyCode::Tab);
        }
        assert_eq!(app.focus, Focus::Seasons);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Columns);

        // A new list asked for in the columns.
        let mut app = wall(8, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('o'));
        app.series.set((0..8).map(item).collect());
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Columns);
    }

    #[test]
    fn going_to_the_wall_from_the_episodes_takes_the_keyboard_to_the_catalogue() {
        let mut app = wall(4, Gallery::detached(false));
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Episodes);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.view, View::Covers);
        assert_eq!(app.focus, Focus::Series);
    }

    #[test]
    fn a_click_chooses_a_cover_and_a_second_click_opens_it() {
        let mut app = wall(8, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);

        let area = tile(&app, 3);
        assert!(matches!(click(&mut app, area), Action::None));
        assert_eq!(app.series.state.selected(), Some(3));
        assert_eq!(app.view, View::Covers, "the first click opened the cover");
        assert!(seasons_asked(&app).is_empty());

        let _ = buffer(120, 40, &mut app);
        let area = tile(&app, 3);
        click(&mut app, area);
        assert_eq!(app.view, View::Columns);
        assert_eq!(seasons_asked(&app), ["GY3"]);
    }

    #[test]
    fn the_wheel_scrolls_the_wall_a_row_at_a_time() {
        let mut app = wall(30, Gallery::detached(false));
        let _ = buffer(120, 40, &mut app);
        let area = tile(&app, 0);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: area.x + 1,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.series.state.selected(), Some(app.grid.columns));
    }

    /// The wall wears the look of the rest of the interface: rounded cards in the
    /// border colour, the chosen one in the accent and, while the keyboard is on the
    /// wall, in a heavier line as well.
    #[test]
    fn the_chosen_cover_is_framed_in_the_accent() {
        let mut app = wall(8, Gallery::detached(false));
        let buffer = buffer(120, 40, &mut app);
        let corner = |row| {
            let area = tile(&app, row);
            buffer[(area.x, area.y)].clone()
        };
        let chosen = corner(0);
        assert_eq!(
            chosen.symbol(),
            "\u{250f}",
            "the chosen cover is not in a heavy line"
        );
        assert_eq!(chosen.fg, app.theme.accent);
        let other = corner(1);
        assert_eq!(
            other.symbol(),
            "\u{256d}",
            "the other covers are not rounded"
        );
        assert_eq!(other.fg, app.theme.border);
    }

    /// A film on the wall says so in the accent, as it does in the Series column, and
    /// a count stays dim.
    #[test]
    fn a_cover_names_a_film_in_the_accent() {
        let mut app = app(Gallery::detached(false));
        let mut film = item(0);
        film.kind = "movie_listing".to_owned();
        app.series.set(vec![film, item(1)]);
        app.sent();
        press(&mut app, KeyCode::Char('t'));
        let buffer = buffer(120, 40, &mut app);
        let find = |word: &str| {
            let first = word.chars().next().map(String::from).unwrap_or_default();
            (0..buffer.area.height)
                .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
                .find(|(x, y)| {
                    buffer[(*x, *y)].symbol() == first
                        && (0..word.chars().count()).all(|at| {
                            let x = *x + u16::try_from(at).unwrap_or(0);
                            x < buffer.area.width
                                && Some(buffer[(x, *y)].symbol())
                                    == word.chars().nth(at).map(String::from).as_deref()
                        })
                })
                .unwrap_or_else(|| panic!("{word:?} is not on the wall"))
        };
        assert_eq!(buffer[find("film")].fg, app.theme.accent);
        assert_eq!(buffer[find("2 seasons")].fg, app.theme.dim);
    }

    /// The footer offers the other view by name, so the wall can be found without the
    /// help popup, and says how to get back once it is up.
    #[test]
    fn the_footer_offers_the_other_view() {
        let mut app = app(Gallery::detached(false));
        app.series.set((0..3).map(item).collect());
        assert!(screen(&buffer(120, 40, &mut app)).contains("t covers"));
        press(&mut app, KeyCode::Char('t'));
        let drawn = screen(&buffer(120, 40, &mut app));
        assert!(drawn.contains("t columns") && !drawn.contains("t covers"));
    }

    #[test]
    fn the_last_row_of_covers_asks_for_the_next_page() {
        let mut app = app(Gallery::detached(false));
        press(&mut app, KeyCode::Char('o'));
        let listing = app.listing.clone();
        let filters = app.filters.clone();
        app.accept(Response::Catalog {
            listing: listing.clone(),
            start: 0,
            filters: filters.clone(),
            result: Ok(Page {
                items: (0..14).map(item).collect(),
                total: Some(200),
                next: Some(14),
            }),
        });
        app.sent();
        press(&mut app, KeyCode::Char('t'));
        let _ = buffer(120, 40, &mut app);
        assert_eq!(app.grid.columns, 6);

        // Fourteen covers are two rows of six and a row of two. The second row is not
        // the end yet.
        press(&mut app, KeyCode::Down);
        assert!(app.sent().is_empty(), "asked for more a row early");
        // The first cover of the last row is: down from there goes nowhere.
        press(&mut app, KeyCode::Down);
        assert_eq!(app.series.state.selected(), Some(12));
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing,
                start: 14,
                filters,
            }]
        );
    }
}
