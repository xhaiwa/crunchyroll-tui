//! Where the last frame put everything, and what a pointer landing on it is over.
//!
//! The interface is drawn out of rectangles that only ever existed as locals inside
//! `ui::draw`, so until now nothing could say which column a pointer was on. This keeps
//! a copy of the handful of boxes a pointer can land in, written down at the end of every
//! frame, beside the arithmetic that turns a column and a row into the thing underneath.
//!
//! Nothing here knows what a click *does* - that is `app.rs`, where the keyboard already
//! decides. Everything here is a function of rectangles and whole numbers, which is what
//! lets it be tested without a terminal, an interface, or a mouse.

use std::io::{IsTerminal, Write, stdout};
use std::panic;

use ratatui::layout::{Margin, Position, Rect};

use super::app::Focus;
use super::keys::Command;

/// How many rows one notch of the wheel moves. Three is what a list scrolls by
/// everywhere else: enough that a flick covers ground, few enough that a short list is
/// not crossed in one notch.
pub const WHEEL: isize = 3;

/// Asking the terminal to report the pointer.
///
/// Written out by name rather than through crossterm's `EnableMouseCapture`, which asks
/// for `?1003` as well - a report for every cell the pointer crosses, button held or
/// not. Nothing here follows a pointer around, so each of those would be a process
/// woken, an event parsed and a frame redrawn - artwork and all - for a mouse merely on
/// its way across the window. What is asked for instead is:
///
/// * `1000` the buttons: pressed, released, and the wheel
/// * `1002` movement while a button is held, which is what makes a drag a drag
/// * `1006` the coordinates as digits, without which a column past 223 cannot be
///   reported at all
///
/// crossterm reads whatever arrives rather than whatever was asked for, so asking for
/// less than its own command does costs nothing on the way back in - and `Moved` simply
/// never turns up.
const ASK: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";

/// The same three, undone in the order they were asked for.
const STOP: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// What the pointer is over. Geometry only: which column, not which item - the item
/// needs the list's own offset and length, which live on the pane and are asked for
/// there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A column, wherever in it: a border and a title are part of the column to anyone
    /// aiming a pointer at one.
    Column(Focus),
    /// Inside the language list.
    Picker,
    /// The language list is open and the pointer is somewhere else, which is how it is
    /// cancelled.
    Outside,
    /// A word along an edge, which answers by doing what its key does.
    Button(Command),
    /// Nothing worth a click.
    Nothing,
}

/// Where the last frame put the things a pointer can land on.
///
/// Filled in at the end of every `ui::draw` and read by the next mouse event. The event
/// loop draws before it polls, so what is written here always describes the frame the
/// user was looking at when they clicked on it - which is why none of it has to be
/// recomputed, or guessed at.
#[derive(Debug, Default, Clone)]
pub struct Regions {
    /// The whole frame. One that has never been drawn has no area at all, so the first
    /// event of a run - and any that arrives between a resize and the redraw answering
    /// it - lands on nothing rather than on a layout that is gone.
    pub area: Rect,
    /// The three columns, borders and titles included.
    pub series: Rect,
    pub seasons: Rect,
    pub episodes: Rect,
    /// The language list while it is open, and nothing while it is not.
    pub picker: Rect,
    /// Every word drawn along the top and bottom edges, and the command it runs. One
    /// list, because they are all the same thing: a word somewhere, that answers.
    pub buttons: Vec<(Command, Rect)>,
}

impl Regions {
    /// The box a column was drawn in.
    pub const fn column(&self, focus: Focus) -> Rect {
        match focus {
            Focus::Series => self.series,
            Focus::Seasons => self.seasons,
            Focus::Episodes => self.episodes,
        }
    }

    /// What is under `at`.
    ///
    /// The language list has first refusal, because it is drawn over everything else:
    /// while it is open, a click that misses it cancels it rather than reaching whatever
    /// it happened to be covering. The words along the edges come next. Nothing overlaps
    /// in practice - the footer is below the columns and the header above them - but the
    /// order is written down so that a layout which ever does overlap resolves the same
    /// way twice.
    pub fn at(&self, at: Position) -> Target {
        if !self.area.contains(at) {
            return Target::Nothing;
        }
        if !self.picker.is_empty() {
            return if self.picker.contains(at) {
                Target::Picker
            } else {
                Target::Outside
            };
        }
        if let Some((command, _)) = self.buttons.iter().find(|(_, area)| area.contains(at)) {
            return Target::Button(*command);
        }
        [Focus::Series, Focus::Seasons, Focus::Episodes]
            .into_iter()
            .find(|focus| self.column(*focus).contains(at))
            .map_or(Target::Nothing, Target::Column)
    }
}

/// The item under `row` in a list drawn in `outer`, or nothing for the border, the
/// title, a row past the end of the list, and the one placeholder row an empty column
/// draws.
///
/// `offset` is what the list widget wrote back as it drew - the index of its first
/// visible row - so this is where the list actually was rather than where it was asked
/// to be. Every row in this interface is exactly one line, which is what makes the
/// arithmetic a subtraction rather than a search.
pub fn row_at(outer: Rect, offset: usize, len: usize, row: u16) -> Option<usize> {
    let inner = outer.inner(Margin::new(1, 1));
    if len == 0 || !inner.contains(Position::new(inner.x, row)) {
        return None;
    }
    let index = offset.saturating_add(usize::from(row - inner.y));
    (index < len).then_some(index)
}

/// Where a run of words drawn one after another from `x` ended up.
///
/// Widths are display cells rather than bytes, because `日本語` is three characters and
/// six columns, and a label measured any other way answers to the left of where it was
/// printed. A word with no command is spacing: it takes its room and gets no box. A word
/// running off the edge of a narrow terminal keeps the part that was drawn, and one
/// wholly off it gets nothing - there is no sense in a button nobody can see.
pub fn lay_out(area: Rect, x: u16, words: &[(Option<Command>, u16)]) -> Vec<(Command, Rect)> {
    let mut boxes = Vec::new();
    let mut cursor = x;
    for (command, width) in words {
        if let Some(command) = command {
            let drawn = Rect {
                x: cursor,
                y: area.y,
                width: *width,
                height: area.height,
            }
            .intersection(area);
            if !drawn.is_empty() {
                boxes.push((*command, drawn));
            }
        }
        cursor = cursor.saturating_add(*width);
    }
    boxes
}

/// Turns pointer reporting on, and says whether it went on, so that it is only ever
/// turned off again by a run that turned it on.
pub fn enable() -> bool {
    let mut out = stdout();
    if !out.is_terminal() {
        return false;
    }
    write!(out, "{ASK}").is_ok() && out.flush().is_ok()
}

/// Turns pointer reporting off. Always before raw mode is given up, never after: a
/// report arriving in the moment between the two is printed on the screen as
/// `^[[<0;40;12M` by a terminal that is no longer swallowing it.
pub fn disable() {
    let mut out = stdout();
    let _ = write!(out, "{STOP}");
    let _ = out.flush();
}

/// Chains a panic hook that stops the reporting.
///
/// ratatui installs one in `try_init` that leaves the alternate screen and raw mode and
/// knows nothing about a pointer. This one runs first and hands on to it, so a panic
/// gives the terminal back whole rather than leaving it reporting a mouse at a shell
/// that has no idea what to do with one.
pub fn restore_on_panic() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        disable();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::{Command, Focus, Position, Rect, Regions, Target, lay_out, row_at};

    /// A column ten rows tall at the top of the screen: one row of border, eight of
    /// list, one of border.
    const COLUMN: Rect = Rect {
        x: 0,
        y: 3,
        width: 30,
        height: 10,
    };

    #[test]
    fn a_row_is_the_offset_plus_the_way_down_the_column() {
        assert_eq!(
            row_at(COLUMN, 0, 5, 3),
            None,
            "the top border is not an item"
        );
        assert_eq!(
            row_at(COLUMN, 0, 5, 12),
            None,
            "the bottom border is not an item"
        );
        assert_eq!(row_at(COLUMN, 0, 5, 4), Some(0));
        assert_eq!(row_at(COLUMN, 0, 5, 6), Some(2));
        assert_eq!(
            row_at(COLUMN, 0, 5, 9),
            None,
            "a row past the end of a short list holds nothing"
        );

        // A scrolled list draws its offset first, so the row under the pointer is not
        // the index it would have been at the top.
        assert_eq!(row_at(COLUMN, 40, 100, 4), Some(40));
        assert_eq!(row_at(COLUMN, 40, 100, 11), Some(47));

        assert_eq!(
            row_at(COLUMN, 0, 0, 4),
            None,
            "the row an empty column draws is a sentence, not an item"
        );
        let sliver = Rect {
            width: 1,
            height: 1,
            ..COLUMN
        };
        assert_eq!(row_at(sliver, 0, 5, 3), None, "a box with no inside");
    }

    #[test]
    fn a_word_answers_where_it_was_drawn() {
        let bar = Rect {
            x: 0,
            y: 20,
            width: 40,
            height: 1,
        };
        let words = [
            (Some(Command::Search), 8),
            (None, 3),
            (Some(Command::Quit), 6),
        ];
        let boxes = lay_out(bar, 1, &words);
        assert_eq!(
            boxes,
            vec![
                (
                    Command::Search,
                    Rect {
                        x: 1,
                        y: 20,
                        width: 8,
                        height: 1
                    }
                ),
                (
                    Command::Quit,
                    Rect {
                        x: 12,
                        y: 20,
                        width: 6,
                        height: 1
                    }
                ),
            ],
            "the gap takes its room and gets no box of its own"
        );

        let narrow = Rect { width: 15, ..bar };
        let boxes = lay_out(narrow, 1, &words);
        assert_eq!(
            boxes,
            vec![
                (
                    Command::Search,
                    Rect {
                        x: 1,
                        y: 20,
                        width: 8,
                        height: 1
                    }
                ),
                (
                    Command::Quit,
                    Rect {
                        x: 12,
                        y: 20,
                        width: 3,
                        height: 1
                    }
                ),
            ],
            "a word over the edge keeps the part that was drawn"
        );

        assert!(
            lay_out(Rect { width: 10, ..bar }, 1, &words).len() == 1,
            "a word wholly past the edge is not a button"
        );
    }

    #[test]
    fn the_language_list_takes_every_click_while_it_is_open() {
        let mut regions = Regions {
            area: Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
            series: COLUMN,
            seasons: Rect {
                x: 30,
                width: 20,
                ..COLUMN
            },
            episodes: Rect { x: 50, ..COLUMN },
            picker: Rect::default(),
            buttons: vec![(
                Command::Quit,
                Rect {
                    x: 70,
                    y: 23,
                    width: 6,
                    height: 1,
                },
            )],
        };

        let on_an_episode = Position::new(55, 6);
        assert_eq!(regions.at(on_an_episode), Target::Column(Focus::Episodes));
        assert_eq!(
            regions.at(Position::new(2, 4)),
            Target::Column(Focus::Series)
        );
        assert_eq!(
            regions.at(Position::new(72, 23)),
            Target::Button(Command::Quit)
        );
        assert_eq!(regions.at(Position::new(2, 20)), Target::Nothing);
        assert_eq!(
            regions.at(Position::new(100, 4)),
            Target::Nothing,
            "a report from outside the frame is not a click on the edge of it"
        );

        regions.picker = Rect {
            x: 20,
            y: 8,
            width: 42,
            height: 6,
        };
        assert_eq!(regions.at(Position::new(30, 10)), Target::Picker);
        assert_eq!(
            regions.at(on_an_episode),
            Target::Outside,
            "while the list is open it is the only thing under the pointer"
        );
        assert_eq!(regions.at(Position::new(72, 23)), Target::Outside);

        assert_eq!(
            Regions::default().at(Position::new(0, 0)),
            Target::Nothing,
            "nothing has been drawn yet, so nothing can be clicked"
        );
    }
}
