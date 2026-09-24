use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect, Size};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, HighlightSpacing, List, ListItem, Padding, Paragraph, Wrap};

use crate::download::OnDisk;
use crate::model::{CatalogItem, Playhead, Season, SeasonEpisode, single_name};
use crate::play::resume_at;
use crate::util::language_name;

use super::app::{App, Download, Editing, Focus, Pane, Picker, State, season_title};
use super::keys::{Bindings, Command};
use super::mouse::{self, Regions};
use super::theme::Theme;
use super::worker::{Choice, FilterKind, Listing};

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

/// What a column is called, wherever it has to be named. One spelling, so that the box
/// asking which rows to keep names the column the way the column names itself.
///
/// The middle column is named after what it is holding rather than after what it usually
/// holds: a film's one row is not a season, and a header still calling it one would be the
/// last thing on screen saying so. Which also means `f` in that column says `Filter Film:`
/// - the box and the title cannot drift apart, because they ask the same function.
fn column_name(app: &App, focus: Focus) -> &'static str {
    match focus {
        Focus::Series => "Series",
        Focus::Seasons => app
            .seasons
            .items
            .first()
            .and_then(|season| single_name(&season.kind))
            .unwrap_or("Seasons"),
        Focus::Episodes => "Episodes",
        Focus::Downloads => "Downloads",
    }
}

/// A column's name, carrying what it has been narrowed to while it is narrowed:
/// `Episodes "journ" 2/24`.
///
/// A narrowing is invisible otherwise, since the rows it hid are simply not there, and
/// a column quietly showing two of a season's twenty-four episodes is the sort of thing
/// somebody comes back to after a cup of tea and reads as a client that has mislaid
/// half the season. The count is both numbers rather than the one, because how much is
/// missing is the part that cannot be seen.
fn pane_title<T>(name: &str, pane: &Pane<T>) -> String {
    let glyph = glyph(name);
    match pane.query() {
        Some(query) => format!(
            "{glyph} {name} \"{query}\" {}/{}",
            pane.rows(),
            pane.items.len()
        ),
        None => format!("{glyph} {name}"),
    }
}

/// The small mark in front of a column's title, so that the four boxes can be told
/// apart at a glance before any of their words have been read.
///
/// Keyed on the name [`column_name`] gives rather than on the focus, because that name
/// is what every title is built from; whatever the middle column is called - `Seasons`,
/// `Film`, `Music` - it is the middle column. The glyphs are all ones a monospace font
/// draws a single cell wide and none of them has an emoji form, which is what would
/// otherwise knock every title after it a column out of line.
fn glyph(name: &str) -> &'static str {
    match name {
        "Series" => "\u{25a4}",
        "Episodes" | "Episode" => "\u{25b8}",
        "Downloads" | "Download" => "\u{21e3}",
        _ => "\u{25eb}",
    }
}

/// What a column with no rows in it has to say for itself. A column with nothing in it
/// and a column narrowed until nothing is left are different problems, and the second
/// one is the user's own doing and a keystroke to undo, so it says so and quotes what
/// it was looking for. An empty box under a title reading `Episodes "xyz" 0/24` would
/// be the interface knowing the answer and keeping it to itself.
fn nothing_shown<T>(pane: &Pane<T>, idle: &str) -> String {
    match pane.query() {
        Some(query) if !pane.items.is_empty() => format!("Nothing matches \"{query}\"."),
        _ => idle.to_owned(),
    }
}

/// A column's box: the border and the title in the accent while it has the keyboard,
/// and both stepped back while it does not, so the focused column is the one thing on
/// the screen framed in colour.
fn pane_block(theme: &Theme, title: &str, focused: bool) -> Block<'static> {
    let heading = if focused {
        theme.title(format!(" {title} "))
    } else {
        theme.heading(format!(" {title} "))
    };
    theme.bordered(focused).title(heading)
}

/// What a column shows when it holds nothing: why it is empty, or that it is still
/// waiting for an answer.
///
/// The waiting and the idle sentence sit in the middle of the column, where they read as
/// something the column is saying rather than as a row of it; a failure keeps to the
/// left, because it is often longer than the column is wide and a centred line cut off
/// at both ends would lose the start of it, which is where the reason usually is.
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
            theme.dim(" Loading\u{2026}"),
        ])
        .centered()
    } else if let Some(error) = error {
        Line::from(vec![theme.error("\u{2717} "), theme.error(error.clone())])
    } else {
        Line::from(theme.dim(idle.to_owned())).centered()
    };
    vec![ListItem::new(line)]
}

/// The little words after a title, held apart by a dot. `(true, ..)` is a word that
/// stands out - what kind of thing a row is where it is not a series, or that it is
/// simulcasting - and is drawn in the accent; the rest are dim.
fn tags(theme: &Theme, tags: Vec<(bool, String)>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, (loud, word)) in tags.into_iter().enumerate() {
        spans.push(theme.dim(if index == 0 { "  " } else { DOT }));
        spans.push(if loud {
            theme.accent(word)
        } else {
            theme.dim(word)
        });
    }
    spans
}

fn series_row(theme: &Theme, series: &CatalogItem) -> ListItem<'static> {
    let metadata = &series.series_metadata;
    let film = &series.movie_listing_metadata;
    let mut words = Vec::new();
    // What it is, where it is not a series. `2 seasons` is a thing to say about a series
    // and nothing to say about a film, and the catalogue now mixes the two: a row that
    // said neither would leave the column looking like a list of series with some odd
    // short ones in it.
    if let Some(word) = single_name(&series.kind) {
        words.push((true, word.to_lowercase()));
    } else if metadata.season_count > 1 {
        words.push((false, format!("{} seasons", metadata.season_count)));
    }
    if metadata.is_dubbed || film.is_dubbed {
        words.push((false, "dub".to_owned()));
    }
    // New episodes every week is the one thing about a series that changes what
    // somebody does next, so it is worth a word in the row rather than only in the panel.
    if metadata.is_simulcast {
        words.push((true, "simulcast".to_owned()));
    }
    let mut spans = vec![theme.strong(series.title.clone())];
    spans.extend(tags(theme, words));
    ListItem::new(Line::from(spans))
}

fn season_row(theme: &Theme, season: &Season, series_title: &str) -> ListItem<'static> {
    // A season usually carries the title of the series, which the column to the left
    // is already showing - see [`season_title`], which the narrowing reads a row with.
    let mut spans = vec![theme.text(season_title(season, series_title))];
    if season.number_of_episodes > 0 {
        spans.extend(tags(
            theme,
            vec![(false, format!("{} ep", season.number_of_episodes))],
        ));
    }
    ListItem::new(Line::from(spans))
}

/// What a marked episode is drawn with. A bar rather than a dot: every other glyph this
/// column can show is round or a tick - the two circles say what is on the disk and the
/// check is the account's opinion of the episode - and a mark is none of those, so it
/// takes a shape none of them has and stands at the left edge, beside the cursor, where
/// what the user has picked belongs.
const MARK: &str = "\u{258c}";

/// One episode: whether it is marked, its number, what this machine already has of it,
/// what the account has already made of it, and its title.
///
/// The marker takes the running time's place rather than a column of its own. The three
/// lists are already fighting for room on a narrow terminal, and a title pushed off the
/// right edge is a worse loss than a running time - which for an episode left partway
/// through is the less interesting of the two anyway, since where to pick it up says more
/// than how long it lasts. It is the same rule playing uses, so a row showing a time is a
/// row mpv opens at that time, and a check is an episode it would start from the top.
///
/// What is on this disk cannot share that slot, because it is a different fact: an
/// episode can be downloaded and never watched, or watched on the phone and never
/// downloaded, and a row has to be able to say both at once. It gets two cells ahead of
/// the time - a filled circle for the whole episode, a half-filled one for an episode
/// that has been started and not finished, whether it is being written now or was left
/// that way by a run that stopped - and the two cells are spent whether there is a file
/// or not, since a marker that appeared only when it had something to report would shift
/// every title in the column as the eye ran down it.
///
/// A mark is a third fact, and the only one of the three the user put there, so it takes
/// a cell of its own in front of the number: `mark` says whether this row has one, or
/// `None` where the season has no marks at all and the cell is not drawn. That cell
/// comes and goes where the disk marker's two never do, and the reason is the same
/// reason - nothing may shift while the eye runs down a column. A mark is a fact about
/// the whole season, so it is the whole column that gains the cell with the first mark
/// and loses it with the last; the disk marker is a fact about one row, and a cell that
/// came and went row by row is what would leave the titles ragged. Between them they
/// keep the rule the first paragraph is about: a gutter standing empty down every season
/// nobody is marking would spend a column of every title on the seasons that are.
fn episode_row(
    theme: &Theme,
    episode: &SeasonEpisode,
    playhead: Option<&Playhead>,
    held: OnDisk,
    mark: Option<bool>,
) -> ListItem<'static> {
    let number = if episode.episode.is_empty() {
        episode.episode_number.to_string()
    } else {
        episode.episode.clone()
    };
    // An episode the account has finished steps back: its number and its title go dim
    // and the check stands out instead, so what is left to watch in a season is what the
    // eye lands on and the finished ones read as done without being hidden.
    let watched = playhead.is_some_and(|seen| seen.fully_watched);
    let quiet = |text: String| {
        if watched {
            theme.dim(text)
        } else {
            theme.accent(text)
        }
    };
    let mut spans = Vec::new();
    if let Some(marked) = mark {
        spans.push(theme.accent(if marked { MARK } else { " " }));
    }
    // `E1` is a number an episode has and a film has not, so a row that is a whole thing
    // in itself is called what the rest of the interface calls it. The word is spent in
    // the four cells the number slot already takes, so nothing below it moves.
    spans.push(match single_name(&episode.kind) {
        Some(word) => quiet(format!("{word:<4}")),
        None => quiet(format!("E{number:<3}")),
    });
    spans.push(match held {
        OnDisk::Complete => theme.accent("● "),
        OnDisk::Partial => theme.dim("◐ "),
        OnDisk::Missing => theme.dim("  "),
    });
    let resume =
        playhead.and_then(|seen| resume_at(seen.playhead, episode.duration_ms, seen.fully_watched));
    if let Some(seconds) = resume {
        spans.push(theme.title(format!("{:>6}", duration(u64::from(seconds) * 1000))));
        spans.push(theme.dim("  "));
    } else if watched {
        spans.push(theme.accent(format!("{:>6}", "✓")));
        spans.push(theme.dim("  "));
    } else if episode.duration_ms > 0 {
        spans.push(theme.dim(format!("{:>6}  ", duration(episode.duration_ms))));
    }
    spans.push(if watched {
        theme.dim(episode.title.clone())
    } else {
        theme.text(episode.title.clone())
    });
    ListItem::new(Line::from(spans))
}

/// A bar as two runs of line: the part that is done, heavy, and the track still to go,
/// thin - given back apart so the done part can be drawn in the accent and the track
/// left dim, which is what makes a bar read at a glance rather than by counting. A
/// fraction with no number behind it yet - a subtitle fetch, a mux, a track whose server
/// would not say how long the file is - is an empty track rather than a full one, which
/// is the honest reading of not knowing.
fn meter(fraction: Option<f64>, width: u16) -> (String, String) {
    let width = usize::from(width);
    let filled = fraction.map_or(0.0, |fraction| fraction.clamp(0.0, 1.0) * width as f64) as usize;
    ("\u{2501}".repeat(filled), "\u{2500}".repeat(width - filled))
}

/// How wide the bar in the queue is. Wide enough to read a tenth off, narrow enough to
/// leave the titles the width they had.
const METER_WIDTH: u16 = 20;

/// One queued episode: which episode it is, and what has become of it.
///
/// The titles are padded out to `column` so that the bars under one another line up -
/// the queue is read down the state rather than across one row of it, and a column of
/// bars each starting somewhere else is a list nobody can scan. The colours say the
/// same thing the words do, since a row that has failed is the one worth finding first.
fn download_row(theme: &Theme, download: &Download, column: usize) -> ListItem<'static> {
    let mut spans = vec![theme.accent(format!("{:<7}", download.number))];
    let title = Span::raw(download.title.clone()).width();
    spans.push(theme.text(download.title.clone()));
    spans.push(theme.text(" ".repeat(column.saturating_sub(title) + 2)));
    match &download.state {
        State::Queued => spans.push(theme.dim("\u{25e6} queued")),
        State::Running => {
            let (stage, fraction) = download.stage().unwrap_or(("starting", None));
            let (done, left) = meter(fraction, METER_WIDTH);
            spans.push(theme.accent(done));
            spans.push(theme.dim(left));
            spans.push(theme.title(match fraction {
                Some(fraction) => format!(" {:>3}%", (fraction * 100.0) as u16),
                None => "     ".to_owned(),
            }));
            spans.push(theme.dim(format!("  {stage}")));
        }
        State::Done => spans.push(theme.accent("\u{2713} done")),
        State::Failed(error) => spans.push(theme.error(format!("\u{2717} failed: {error}"))),
    }
    ListItem::new(Line::from(spans))
}

/// A run of words drawn one after another, each with the command a click on it runs, or
/// nothing where the run is only spacing. The line that is drawn and the boxes a pointer
/// is hit against both come out of one of these, so a word cannot be printed in one
/// place and answer in another.
type Run = Vec<(Option<Command>, Vec<Span<'static>>)>;

/// What a string is worth in columns. A label measured in bytes puts its box six columns
/// to the left of itself the moment a language name is not written in ASCII.
fn cells(spans: &[Span<'static>]) -> u16 {
    let width: usize = spans.iter().map(Span::width).sum();
    u16::try_from(width).unwrap_or(u16::MAX)
}

/// The words of a run, as the width each takes.
fn widths(run: &Run) -> Vec<(Option<Command>, u16)> {
    run.iter()
        .map(|(command, spans)| (*command, cells(spans)))
        .collect()
}

fn line(run: &Run) -> Line<'static> {
    Line::from(
        run.iter()
            .flat_map(|(_, spans)| spans.iter().cloned())
            .collect::<Vec<_>>(),
    )
}

/// How much of the list the catalogue column is holding, for the header.
///
/// The column loads a page at a time, so the count on its own would be the one number in
/// the interface that quietly means something different from what it says: `100 series`
/// where there are twelve hundred of them reads as the end of the catalogue rather than
/// as the first hundredth of it. Against the total it says both things at once - how far
/// the list has been read, and that there is more of it to walk to.
///
/// Where the total is unknown it is left out rather than guessed at. The watchlist, the
/// history and a search have no figure that counts the same things this column shows -
/// see [`crate::api::Page`] - and a page the simulcast filter has sieved has thrown away
/// rows the total still counts, so it arrives without one. `100 of 100 series` for a list
/// that goes on would be a worse answer than saying nothing about the length at all.
fn tally(app: &App) -> String {
    let loaded = app.series.items.len();
    match app.paging.total {
        Some(total) => format!("{loaded} of {total} series"),
        None => format!("{loaded} series"),
    }
}

/// What the words along the header are held apart by: a dot with room either side, the
/// same separator the details panel puts between its facts, so a line of the interface
/// reads as a list of things wherever it is one.
const DOT: &str = " \u{b7} ";

/// The left of the header: what is being listed, how much of it, and what it is narrowed
/// by.
///
/// The label is a button, because what it says is exactly what a click on it changes -
/// `Popular`, `Watchlist` and `Continue watching` alike move on to the next list, and
/// `Search: frieren` leaves the search. The count beside it is not. The filters that
/// follow are buttons too, each opening the list it was chosen from, which is where it
/// is cleared as well as where it is set.
///
/// They are drawn only while a browse listing is on screen. The other three lists are
/// not narrowed by them - see [`Filters`](super::worker::Filters) - and a header is a
/// description of what is under it, not of what the interface is holding. Leaving the
/// catalogue with a filter on says so on the status line, so the words do not simply
/// vanish.
///
/// While the box is being typed into, none of it is a button: a click there closes the
/// box, as escape does. It carries its own two keys at the end instead, because those
/// are the only two that mean anything while it is open and neither is on the line
/// along the bottom.
fn listing(app: &App) -> Run {
    let theme = &app.theme;
    match &app.editing {
        // The two boxes are told apart by the word in front of them, because they are
        // the two things a box of typing could be doing and only one of them is about
        // to go to Crunchyroll: `Search:` replaces this column with an answer, and
        // `Filter Episodes:` leaves a column alone but for the rows it is hiding.
        Some(editing) => {
            let (prompt, enter, escape) = match editing {
                Editing::Search(_) => ("Search: ".to_owned(), "search", "cancel"),
                Editing::Narrow { focus, .. } => (
                    format!("Filter {}: ", column_name(app, *focus)),
                    "keep",
                    "clear",
                ),
            };
            vec![(
                None,
                vec![
                    theme.text(" "),
                    theme.title(prompt),
                    theme.strong(editing.query().to_owned()),
                    theme.accent("\u{258f}"),
                    theme.dim("   "),
                    theme.title("\u{23ce}"),
                    theme.dim(format!(" {enter}{DOT}")),
                    theme.title("esc"),
                    theme.dim(format!(" {escape}")),
                ],
            )]
        }
        None => {
            let mut run = vec![
                (None, vec![theme.text(" ")]),
                (
                    Some(match app.listing {
                        Listing::Browse(_) | Listing::Watchlist | Listing::History => {
                            Command::Order
                        }
                        Listing::Search(_) => Command::Back,
                    }),
                    vec![theme.title(app.listing.label())],
                ),
                (None, vec![theme.dim(format!("{DOT}{}", tally(app)))]),
            ];
            if matches!(app.listing, Listing::Browse(_)) {
                let filters = [
                    (Command::Genre, app.filters.word(FilterKind::Genre)),
                    (Command::AnimeSeason, app.filters.word(FilterKind::Season)),
                    (
                        Command::Simulcast,
                        app.filters.simulcast.then(|| "Simulcast".to_owned()),
                    ),
                ];
                for (command, word) in filters {
                    if let Some(word) = word {
                        run.push((None, vec![theme.dim(DOT)]));
                        run.push((Some(command), vec![theme.accent(word)]));
                    }
                }
            }
            run
        }
    }
}

/// The three settings along the top right, in the order they are drawn, each with the
/// command a click on it runs: a dim word saying which setting it is and the value in
/// the accent, so the eye reads the values and the words are there for whoever needs
/// telling what `日本語` is the setting of.
fn settings(app: &App) -> Run {
    let theme = &app.theme;
    let chip = |command, what: &str, value: String| {
        (
            Some(command),
            vec![theme.dim(format!("{what} ")), theme.title(value)],
        )
    };
    vec![
        (None, vec![theme.text(" ")]),
        chip(
            Command::AudioLanguage,
            "audio",
            language_name(&app.audio()).to_owned(),
        ),
        (None, vec![theme.dim(DOT)]),
        chip(
            Command::SubtitleLanguage,
            "subs",
            language_name(&app.subs()).to_owned(),
        ),
        (None, vec![theme.dim(DOT)]),
        chip(Command::Quality, "video", app.options.video_quality.clone()),
        (None, vec![theme.text(" ")]),
    ]
}

/// The name of the program, set in the top left corner of the header as a label of its
/// own so the frame has a title the way a window does.
const NAME: &str = " crunchyroll-tui ";

fn header(app: &App) -> Paragraph<'static> {
    let theme = &app.theme;
    let block = theme
        .bordered(false)
        .title(Line::from(vec![
            theme.text(" "),
            theme.badge(NAME),
            theme.text(" "),
        ]))
        .title_top(line(&settings(app)).right_aligned());
    Paragraph::new(line(&listing(app))).block(block)
}

/// A line of facts held apart by dots: the plain ones in the heading colour, and after
/// them the ones worth finding first - that a series is simulcasting, where an episode
/// was left off - in the accent.
fn facts(theme: &Theme, plain: Vec<String>, loud: Vec<String>) -> Line<'static> {
    let mut spans = Vec::new();
    let words = plain
        .into_iter()
        .map(|fact| theme.heading(fact))
        .chain(loud.into_iter().map(|fact| theme.accent(fact)));
    for (index, word) in words.enumerate() {
        if index > 0 {
            spans.push(theme.dim(DOT));
        }
        spans.push(word);
    }
    Line::from(spans)
}

/// The panel under the columns: everything about the item the cursor is on that does
/// not fit on its one line, as a card - the title in the accent, a line of facts, and
/// then the description - and what to call the card, which is what it is describing
/// rather than a word like `Details` that would be true of any of them.
fn details(app: &App) -> (&'static str, Vec<Line<'static>>) {
    let theme = &app.theme;
    let mut lines = Vec::new();
    let name = match app.focus {
        Focus::Series | Focus::Seasons => {
            let Some(series) = app.series.selected() else {
                return ("Series", lines);
            };
            let metadata = &series.series_metadata;
            // A film keeps the same facts under a name of its own and leaves
            // `series_metadata` empty, so both are read and whichever has something to
            // say fills the line. Nothing has both.
            let film = &series.movie_listing_metadata;
            lines.push(Line::from(theme.title(series.title.clone())));
            let mut plain = Vec::new();
            let mut loud = Vec::new();
            // What it is comes first where it is not a series, because everything after
            // it reads differently for a film - a running time rather than a count of
            // episodes - and this is the one place with room to say which is being
            // described.
            if let Some(word) = single_name(&series.kind) {
                plain.push(word.to_owned());
            }
            let year = if metadata.series_launch_year > 0 {
                metadata.series_launch_year
            } else {
                film.movie_release_year
            };
            if year > 0 {
                plain.push(year.to_string());
            }
            if metadata.season_count > 1 {
                plain.push(format!("{} seasons", metadata.season_count));
            }
            if metadata.episode_count > 0 {
                plain.push(format!("{} episodes", metadata.episode_count));
            }
            if film.duration_ms > 0 {
                plain.push(duration(film.duration_ms));
            }
            if !metadata.audio_locales.is_empty() {
                plain.push(format!("{} audio tracks", metadata.audio_locales.len()));
            }
            let subtitles = if metadata.subtitle_locales.is_empty() {
                &film.subtitle_locales
            } else {
                &metadata.subtitle_locales
            };
            if !subtitles.is_empty() {
                plain.push(format!("{} subtitles", subtitles.len()));
            }
            plain.extend(
                metadata
                    .maturity_ratings
                    .iter()
                    .chain(film.maturity_ratings.iter())
                    .cloned(),
            );
            if metadata.is_simulcast {
                loud.push("simulcast".to_owned());
            }
            lines.push(facts(theme, plain, loud));
            lines.push(Line::from(theme.text(series.description.clone())));
            single_name(&series.kind).unwrap_or("Series")
        }
        Focus::Episodes => {
            let Some(episode) = app.episodes.selected() else {
                return ("Episode", lines);
            };
            lines.push(Line::from(theme.title(episode.title.clone())));
            let mut plain = vec![single_name(&episode.kind).map_or_else(
                || {
                    format!(
                        "S{}E{}",
                        episode.season_number,
                        if episode.episode.is_empty() {
                            episode.episode_number.to_string()
                        } else {
                            episode.episode.clone()
                        }
                    )
                },
                str::to_owned,
            )];
            if episode.duration_ms > 0 {
                plain.push(duration(episode.duration_ms));
            }
            if !episode.audio_locale.is_empty() {
                plain.push(language_name(&episode.audio_locale).to_owned());
            }
            if episode.versions.len() > 1 {
                plain.push(format!("{} dubs", episode.versions.len()));
            }
            if let Some(date) = episode.availability_starts.split('T').next()
                && !date.is_empty()
            {
                plain.push(date.to_owned());
            }
            // What the row says in a glyph and a number, said here in words: the row
            // has four cells for it and the panel has the width of the screen.
            let mut loud = Vec::new();
            let playhead = app.playheads.get(&episode.id);
            if let Some(seconds) = playhead
                .and_then(|seen| resume_at(seen.playhead, episode.duration_ms, seen.fully_watched))
            {
                loud.push(format!("resume at {}", duration(u64::from(seconds) * 1000)));
            } else if playhead.is_some_and(|seen| seen.fully_watched) {
                loud.push("watched".to_owned());
            }
            match app.downloaded.get(&episode.id) {
                Some(OnDisk::Complete) => loud.push("on disk".to_owned()),
                Some(OnDisk::Partial) => loud.push("partly on disk".to_owned()),
                Some(OnDisk::Missing) | None => {}
            }
            lines.push(facts(theme, plain, loud));
            lines.push(Line::from(theme.text(episode.description.clone())));
            single_name(&episode.kind).unwrap_or("Episode")
        }
        // The panel is one line per episode and a failure is a sentence out of
        // anyhow's chain, so this is the only place with room to say what went wrong.
        Focus::Downloads => {
            let Some(download) = app.downloads.selected() else {
                return ("Download", lines);
            };
            lines.push(Line::from(theme.title(download.title.clone())));
            let mut plain = vec![download.number.clone()];
            if !download.series.is_empty() {
                plain.push(download.series.clone());
            }
            lines.push(facts(theme, plain, Vec::new()));
            lines.push(Line::from(match &download.state {
                State::Queued => theme.dim("Waiting for the episode in front of it."),
                State::Running => match download.stage() {
                    Some((stage, _)) => theme.text(format!("Downloading: {stage}.")),
                    None => theme.text("Starting."),
                },
                State::Done => theme.dim("Downloaded."),
                State::Failed(error) => theme.error(error.clone()),
            }));
            "Download"
        }
    };
    (name, lines)
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

/// How many rows the queue gets out of the body: none at all while it is empty, and
/// otherwise a row per episode plus the border, within what the body can spare.
///
/// A strip under the three columns rather than a fourth column beside them. A fourth
/// column would take a quarter of the width off the lists for the whole of a run, and
/// most of a run has nothing downloading; a strip that is not drawn at all while the
/// queue is empty costs the columns nothing, which is the state the interface spends
/// most of its time in. A queue also reads across rather than down - an episode, a bar,
/// a percentage - so a wide short box fits it and a tall narrow one does not.
///
/// It may take half the body while it has the keyboard, because that is when someone is
/// reading it, and a third of it otherwise, which is enough to keep an eye on. Either
/// way the columns keep the rest: the queue is a thing to glance at, not the interface.
fn downloads_height(body: Rect, queued: usize, focused: bool) -> u16 {
    if queued == 0 {
        return 0;
    }
    let share = if focused {
        body.height / 2
    } else {
        body.height / 3
    };
    let wanted = u16::try_from(queued).unwrap_or(u16::MAX).saturating_add(2);
    // A box with no room for a row of its own is worse than none: it is a border
    // sitting where a list could have been.
    match wanted.min(share) {
        0..=2 => 0,
        height => height,
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

/// One of the lists that open over the interface: every row it offers, the one in force
/// marked, and each row's value beside its words - for anyone who thinks in locales
/// rather than in languages, or who wants to see that `Slice of Life` is the
/// `slice-of-life` a URL would carry.
///
/// A list whose rows have not arrived yet says so in the hint along the bottom edge
/// rather than in a row of its own, because a row there would be one the cursor could
/// land on and apply.
///
/// Gives back the box it drew itself in, which is what tells a click whether it landed
/// on the list or beside it.
fn picker_overlay(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    keys: &Bindings,
    picker: &mut Picker,
    current: &str,
    tick: usize,
) -> Rect {
    let width = |row: &Choice| Span::raw(row.label.as_str()).width();
    let column = picker.pane.items.iter().map(width).max().unwrap_or(0);
    let items: Vec<ListItem> = picker
        .pane
        .items
        .iter()
        .map(|row| {
            let padding = " ".repeat(column - width(row) + 2);
            ListItem::new(Line::from(vec![
                theme.text(if row.value == current { "● " } else { "  " }),
                theme.text(format!("{}{padding}", row.label)),
                theme.dim(row.value.clone()),
            ]))
        })
        .collect();
    // Wide enough for the longest name, and never so narrow that the hint along the
    // bottom edge is cut in half.
    let area = popup(
        area,
        (column as u16 + 20).max(42),
        picker.pane.items.len() as u16 + 2,
    );
    let mut hint = [
        (Command::Open, "apply"),
        (Command::NextColumn, "other list"),
        (Command::Back, "cancel"),
    ]
    .iter()
    .filter_map(|(command, what)| {
        let key = keys.first(*command);
        (!key.is_empty()).then(|| format!("{key} {what}"))
    })
    .collect::<Vec<_>>()
    .join(" · ");
    if picker.pane.loading {
        hint = format!("{} {hint}", SPINNER[tick % SPINNER.len()]);
    }
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                theme
                    .bordered(true)
                    .title(theme.title(picker.title()))
                    .title_bottom(theme.dim(format!(" {hint} "))),
            )
            .highlight_style(theme.highlight(true))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        area,
        &mut picker.pane.state,
    );
    area
}

/// What the help popup lists, and in what order. Commands that read as one line share a
/// row; the keys printed are whatever they are bound to, so a config that moves them
/// documents itself instead of leaving the popup lying.
const HELP: [(&[Command], &str); 23] = [
    (&[Command::Up, Command::Down], "move the cursor"),
    (
        &[Command::PageUp, Command::PageDown],
        "move a page at a time",
    ),
    (
        &[Command::Top, Command::Bottom],
        "jump to the first or last item",
    ),
    (
        &[Command::Open],
        "open the selection, play an episode, drop a download",
    ),
    (&[Command::Back], "go back a column, and leave a search"),
    (&[Command::NextColumn], "cycle the columns"),
    (&[Command::Search], "search the catalogue"),
    (
        &[Command::Filter],
        "narrow this column to the rows that match, as you type",
    ),
    (&[Command::Order], "change the list the catalogue shows"),
    (
        &[Command::Genre, Command::AnimeSeason],
        "narrow the catalogue by genre / anime season",
    ),
    (
        &[Command::Simulcast],
        "show only what is simulcasting, or stop",
    ),
    (
        &[Command::Play, Command::PlayRest],
        "play the episode / the rest of the season",
    ),
    (
        &[Command::Mark],
        "mark the episode, to queue several of them at once",
    ),
    (
        &[Command::Download, Command::DownloadSeason],
        "queue the episode or the marked ones / the whole season",
    ),
    (
        &[Command::Watchlist],
        "put the series on the watchlist, or take it off",
    ),
    (
        &[Command::MarkWatched, Command::MarkUnwatched],
        "mark the episode watched / unwatched",
    ),
    (
        &[Command::AudioLanguage, Command::SubtitleLanguage],
        "pick the audio / subtitle language",
    ),
    (
        &[Command::NextAudio, Command::NextSubtitle],
        "next audio / subtitle language, without the list",
    ),
    (&[Command::Quality], "cycle the video quality"),
    (
        &[Command::Images],
        "show or hide the poster and the episode still",
    ),
    (&[Command::Reload], "reload the current column"),
    (&[Command::Help], "show this list"),
    (&[Command::Quit], "quit"),
];

/// The reminder along the bottom edge: the handful worth a permanent line, with one key
/// each because there is no room for two, and what a click on the words runs.
///
/// The command a click runs is written down rather than taken to be the first of the
/// keys shown, because the two are not always the same thing: `move` names two keys, and
/// a pointer that wants to move has a wheel already.
const FOOTER: [(&[Command], &str, Option<Command>); 9] = [
    (&[Command::Up, Command::Down], "move", None),
    (&[Command::Open], "open/play", Some(Command::Open)),
    (&[Command::Back], "back", Some(Command::Back)),
    (&[Command::Search], "search", Some(Command::Search)),
    (&[Command::Download], "download", Some(Command::Download)),
    (
        &[Command::AudioLanguage, Command::SubtitleLanguage],
        "language",
        Some(Command::AudioLanguage),
    ),
    (&[Command::Quality], "quality", Some(Command::Quality)),
    (&[Command::Help], "keys", Some(Command::Help)),
    (&[Command::Quit], "quit", Some(Command::Quit)),
];

/// What the hints along the bottom edge are held apart by.
const FOOTER_GAP: &str = "   ";

/// The reminder along the bottom edge as a run of words: each hint a key in the accent
/// and what it does in dim, which is what lets the eye pick the keys out of the line
/// without a separator between every pair. One walk over the table makes both the line
/// and the boxes, so a hint unbound by the config leaves neither a gap in the line nor
/// a box over nothing.
fn footer(app: &App) -> Run {
    let theme = &app.theme;
    let mut run: Run = vec![(None, vec![theme.text(" ")])];
    for (commands, what, click) in FOOTER {
        let key = one_key(&app.keys, commands, "/");
        if key.is_empty() {
            continue;
        }
        if run.len() > 1 {
            run.push((None, vec![theme.dim(FOOTER_GAP)]));
        }
        run.push((click, vec![theme.title(key), theme.dim(format!(" {what}"))]));
    }
    run
}

/// What the interface is waiting on, if anything, in the words the status line uses
/// while there is no sentence of its own to show. The columns say `Loading…` where they
/// are empty, but a column that is being filled again keeps its old rows until the new
/// ones land, and a page appended to the catalogue happens below the fold - so this is
/// the one place that is always on screen to say something is on its way.
fn waiting_on(app: &App) -> Option<&'static str> {
    if app
        .picker
        .as_ref()
        .is_some_and(|picker| picker.pane.loading)
    {
        Some("Loading the list\u{2026}")
    } else if app.episodes.loading {
        Some("Loading episodes\u{2026}")
    } else if app.seasons.loading {
        Some("Loading seasons\u{2026}")
    } else if app.paging.asked.is_some_and(|start| start > 0) {
        Some("Loading more of the catalogue\u{2026}")
    } else if app.series.loading || app.paging.asked.is_some() {
        Some("Loading the catalogue\u{2026}")
    } else {
        None
    }
}

/// The status line: the last thing the interface had to say, with a mark in front
/// saying what kind of thing it was - a cross for a failure, a spinner while something
/// is on its way, a check otherwise.
fn status_line(app: &App) -> Line<'static> {
    let theme = &app.theme;
    let waiting = waiting_on(app);
    let icon = if waiting.is_some() {
        theme.accent(SPINNER[app.tick % SPINNER.len()])
    } else {
        theme.accent("\u{2713}")
    };
    let spans = match (&app.notice, waiting) {
        (Some(notice), _) if notice.error => {
            vec![theme.error(" \u{2717} "), theme.error(notice.text.clone())]
        }
        (Some(notice), _) => vec![
            theme.text(" "),
            icon,
            theme.text(format!(" {}", notice.text)),
        ],
        (None, Some(what)) => vec![theme.text(" "), icon, theme.dim(format!(" {what}"))],
        (None, None) => vec![theme.dim(" \u{2713} Ready.")],
    };
    Line::from(spans)
}

/// The right end of the status line: what the user has put in motion, so that a mark
/// or a queue scrolled out of sight is still counted somewhere. Empty while there is
/// nothing to count, which is most of the time.
fn activity(app: &App) -> Line<'static> {
    let theme = &app.theme;
    let mut words = Vec::new();
    if !app.marked.is_empty() {
        words.push(theme.accent(format!("{MARK} {} marked", app.marked.len())));
    }
    let count = |wanted: fn(&State) -> bool| {
        app.downloads
            .items
            .iter()
            .filter(|download| wanted(&download.state))
            .count()
    };
    let running = count(|state| matches!(state, State::Running));
    let queued = count(|state| matches!(state, State::Queued));
    let failed = count(|state| matches!(state, State::Failed(_)));
    if running > 0 {
        words.push(theme.accent(format!("\u{21e3} {running} downloading")));
    }
    if queued > 0 {
        words.push(theme.dim(format!("{queued} queued")));
    }
    if failed > 0 {
        words.push(theme.error(format!("{failed} failed")));
    }
    let mut spans = Vec::new();
    for (index, word) in words.into_iter().enumerate() {
        if index > 0 {
            spans.push(theme.dim(DOT));
        }
        spans.push(word);
    }
    if !spans.is_empty() {
        spans.push(theme.text(" "));
    }
    Line::from(spans)
}

/// Every key the commands answer to: `↑ k / ↓ j`. A command that has been unbound
/// contributes nothing rather than a gap.
fn every_key(keys: &Bindings, commands: &[Command], separator: &str) -> String {
    commands
        .iter()
        .map(|command| keys.label(*command))
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

/// One key each: `a/s`, for the places where a list of alternatives would not fit.
fn one_key(keys: &Bindings, commands: &[Command], separator: &str) -> String {
    commands
        .iter()
        .map(|command| keys.first(*command))
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

fn help_overlay(frame: &mut Frame, area: Rect, theme: &Theme, keys: &Bindings) {
    let rows: Vec<(String, &str)> = HELP
        .iter()
        .map(|(commands, what)| (every_key(keys, commands, " / "), *what))
        .filter(|(shown, _)| !shown.is_empty())
        .collect();
    // The key column is as wide as the widest binding, so a remap to `ctrl-pgdn` pushes
    // the descriptions over rather than running into them.
    let column = rows
        .iter()
        .map(|(shown, _)| Span::raw(shown).width())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(shown, what)| {
            let padding = " ".repeat(column - Span::raw(shown).width());
            Line::from(vec![
                theme.accent(format!(" {shown}{padding}  ")),
                theme.text(*what),
            ])
        })
        .collect();
    // And the box is as wide as the widest line it holds - the key column, the space
    // either side of it, the longest description and the two the border takes - rather
    // than a number chosen once and quietly outgrown by a description added later. A
    // popup that cuts its own last word off is worse than one that is a little wide.
    let widest = rows
        .iter()
        .map(|(_, what)| Span::raw(*what).width())
        .max()
        .unwrap_or(0);
    let popup = popup(area, (column + widest) as u16 + 5, lines.len() as u16 + 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(theme.bordered(true).title(theme.title(" Keys "))),
        popup,
    );
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

    // Every word a pointer can land on, gathered as the frame is drawn rather than
    // worked out again afterwards.
    let mut buttons: Vec<(Command, Rect)> = Vec::new();

    frame.render_widget(header(app), top);
    // The settings are a right-aligned title, so they sit on the border row itself,
    // inside it and hard against the right edge; the listing label is the paragraph's
    // own line, one row below. Both are reproduced here rather than guessed at, so that
    // a word answers exactly where it was printed.
    let bar = Rect {
        x: top.x.saturating_add(1),
        y: top.y,
        width: top.width.saturating_sub(2),
        height: top.height.min(1),
    };
    let labels = widths(&settings(app));
    let wide: u16 = labels.iter().map(|(_, width)| width).sum();
    let start = bar.right().saturating_sub(wide).max(bar.left());
    buttons.extend(mouse::lay_out(bar, start, &labels));
    buttons.extend(mouse::lay_out(
        top.inner(Margin::new(1, 1)),
        bar.left(),
        &widths(&listing(app)),
    ));

    // The queue comes off the bottom of the body before anything else is measured
    // against it, so the strip runs the whole width - it is a list of episodes from
    // wherever they were queued, not a thing about the series in the poster - and the
    // poster is shaped to the height the columns are actually left with.
    // As tall as the queue is showing, and never shorter than one row while there is a
    // queue at all: a narrowing that matched nothing needs somewhere to say so, and a
    // strip that vanished as its last row was hidden would read as a queue that had
    // emptied itself.
    let queued = if app.downloads.items.is_empty() {
        0
    } else {
        app.downloads.rows().max(1)
    };
    let queue = downloads_height(body, queued, focus == Focus::Downloads);
    let [body, queue_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(queue)]).areas(body);

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

    let shown = app.series.shown();
    let items: Vec<ListItem> = if shown.is_empty() {
        placeholder(
            &theme,
            app.series.loading,
            app.series.error.as_ref(),
            &nothing_shown(&app.series, "Nothing here."),
            tick,
        )
    } else {
        shown
            .iter()
            .map(|series| series_row(&theme, series))
            .collect()
    };
    let title = pane_title(column_name(app, Focus::Series), &app.series);
    let focused = focus == Focus::Series;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&theme, &title, focused))
            .highlight_style(theme.highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        left,
        &mut app.series.state,
    );

    let shown = app.seasons.shown();
    let items: Vec<ListItem> = if shown.is_empty() {
        placeholder(
            &theme,
            app.seasons.loading,
            app.seasons.error.as_ref(),
            &nothing_shown(&app.seasons, "Pick a series on the left."),
            tick,
        )
    } else {
        let series_title = app
            .series
            .selected()
            .map_or("", |series| series.title.as_str());
        shown
            .iter()
            .map(|season| season_row(&theme, season, series_title))
            .collect()
    };
    let title = pane_title(column_name(app, Focus::Seasons), &app.seasons);
    let focused = focus == Focus::Seasons;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&theme, &title, focused))
            .highlight_style(theme.highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        middle,
        &mut app.seasons.state,
    );

    let shown = app.episodes.shown();
    let items: Vec<ListItem> = if shown.is_empty() {
        placeholder(
            &theme,
            app.episodes.loading,
            app.episodes.error.as_ref(),
            &nothing_shown(&app.episodes, "Pick a season to list its episodes."),
            tick,
        )
    } else {
        let marked: Vec<bool> = shown
            .iter()
            .map(|episode| app.marked.contains(&episode.id))
            .collect();
        // Read off the rows that are drawn rather than off the set, so a mark carried
        // across a list that came back without its episode - or one on a row a
        // narrowing is hiding - does not open a gutter for a row that is not there. See
        // [`episode_row`].
        let gutter = marked.contains(&true);
        shown
            .iter()
            .zip(marked)
            .map(|(episode, marked)| {
                let held = app
                    .downloaded
                    .get(&episode.id)
                    .copied()
                    .unwrap_or(OnDisk::Missing);
                episode_row(
                    &theme,
                    episode,
                    app.playheads.get(&episode.id),
                    held,
                    gutter.then_some(marked),
                )
            })
            .collect()
    };
    let title = pane_title(column_name(app, Focus::Episodes), &app.episodes);
    let focused = focus == Focus::Episodes;
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&theme, &title, focused))
            .highlight_style(theme.highlight(focused))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        right,
        &mut app.episodes.state,
    );

    // Nothing at all while the queue is empty, which is what keeps the three columns
    // the size they were before any of this existed.
    if !queue_area.is_empty() {
        // Wide enough for the longest title in the queue, and never so wide that the
        // bars are pushed off a narrow terminal - a title cut short is a smaller loss
        // than the state of the download it belongs to.
        let shown = app.downloads.shown();
        let column = shown
            .iter()
            .map(|download| Span::raw(download.title.clone()).width())
            .max()
            .unwrap_or(0)
            .min(usize::from(queue_area.width / 3));
        let items: Vec<ListItem> = if shown.is_empty() {
            placeholder(
                &theme,
                false,
                None,
                &nothing_shown(&app.downloads, "Nothing queued."),
                tick,
            )
        } else {
            shown
                .iter()
                .map(|download| download_row(&theme, download, column))
                .collect()
        };
        let title = pane_title(column_name(app, Focus::Downloads), &app.downloads);
        let focused = focus == Focus::Downloads;
        frame.render_stateful_widget(
            List::new(items)
                .block(pane_block(&theme, &title, focused))
                .highlight_style(theme.highlight(focused))
                .highlight_symbol("› ")
                .highlight_spacing(HighlightSpacing::Always),
            queue_area,
            &mut app.downloads.state,
        );
    }

    if details_height > 0 {
        let (name, mut lines) = details(app);
        // A card with nothing under the cursor to describe says so, rather than standing
        // empty and looking like something failed to arrive.
        if lines.is_empty() {
            lines.push(Line::from(theme.dim("Nothing selected yet.")));
        }
        // A cell of room either side, so the words sit inside the card rather than
        // against its edges.
        let block = theme
            .bordered(false)
            .padding(Padding::horizontal(1))
            .title(theme.heading(format!(" {} {name} ", glyph(name))));
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

        if let Some(url) = still
            && !app.art.draw(frame, still_area, &url)
        {
            frame.render_widget(waiting(&theme), middle_row(still_area));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), text);
    }

    // The counts along the right take what they need and the sentence gets the rest,
    // so a long notice runs into the edge of its own half rather than over the counts.
    let counts = activity(app);
    let [said, counted] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(u16::try_from(counts.width()).unwrap_or(u16::MAX)),
    ])
    .areas(status);
    frame.render_widget(Paragraph::new(status_line(app)), said);
    frame.render_widget(Paragraph::new(counts), counted);

    let reminder = footer(app);
    frame.render_widget(Paragraph::new(line(&reminder)), keys);
    buttons.extend(mouse::lay_out(keys, keys.x, &widths(&reminder)));

    if app.show_help {
        help_overlay(frame, area, &theme, &app.keys);
    }

    // Read what is in use before the list borrows the app to draw itself.
    let current = app.picker.as_ref().map(|picker| app.chosen(picker.kind));
    let mut picked = Rect::default();
    if let (Some(current), Some(picker)) = (current, app.picker.as_mut()) {
        picked = picker_overlay(frame, area, &theme, &app.keys, picker, &current, tick);
    }

    // Everything a pointer can land on, as the frame about to be shown laid it out.
    // Written last, when every borrow the drawing took is over - and read by the next
    // event, which cannot arrive before this frame is on screen, because the loop draws
    // and only then polls.
    app.regions = Regions {
        area,
        series: left,
        seasons: middle,
        episodes: right,
        downloads: queue_area,
        picker: picked,
        buttons,
    };
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::{Rect, Size};
    use ratatui::style::Color;
    use ratatui::text::Span;

    use image::{DynamicImage, Rgb, RgbImage};

    use crate::api::Page;
    use crate::download::{DownloadOptions, OnDisk};
    use crate::model::{
        Artwork, CatalogItem, Images, MovieListingMetadata, Playhead, Season, SeasonEpisode,
        SeriesMetadata,
    };
    use crate::tui::app::{Action, App, Focus, Picking};
    use crate::tui::art::Gallery;
    use crate::tui::keys::{self, Bindings, Command};
    use crate::tui::theme::{self, Theme};
    use crate::tui::worker::{Choice, FilterKind, Filters, Listing, Request, Response, Worker};

    use super::{
        HELP, MARK, SPINNER, cells, downloads_height, draw, duration, meter, poster_width,
        thumbnail_width,
    };
    use crate::tui::app::State;
    use crate::tui::worker::Update;

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
            start_at: None,
            playhead: None,
            reporter: None,
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
            ..CatalogItem::default()
        }]);
        app.seasons.set(vec![Season {
            id: "S1".to_owned(),
            season_number: 1,
            title: "Frieren".to_owned(),
            number_of_episodes: 28,
            audio_locales: vec!["ja-JP".to_owned(), "en-US".to_owned(), "fr-FR".to_owned()],
            subtitle_locales: vec!["en-US".to_owned(), "fr-FR".to_owned()],
            ..Season::default()
        }]);
        app.episodes.set(vec![
            SeasonEpisode {
                id: "E1".to_owned(),
                episode: "1".to_owned(),
                episode_number: 1,
                season_number: 1,
                series_title: "Frieren".to_owned(),
                title: "The Journey Ends".to_owned(),
                duration_ms: 1_461_000,
                images: Images {
                    thumbnail: artwork(STILL, 320),
                    ..Images::default()
                },
                ..SeasonEpisode::default()
            },
            SeasonEpisode {
                id: "E2".to_owned(),
                episode: "2".to_owned(),
                episode_number: 2,
                season_number: 1,
                series_title: "Frieren".to_owned(),
                title: "The Priest's Lie".to_owned(),
                duration_ms: 1_420_000,
                ..SeasonEpisode::default()
            },
        ]);
        app
    }

    /// What the playheads endpoint has to say about one episode.
    fn playhead(content_id: &str, seconds: u32, fully_watched: bool) -> Playhead {
        Playhead {
            content_id: content_id.to_owned(),
            playhead: seconds,
            fully_watched,
        }
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
            "crunchyroll-tui",
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

    /// The header is a title bar: the program's name in the corner, what is listed and
    /// how much of it underneath, and the three settings along the right as one line of
    /// labelled values held apart the way the facts in the details panel are.
    #[test]
    fn the_header_reads_as_a_title_bar() {
        let mut app = app();
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains(" crunchyroll-tui "),
            "no name in the corner"
        );
        assert!(screen.contains("Continue watching \u{b7} 1 series"));
        assert!(screen.contains("subs English \u{b7} video 1080p"));

        // While the box is open it says which two keys mean something in it, since
        // neither of them is on the line along the bottom.
        press(&mut app, KeyCode::Char('/'));
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("Search: "));
        assert!(screen.contains("\u{23ce} search \u{b7} esc cancel"));
        press(&mut app, KeyCode::Esc);
        app.focus = Focus::Episodes;
        press(&mut app, KeyCode::Char('f'));
        assert!(rendered(120, 30, &mut app).contains("\u{23ce} keep \u{b7} esc clear"));
    }

    /// Each column wears its own mark in front of its name, and only the column with the
    /// keyboard is framed in the accent - which is what finds it on a screen of four
    /// boxes before a word of any title has been read.
    #[test]
    fn the_focused_column_is_the_one_framed_in_colour() {
        let mut app = app();
        let buffer = buffer(120, 30, &mut app);
        let screen: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        for expected in ["\u{25a4} Series", "\u{25eb} Seasons", "\u{25b8} Episodes"] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }

        let corner = |area: Rect| &buffer[(area.x, area.y)];
        let series = corner(app.regions.series);
        let episodes = corner(app.regions.episodes);
        assert_eq!(series.symbol(), "\u{256d}", "the box is not rounded");
        assert_eq!(
            series.fg,
            Color::Yellow,
            "the focused column is not accented"
        );
        assert_eq!(episodes.fg, Color::DarkGray, "an idle column is accented");
    }

    /// A film keeps the three columns and changes the words in them. Four places would
    /// otherwise be saying series things about it: the catalogue row, where `2 seasons`
    /// becomes what the thing is; the middle column's title and its one row, which is not
    /// a season and must not be numbered as one; the number slot in the episodes column,
    /// where `E1` is a number nobody gave the film; and the panel, which has a running
    /// time to show where a series has a count of episodes.
    #[test]
    fn a_film_reads_as_a_film_in_every_column() {
        let mut app = app();
        app.series.set(vec![CatalogItem {
            id: "GM5V7XW1Q".to_owned(),
            kind: "movie_listing".to_owned(),
            title: "Suzume".to_owned(),
            description: "A door opens.".to_owned(),
            movie_listing_metadata: MovieListingMetadata {
                movie_release_year: 2022,
                duration_ms: 7_212_000,
                maturity_ratings: vec!["PG-13".to_owned()],
                ..MovieListingMetadata::default()
            },
            ..CatalogItem::default()
        }]);
        app.seasons.set(vec![Season {
            id: "GM5V7XW1Q".to_owned(),
            kind: "movie_listing".to_owned(),
            title: "Suzume".to_owned(),
            ..Season::default()
        }]);
        app.episodes.set(vec![SeasonEpisode {
            id: "GY8DVXWZ1".to_owned(),
            kind: "movie".to_owned(),
            season_number: 1,
            episode_number: 1,
            series_title: "Suzume".to_owned(),
            title: "Suzume".to_owned(),
            duration_ms: 7_212_000,
            ..SeasonEpisode::default()
        }]);

        let screen = rendered(120, 30, &mut app);
        for expected in [
            "Suzume  film",
            " Film ",
            "Film · 2022 · 2:00:12 · PG-13",
            "A door opens.",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
        assert!(
            !screen.contains("Season 0"),
            "the middle column numbered a film as a season"
        );

        // And the panel, which prints the number of whatever the cursor is on, says what
        // the row is instead of giving the film a season and an episode.
        app.focus = Focus::Episodes;
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("Film · 2:00:12"),
            "the panel numbered the film"
        );
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

    /// The popup is the only place the keys are written down, so it has to name every
    /// command - a new one cannot be added without a line here - and it has to name the
    /// keys the config in front of it asked for rather than the ones that shipped.
    #[test]
    fn the_help_popup_lists_every_command_as_it_is_bound() {
        let listed: Vec<Command> = HELP
            .iter()
            .flat_map(|(commands, _)| commands.iter().copied())
            .collect();
        for (command, _) in keys::DEFAULTS {
            assert!(
                listed.contains(&command),
                "{} is on no line of the help popup",
                command.name()
            );
        }

        let (bindings, warnings) =
            toml::from_str::<keys::Settings>("down = \"e\"\nquit = \"ctrl-q\"\n")
                .expect("valid config")
                .resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut app = app();
        app.keys = bindings;
        app.show_help = true;
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("↑ k / e"),
            "the popup still offers j for down"
        );
        assert!(
            screen.contains("ctrl-q"),
            "the popup still offers q for quit"
        );
        // And the reminder along the bottom edge, which is not covered by the popup.
        assert!(
            screen.contains("ctrl-q quit"),
            "the bottom edge still offers q for quit"
        );
        // The box grows to what it holds, so a line added to the table is not a line
        // with its last word cut off.
        for (_, what) in HELP {
            assert!(
                screen.contains(what),
                "the popup cut {what:?} off at its own edge"
            );
        }
    }

    /// A remapped key has to do the thing it was remapped to, not only be advertised.
    #[test]
    fn a_remapped_key_moves_the_cursor() {
        let (bindings, warnings) = toml::from_str::<keys::Settings>("down = \"e\"\n")
            .expect("valid config")
            .resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut app = app();
        app.keys = bindings;
        app.focus = Focus::Episodes;
        app.episodes.set(vec![
            app.episodes.items[0].clone(),
            SeasonEpisode {
                id: "E2".to_owned(),
                episode: "2".to_owned(),
                episode_number: 2,
                season_number: 1,
                title: "The Mage Who Sealed the Demon King".to_owned(),
                ..SeasonEpisode::default()
            },
        ]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.episodes.state.selected(), Some(0), "j is not bound");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.episodes.state.selected(), Some(1));
    }

    /// The catalogue label is a button and what it does is move on to the next list, so
    /// a pointer has to be able to walk the whole ring - the account's own lists
    /// included - and come back round to where it started. A list that is drawn but not
    /// clickable would be one the mouse could enter and never leave. The walk starts
    /// wherever the interface opens, which is the point: every stop has to lead on.
    #[test]
    fn the_listing_label_walks_the_whole_ring() {
        let mut app = app();
        let mut labels = Vec::new();
        for _ in 0..Listing::sources().len() + 1 {
            labels.push(app.listing.label());
            let _ = buffer(120, 30, &mut app);
            let (x, y) = middle(button(&app, Command::Order));
            click(&mut app, x, y);
        }
        assert_eq!(
            labels,
            [
                "Continue watching",
                "Popular",
                "Recently added",
                "A to Z",
                "Watchlist",
                "Continue watching"
            ]
        );
    }

    /// A search is a detour off the ring, and leaving one has to come back to the order
    /// that was in use when it began rather than to the top of the catalogue: someone
    /// who went looking from A to Z did not ask to be put back on Popular.
    #[test]
    fn leaving_a_search_comes_back_to_the_order_in_use() {
        let mut app = app();
        // Off the opening list and one step along the browse orders, so that coming back
        // to the top of the catalogue would be visibly the wrong answer.
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.listing.label(), "Recently added");
        press(&mut app, KeyCode::Char('/'));
        for letter in "frieren".chars() {
            press(&mut app, KeyCode::Char(letter));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.listing.label(), "Search: frieren");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.listing.label(), "Recently added");
    }

    /// The watchlist is a thing about a series, and the seasons and the episodes on
    /// screen are that series' own - so the key means the same series from any of the
    /// three columns, rather than doing nothing in two of them.
    #[test]
    fn the_watchlist_key_takes_the_series_from_any_column() {
        let mut app = app();
        for focus in [Focus::Series, Focus::Seasons, Focus::Episodes] {
            app.focus = focus;
            // The catalogue the interface asks for as it opens is not what is being
            // asked about here.
            app.sent();
            press(&mut app, KeyCode::Char('w'));
            assert_eq!(
                app.sent(),
                vec![Request::Watchlist {
                    series_id: "GY5P48XEY".to_owned(),
                    series_title: "Frieren".to_owned(),
                }],
                "from the {focus:?} column"
            );
        }
    }

    /// And says so rather than going quiet when the catalogue has nothing selected -
    /// while the first page is still on its way, or after a search that found nothing.
    #[test]
    fn the_watchlist_key_needs_a_series() {
        let mut app = app();
        app.series.clear();
        app.sent();
        press(&mut app, KeyCode::Char('w'));
        assert!(
            app.sent().is_empty(),
            "asked Crunchyroll about a series nobody picked"
        );
        assert!(rendered(120, 30, &mut app).contains("Pick a series first."));
    }

    /// Marking watched is about the episode under the cursor: the playhead goes to the
    /// episode's own running time in whole seconds, which is what Crunchyroll counts as
    /// watched, and back to zero to undo it. The notice needs the episode's number as
    /// well, since the cursor may have moved on by the time the answer arrives.
    #[test]
    fn marking_an_episode_moves_its_playhead_to_the_end_and_back() {
        let mut app = app();
        app.focus = Focus::Episodes;
        app.episodes.set(vec![SeasonEpisode {
            id: "GZ7UV8KWZ".to_owned(),
            episode: "4".to_owned(),
            episode_number: 4,
            season_number: 1,
            title: "The Land Where Souls Rest".to_owned(),
            duration_ms: 1_461_999,
            ..SeasonEpisode::default()
        }]);
        app.sent();

        press(&mut app, KeyCode::Char('m'));
        assert_eq!(
            app.sent(),
            vec![Request::Playhead {
                episode_id: "GZ7UV8KWZ".to_owned(),
                label: "E4".to_owned(),
                seconds: 1461,
            }]
        );

        press(&mut app, KeyCode::Char('M'));
        assert_eq!(
            app.sent(),
            vec![Request::Playhead {
                episode_id: "GZ7UV8KWZ".to_owned(),
                label: "E4".to_owned(),
                seconds: 0,
            }]
        );
    }

    /// Some episodes come with no running time at all, and the playhead that marks one
    /// watched is its running time. Sending the zero would put the playhead at the start,
    /// which is what unwatched means - so `m` would quietly do what `M` does. It says
    /// there is nothing to aim at instead, and `M` still works, since zero is where it
    /// was going anyway.
    #[test]
    fn an_episode_with_no_running_time_cannot_be_marked_watched() {
        let mut app = app();
        app.focus = Focus::Episodes;
        app.episodes.set(vec![SeasonEpisode {
            id: "GZ7UV8KWZ".to_owned(),
            episode: "4".to_owned(),
            episode_number: 4,
            duration_ms: 0,
            ..SeasonEpisode::default()
        }]);
        app.sent();

        press(&mut app, KeyCode::Char('m'));
        assert!(
            app.sent().is_empty(),
            "marked an episode watched by putting its playhead back to the start"
        );
        assert!(rendered(120, 30, &mut app).contains("no running time"));

        // Unmarking one is still the same request it always was.
        press(&mut app, KeyCode::Char('M'));
        assert_eq!(
            app.sent(),
            vec![Request::Playhead {
                episode_id: "GZ7UV8KWZ".to_owned(),
                label: "E4".to_owned(),
                seconds: 0,
            }]
        );
    }

    /// With no season open there is no episode to mark, whatever the other two columns
    /// hold - so it asks for one in the same words `play` and `download` use.
    #[test]
    fn marking_watched_needs_an_open_season() {
        let mut app = app();
        app.episodes.clear();
        app.focus = Focus::Series;
        app.sent();
        press(&mut app, KeyCode::Char('m'));
        assert!(
            app.sent().is_empty(),
            "moved the playhead of an episode nobody picked"
        );
        assert!(rendered(120, 30, &mut app).contains("Open a season first."));
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
        assert!(
            app.picker
                .as_ref()
                .is_some_and(|picker| picker.kind == Picking::Audio)
        );
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

    /// The rows Crunchyroll answers a request for the categories with, once the worker
    /// has turned them into something a list can show.
    fn genres() -> Vec<Choice> {
        [("action", "Action"), ("comedy", "Comedy")]
            .iter()
            .map(|(value, label)| Choice {
                value: (*value).to_owned(),
                label: (*label).to_owned(),
            })
            .collect()
    }

    /// Every catalogue page the interface has asked for since it was last asked, and what
    /// it wanted each one narrowed to.
    fn catalogue_asked(app: &App) -> Vec<(Listing, Filters)> {
        app.sent()
            .into_iter()
            .filter_map(|request| match request {
                Request::Catalog {
                    listing, filters, ..
                } => Some((listing, filters)),
                _ => None,
            })
            .collect()
    }

    /// The genres are Crunchyroll's to name, so the list is fetched rather than written
    /// down here - and the popup is up the moment the key is pressed, holding the one row
    /// that is ours, rather than the key doing nothing visible while a request is out.
    /// The answer is then kept for the run: the categories change a few times a year,
    /// which is not between two presses of a key.
    #[test]
    fn the_genre_list_comes_off_the_network_and_is_asked_for_once() {
        let mut app = app();
        // The catalogue the interface asks for as it opens is not what is being asked
        // about here.
        app.sent();
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.sent(), vec![Request::FilterValues(FilterKind::Genre)]);
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("Genre"),
            "the popup is up before the answer is"
        );
        assert!(screen.contains("All"));
        assert!(
            screen.contains(SPINNER[0]),
            "a list still waiting for its rows says so along its bottom edge"
        );

        app.accept(Response::FilterValues {
            which: FilterKind::Genre,
            result: Ok(genres()),
        });
        let screen = rendered(120, 30, &mut app);
        for expected in ["All", "Action", "action", "Comedy"] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }

        press(&mut app, KeyCode::Esc);
        assert!(app.picker.is_none(), "esc closes the list");
        app.sent();
        press(&mut app, KeyCode::Char('c'));
        assert!(
            app.sent().is_empty(),
            "the list was asked for a second time"
        );
        assert!(rendered(120, 30, &mut app).contains("Action"));
    }

    /// Choosing a genre is a different catalogue rather than the same one with rows
    /// hidden, so the list is asked for again from the start - narrowed - and the status
    /// line says what is now on screen, since the column changing under the cursor
    /// without a word would read as a glitch.
    #[test]
    fn choosing_a_genre_asks_the_catalogue_again_and_says_what_is_showing() {
        let mut app = app();
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.listing, Listing::Browse(0));
        press(&mut app, KeyCode::Char('c'));
        app.accept(Response::FilterValues {
            which: FilterKind::Genre,
            result: Ok(genres()),
        });
        app.sent();

        // All is the row the list opens on while nothing is chosen, so one step down is
        // the first genre.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.picker.is_none(), "choosing closes the list");
        let asked = catalogue_asked(&app);
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(asked[0].0, Listing::Browse(0));
        assert_eq!(
            asked[0]
                .1
                .chosen(FilterKind::Genre)
                .map(|genre| genre.value.as_str()),
            Some("action"),
            "the catalogue was asked for again without the genre"
        );
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("Showing Popular · Genre: Action."));
    }

    /// Taking a filter off has to be as easy as putting one on, and there is no second
    /// key for it: All sits at the top of the same list, where the eye lands. The list
    /// opens on what is in force, so a filter that is set is not one All is a keypress
    /// away from by accident.
    #[test]
    fn all_at_the_top_of_the_list_takes_the_filter_off_again() {
        let mut app = app();
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('c'));
        app.accept(Response::FilterValues {
            which: FilterKind::Genre,
            result: Ok(genres()),
        });
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.filters.genre.is_some());
        app.sent();

        press(&mut app, KeyCode::Char('c'));
        assert_eq!(
            app.picker
                .as_ref()
                .and_then(|picker| picker.pane.state.selected()),
            Some(1),
            "the list opens on the genre in force rather than at the top"
        );
        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Enter);
        assert!(app.filters.genre.is_none(), "All did not clear the genre");
        assert!(!app.filters.any());
        assert_eq!(
            catalogue_asked(&app),
            vec![(Listing::Browse(0), Filters::default())]
        );
        assert!(rendered(120, 30, &mut app).contains("Showing Popular."));
    }

    /// A filter narrows the browse listings and nothing else, so one set while the
    /// watchlist or the history is up would otherwise be a word in the header about a
    /// list it has nothing to do with. The column goes back to the catalogue instead and
    /// the status line says why - the one thing it must not do is take the key and look
    /// as though nothing happened.
    #[test]
    fn a_filter_set_on_a_list_it_cannot_narrow_brings_the_catalogue_back() {
        let mut app = app();
        assert_eq!(app.listing, Listing::History, "the interface opens on this");
        app.sent();
        press(&mut app, KeyCode::Char('u'));
        assert!(app.filters.simulcast);
        assert_eq!(app.listing, Listing::Browse(0));
        let asked = catalogue_asked(&app);
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(asked[0].0, Listing::Browse(0));
        assert!(asked[0].1.simulcast);
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("Showing Popular · Simulcast - the filters narrow the catalogue only."),
            "the column moved without saying so"
        );

        // And the same key again takes it off, with the column already where it belongs.
        press(&mut app, KeyCode::Char('u'));
        assert!(!app.filters.simulcast);
        assert!(rendered(120, 30, &mut app).contains("Showing Popular."));
    }

    /// The header describes what is under it, so the filters show there while a browse
    /// listing is on screen and not while one of the account's own lists is. Each word
    /// runs the list it was chosen from, which is where it is cleared as well as where it
    /// was set - and a filter out of force is out of force rather than forgotten, which
    /// is what the status line says on the way off the catalogue.
    #[test]
    fn the_header_names_the_filters_that_are_on_and_they_answer_to_a_click() {
        let mut app = app();
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('u'));
        press(&mut app, KeyCode::Char('c'));
        app.accept(Response::FilterValues {
            which: FilterKind::Genre,
            result: Ok(genres()),
        });
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("Genre: Action"));
        assert!(screen.contains("Simulcast"));

        let (x, y) = middle(button(&app, Command::Genre));
        click(&mut app, x, y);
        assert!(
            app.picker
                .as_ref()
                .is_some_and(|picker| picker.kind == Picking::Filter(FilterKind::Genre)),
            "the word in the header opens the list it came from"
        );
        press(&mut app, KeyCode::Esc);

        // Round the ring to the watchlist, which no filter can narrow.
        for _ in 0..3 {
            press(&mut app, KeyCode::Char('o'));
        }
        assert_eq!(app.listing, Listing::Watchlist);
        let screen = rendered(120, 30, &mut app);
        assert!(
            !screen.contains("Genre: Action"),
            "the header is describing a list the filters do not touch"
        );
        assert!(screen.contains("Watchlist - the filters narrow the catalogue only."));
        assert!(app.filters.any(), "out of force is not forgotten");

        // And on again to the catalogue, where they are in force and shown once more.
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.listing, Listing::Browse(0));
        assert!(rendered(120, 30, &mut app).contains("Genre: Action"));
    }

    /// The filters are half of what was asked for, so they are half of what makes an
    /// answer stale: a page of the whole catalogue that was already on its way when the
    /// genre was chosen is the answer to a question nobody is asking any more, and
    /// letting it into the column would leave the header describing a narrowed catalogue
    /// over a hundred series that are not.
    #[test]
    fn a_page_asked_for_before_the_filter_changed_is_dropped() {
        let mut app = app();
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('c'));
        app.accept(Response::FilterValues {
            which: FilterKind::Genre,
            result: Ok(genres()),
        });
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);

        let page = vec![CatalogItem {
            id: "GY8VEQ95Y".to_owned(),
            kind: "series".to_owned(),
            title: "Dandadan".to_owned(),
            ..CatalogItem::default()
        }];
        app.accept(Response::Catalog {
            listing: Listing::Browse(0),
            start: 0,
            filters: Filters::default(),
            result: Ok(Page {
                items: page.clone(),
                total: None,
                next: None,
            }),
        });
        assert!(
            app.series.items.is_empty(),
            "the unfiltered page landed in a filtered column"
        );

        app.accept(Response::Catalog {
            listing: Listing::Browse(0),
            start: 0,
            filters: app.filters.clone(),
            result: Ok(Page {
                items: page,
                total: None,
                next: None,
            }),
        });
        assert_eq!(app.series.items.len(), 1, "and the right answer is taken");
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

    /// An episode half watched on the phone is the one thing worth knowing while looking
    /// down a season, and it has to be readable without counting columns: the time a row
    /// shows is the time playing it would open at, and a check is an episode with nothing
    /// left to go back to.
    #[test]
    fn says_where_an_episode_was_left_off() {
        let mut app = app();
        app.playheads = [
            ("E1".to_owned(), playhead("E1", 842, false)),
            ("E2".to_owned(), playhead("E2", 1_410, true)),
        ]
        .into_iter()
        .collect();
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("14:02"), "the position to resume E1 from");
        assert!(screen.contains("\u{2713}"), "E2 has been watched");
        assert!(
            !screen.contains("24:21"),
            "the marker takes the running time's place rather than a column of its own"
        );

        // A position in the opening seconds is not a place to be sent back to, and the
        // row says exactly what playing it would do: nothing.
        app.playheads = [("E1".to_owned(), playhead("E1", 8, false))]
            .into_iter()
            .collect();
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("24:21"), "so the running time is back");
    }

    /// A finished episode steps back rather than disappearing: its title goes dim, so
    /// what is left of a season is what the eye lands on, and the check stands out in
    /// its place so the row still says why.
    #[test]
    fn a_watched_episode_steps_back() {
        let mut app = app();
        app.playheads = [("E2".to_owned(), playhead("E2", 1_410, true))]
            .into_iter()
            .collect();
        let drawn = buffer(120, 30, &mut app);
        let first_cell_of = |text: &str| {
            let area = drawn.area;
            (area.top()..area.bottom())
                .find_map(|y| {
                    let line: String = (area.left()..area.right())
                        .map(|x| drawn[(x, y)].symbol())
                        .collect();
                    // Every cell up to the word is one column: nothing wide is drawn
                    // in the episodes column in front of a title.
                    line.find(text).map(|at| {
                        let x = line[..at].chars().count() as u16;
                        drawn[(x, y)].clone()
                    })
                })
                .unwrap_or_else(|| panic!("{text:?} was not drawn"))
        };
        assert_eq!(first_cell_of("The Priest's Lie").fg, Color::DarkGray);
        assert_eq!(first_cell_of("\u{2713}").fg, Color::Yellow);
        assert_ne!(
            first_cell_of("The Journey Ends").fg,
            Color::DarkGray,
            "an episode nobody has watched went dim"
        );
    }

    /// A series that is simulcasting says so in its row, and a film says what it is,
    /// both in the accent among the dim words, since either changes what someone does
    /// next.
    #[test]
    fn a_catalogue_row_names_what_stands_out() {
        let mut app = app();
        app.series.items[0].series_metadata.is_simulcast = true;
        app.series.items[0].series_metadata.is_dubbed = true;
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("Frieren  dub \u{b7} simulcast"));
    }

    /// The status line says what kind of thing it is saying before it says it: a cross
    /// for a failure, a spinner while something is on its way, a check otherwise - and it
    /// counts along its right end what the user has set in motion.
    #[test]
    fn the_status_line_marks_what_kind_of_news_it_is() {
        let mut app = app();
        // The opening list is still out, since nothing here answers it.
        assert!(
            rendered(120, 30, &mut app)
                .contains(&format!(" {} Loading the catalogue\u{2026}", SPINNER[0]))
        );
        app.paging.asked = None;
        assert!(rendered(120, 30, &mut app).contains(" \u{2713} Ready."));

        app.series.items.clear();
        press(&mut app, KeyCode::Char('w'));
        assert!(rendered(120, 30, &mut app).contains(" \u{2717} Pick a series first."));

        app.notice = None;
        app.episodes.loading = true;
        assert!(
            rendered(120, 30, &mut app)
                .contains(&format!(" {} Loading episodes\u{2026}", SPINNER[0]))
        );
        app.episodes.loading = false;

        let mut app = self::app();
        app.marked.insert("E1".to_owned());
        with_downloads(&mut app, 3);
        app.downloads.items[0].state = State::Running;
        app.downloads.items[2].state = State::Failed("no".to_owned());
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains(&format!(
                "{MARK} 1 marked \u{b7} \u{21e3} 1 downloading \u{b7} 1 queued \u{b7} 1 failed"
            )),
            "the status line kept count of nothing"
        );
    }

    /// Each hint along the bottom is a key in the accent and what it does in dim, which is
    /// what picks the keys out of the line.
    #[test]
    fn the_footer_sets_the_keys_apart_from_their_words() {
        let mut app = app();
        let drawn = buffer(120, 30, &mut app);
        let quit = button(&app, Command::Quit);
        assert_eq!(drawn[(quit.x, quit.y)].symbol(), "q");
        assert_eq!(drawn[(quit.x, quit.y)].fg, Color::Yellow);
        assert_eq!(drawn[(quit.x + 2, quit.y)].fg, Color::DarkGray);
    }

    /// The panel is a card named after what it describes, and for an episode it says in
    /// words what the row can only say in a glyph and four cells: where it was left off
    /// and whether it is on the disk.
    #[test]
    fn the_details_card_is_named_after_what_it_describes() {
        let mut app = app();
        app.focus = Focus::Episodes;
        app.playheads = [("E1".to_owned(), playhead("E1", 842, false))]
            .into_iter()
            .collect();
        app.downloaded = [("E1".to_owned(), OnDisk::Complete)].into_iter().collect();
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("\u{25b8} Episode "),
            "the card is not named"
        );
        assert!(
            screen.contains("S1E1 \u{b7} 24:21 \u{b7} resume at 14:02 \u{b7} on disk"),
            "the card kept the row's glyphs to itself"
        );

        app.playheads = [("E1".to_owned(), playhead("E1", 1_461, true))]
            .into_iter()
            .collect();
        app.downloaded.clear();
        assert!(rendered(120, 30, &mut app).contains("S1E1 \u{b7} 24:21 \u{b7} watched"));

        with_downloads(&mut app, 1);
        app.focus = Focus::Downloads;
        assert!(rendered(120, 30, &mut app).contains("\u{21e3} Download "));

        // And a card with nothing to describe says so rather than standing empty.
        app.series.clear();
        app.focus = Focus::Series;
        assert!(rendered(120, 30, &mut app).contains("Nothing selected yet."));
    }

    /// A mark is a decision the user made about a row, so it has to be visible on that
    /// row - and the column it needs has to be there only while a mark is on one of them,
    /// since the width of this column is what the titles are living on.
    #[test]
    fn a_marked_episode_carries_a_bar_and_an_unmarked_season_carries_no_column() {
        let mut app = app();
        let plain = rendered(120, 30, &mut app);
        assert!(
            !plain.contains(MARK),
            "a season with nothing marked drew the column anyway"
        );
        // The cursor is on E1, so its row is the arrow and then the number.
        assert!(plain.contains("\u{203a} E1"));

        app.marked.insert("E2".to_owned());
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains(&format!("{MARK}E2")),
            "the marked episode is not wearing its mark"
        );
        assert!(
            screen.contains("\u{203a}  E1"),
            "the unmarked rows did not move over with the marked one"
        );

        // And the column goes again with the last mark, rather than standing empty for
        // the rest of the season.
        app.marked.clear();
        assert_eq!(rendered(120, 30, &mut app), plain);
    }

    /// The three things a row can say about an episode are three different things, and
    /// the row has to be able to say all of them at once: an episode can be half on the
    /// disk, half watched, and marked for downloading again, and each of those is read
    /// off a different part of the row. The mark and the disk marker in particular are
    /// two markers a few cells apart, so neither may be mistaken for the other.
    #[test]
    fn a_mark_and_a_disk_marker_are_read_apart_on_one_row() {
        let mut app = app();
        app.downloaded = [
            ("E1".to_owned(), OnDisk::Complete),
            ("E2".to_owned(), OnDisk::Partial),
        ]
        .into_iter()
        .collect();
        app.playheads = [("E2".to_owned(), playhead("E2", 842, false))]
            .into_iter()
            .collect();
        app.marked.insert("E2".to_owned());

        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains(&format!("{MARK}E2  \u{25d0}")),
            "the mark and what is on the disk ran into one another"
        );
        assert!(
            screen.contains("14:02"),
            "and neither of them took the playhead's slot"
        );
        // E1 is on the disk and not marked, so it shows the circle and an empty gutter.
        assert!(screen.contains("\u{203a}  E1  \u{25cf}"));
    }

    /// A season already sitting on the disk looked exactly like one that was not, and
    /// starting the download again was the only way to find out which was which. The
    /// marker says it in the column, beside what the account has watched rather than
    /// instead of it: an episode can be downloaded and never watched, or watched on the
    /// phone and never downloaded, and a row has to be able to say both at once.
    #[test]
    fn says_which_episodes_are_already_on_the_disk() {
        let mut app = app();
        app.downloaded = [
            ("E1".to_owned(), OnDisk::Complete),
            ("E2".to_owned(), OnDisk::Partial),
        ]
        .into_iter()
        .collect();
        app.playheads = [("E1".to_owned(), playhead("E1", 842, false))]
            .into_iter()
            .collect();
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("E1  \u{25cf}"), "E1 is here in full");
        assert!(
            screen.contains("E2  \u{25d0}"),
            "and a cut-off download left half of E2"
        );
        assert!(
            screen.contains("14:02"),
            "the download marker was drawn in the playhead's slot instead of its own"
        );

        // An episode with no file spends the same two cells on nothing, so a column with
        // one downloaded episode in it does not have its titles stepping in and out.
        app.downloaded.clear();
        let screen = rendered(120, 30, &mut app);
        assert!(
            !screen.contains('\u{25cf}') && !screen.contains('\u{25d0}'),
            "a marker was drawn for an episode that is not here"
        );
        assert!(
            screen.contains("E1     14:02"),
            "the columns moved when the marker went"
        );
    }

    /// An answer is only worth painting onto the column it was asked about. The lists are
    /// fetched one after another and a slow one comes back after the user has moved on,
    /// so an answer for a season that is no longer open has to be dropped rather than
    /// marking whichever rows happen to be there now.
    #[test]
    fn a_playhead_for_a_season_nobody_is_looking_at_is_dropped() {
        let mut app = app();
        app.episodes.owner = "S1".to_owned();
        app.accept(Response::Playheads {
            season_id: "S2".to_owned(),
            result: Ok(vec![playhead("E1", 842, false)]),
        });
        assert!(
            app.playheads.is_empty(),
            "a season the user has left painted this one's rows"
        );

        app.accept(Response::Playheads {
            season_id: "S1".to_owned(),
            result: Ok(vec![playhead("E1", 842, false)]),
        });
        assert_eq!(app.playheads.len(), 1);
        assert!(rendered(120, 30, &mut app).contains("14:02"));

        // And an answer that never came is a column drawn the way it always was, rather
        // than an error in front of the episode titles.
        app.accept(Response::Playheads {
            season_id: "S1".to_owned(),
            result: Err("Crunchyroll said no".to_owned()),
        });
        assert!(app.notice.is_none());
        assert_eq!(app.playheads.len(), 1, "and nothing was thrown away");
    }

    /// Every panel is optional except the columns, so a terminal too short for the
    /// details or too narrow for the help popup still has to draw rather than panic.
    #[test]
    fn survives_a_cramped_terminal() {
        /// A box nobody can see is worse than no box: a click would land on it and the
        /// interface would answer for a word that was never printed.
        fn every_box_is_on_screen(app: &App, width: u16, height: u16) {
            let screen = Rect::new(0, 0, width, height);
            for (command, area) in &app.regions.buttons {
                assert!(
                    !area.is_empty() && area.intersection(screen) == *area,
                    "{} was given a box off the edge of a {width}x{height} screen",
                    command.name()
                );
            }
        }

        let mut illustrated = illustrated();
        for (width, height) in [(120, 14), (40, 10), (20, 6), (8, 4), (1, 1)] {
            let screen = rendered(width, height, &mut illustrated);
            assert!(!screen.is_empty());
            every_box_is_on_screen(&illustrated, width, height);
        }

        let mut app = app();
        for (width, height) in [(120, 14), (40, 10), (20, 6), (8, 4)] {
            let screen = rendered(width, height, &mut app);
            assert!(!screen.is_empty());
            every_box_is_on_screen(&app, width, height);
        }
        app.show_help = true;
        for (width, height) in [(120, 30), (20, 6), (8, 4)] {
            let screen = rendered(width, height, &mut app);
            assert!(!screen.is_empty());
            every_box_is_on_screen(&app, width, height);
        }
        app.show_help = false;
        press(&mut app, KeyCode::Char('a'));
        for (width, height) in [(120, 30), (20, 6), (8, 4)] {
            let screen = rendered(width, height, &mut app);
            assert!(!screen.is_empty());
            every_box_is_on_screen(&app, width, height);
        }
    }

    /// A pointer report, as crossterm hands one over.
    fn pointer(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(app: &mut App, x: u16, y: u16) -> Action {
        app.on_mouse(pointer(MouseEventKind::Down(MouseButton::Left), x, y))
    }

    fn wheel(app: &mut App, x: u16, y: u16, down: bool) {
        let kind = if down {
            MouseEventKind::ScrollDown
        } else {
            MouseEventKind::ScrollUp
        };
        app.on_mouse(pointer(kind, x, y));
    }

    /// Where a word along an edge was drawn, by the command it runs.
    fn button(app: &App, command: Command) -> Rect {
        app.regions
            .buttons
            .iter()
            .find(|(named, _)| *named == command)
            .map_or_else(
                || panic!("{} is not a word anyone can click", command.name()),
                |(_, area)| *area,
            )
    }

    /// The middle of a box, which is where a pointer lands on one.
    fn middle(area: Rect) -> (u16, u16) {
        (area.x + area.width / 2, area.y + area.height / 2)
    }

    /// The row of a column that holds the item `index` rows down its visible part.
    fn row(area: Rect, index: u16) -> (u16, u16) {
        (area.x + 1, area.y + 1 + index)
    }

    /// A catalogue column part-way through a long list, as the worker fills one: a
    /// browse order - the history the interface opens on is not paged - with its first
    /// page in and the rest of the list behind it.
    fn a_first_page(app: &mut App, loaded: usize, total: Option<usize>) {
        press(app, KeyCode::Char('o'));
        let items = (0..loaded)
            .map(|index| CatalogItem {
                id: format!("GY{index}"),
                kind: "series".to_owned(),
                title: format!("Series {index}"),
                ..CatalogItem::default()
            })
            .collect();
        let listing = app.listing.clone();
        let filters = app.filters.clone();
        app.accept(Response::Catalog {
            listing,
            start: 0,
            filters,
            result: Ok(Page {
                items,
                total,
                next: Some(loaded),
            }),
        });
        app.sent();
    }

    fn several_series(app: &mut App, count: usize) {
        let one = app.series.items[0].clone();
        app.series.set(
            (0..count)
                .map(|index| CatalogItem {
                    id: format!("GY{index}"),
                    title: format!("Series {index}"),
                    ..one.clone()
                })
                .collect(),
        );
    }

    fn several_episodes(app: &mut App, count: usize) {
        let one = app.episodes.items[0].clone();
        app.episodes.set(
            (0..count)
                .map(|index| SeasonEpisode {
                    id: format!("E{index}"),
                    episode: (index + 1).to_string(),
                    episode_number: index as i32 + 1,
                    ..one.clone()
                })
                .collect(),
        );
    }

    /// The one thing a pure test of the arithmetic cannot catch: a word drawn in one
    /// place and clicked in another. Every box is read back off the screen it was
    /// written for. `日本語` is here on purpose - three characters, six columns - so a
    /// width measured in bytes or in characters moves the box and fails this outright.
    #[test]
    fn a_word_answers_where_it_was_drawn() {
        let mut app = app();
        let buffer = buffer(120, 30, &mut app);
        let word = |command| {
            let area = button(&app, command);
            // A character two columns wide is written into the first of them and the
            // second is left blank, so reading a box back means stepping over what a
            // wide character took rather than counting cells.
            let mut text = String::new();
            let mut x = area.x;
            while x < area.right() {
                let symbol = buffer[(x, area.y)].symbol();
                text.push_str(symbol);
                x += cells(&[Span::raw(symbol.to_owned())]).max(1);
            }
            text
        };
        assert_eq!(word(Command::AudioLanguage), "audio 日本語");
        assert_eq!(word(Command::SubtitleLanguage), "subs English");
        assert_eq!(word(Command::Quality), "video 1080p");
        assert_eq!(word(Command::Order), "Continue watching");
        assert_eq!(word(Command::Open), "⏎ open/play");
        assert_eq!(word(Command::Download), "d download");
        assert_eq!(word(Command::Quit), "q quit");
    }

    /// A click chooses, and only a click on what is already chosen opens. The first half
    /// is what makes the second safe: there is no way to land in a column and play an
    /// episode in one go.
    #[test]
    fn a_first_click_chooses_and_a_second_one_opens() {
        let mut app = app();
        several_episodes(&mut app, 4);
        let _ = buffer(120, 30, &mut app);
        let episodes = app.regions.episodes;

        let (x, y) = row(episodes, 2);
        assert!(matches!(click(&mut app, x, y), Action::None));
        assert_eq!(app.focus, Focus::Episodes, "the click went to that column");
        assert_eq!(app.episodes.state.selected(), Some(2));

        // A different row is still only a choice, however many times it is clicked on
        // the way past.
        let (x, y) = row(episodes, 0);
        assert!(matches!(click(&mut app, x, y), Action::None));
        assert_eq!(app.episodes.state.selected(), Some(0));

        match click(&mut app, x, y) {
            Action::Play(episodes) => assert_eq!(episodes.len(), 1),
            _ => panic!("a click on the chosen episode did not play it"),
        }
    }

    /// Landing in a column that has something chosen already must not open it - which is
    /// the whole of what keeps mpv from starting by surprise.
    #[test]
    fn a_click_into_another_column_never_opens_it() {
        let mut app = app();
        let _ = buffer(120, 30, &mut app);
        assert_eq!(app.focus, Focus::Series);
        assert_eq!(
            app.episodes.state.selected(),
            Some(0),
            "a list that has arrived opens on its first item"
        );

        let (x, y) = row(app.regions.episodes, 0);
        assert!(matches!(click(&mut app, x, y), Action::None));
        assert_eq!(app.focus, Focus::Episodes);
    }

    /// A scrolled list draws its offset first, so the row under the pointer is not the
    /// index it would have been at the top of the list.
    #[test]
    fn a_scrolled_column_is_hit_where_it_was_drawn() {
        let mut app = app();
        several_series(&mut app, 100);
        press(&mut app, KeyCode::End);
        let _ = buffer(120, 30, &mut app);
        let offset = app.series.state.offset();
        assert!(offset > 0, "the list never scrolled");

        let (x, y) = row(app.regions.series, 0);
        click(&mut app, x, y);
        assert_eq!(app.series.state.selected(), Some(offset));
    }

    /// The header counts what is loaded against what the list holds, because the column
    /// now shows a page of a list rather than the list. `100 series` under a catalogue
    /// twelve hundred long reads as the end of it, which is exactly the impression this
    /// whole feature exists to correct - and the count is the only place the interface
    /// can say otherwise.
    ///
    /// Where no total is known the count stands alone rather than being made up: a list
    /// whose length nothing counts in the units of this column would otherwise print
    /// `100 of 100` and stop meaning anything.
    #[test]
    fn the_header_says_how_much_of_the_list_is_loaded() {
        let mut app = app();
        a_first_page(&mut app, 100, Some(1203));
        assert!(
            rendered(120, 30, &mut app).contains("100 of 1203 series"),
            "the header kept the count to itself"
        );

        app.paging.total = None;
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("100 series"));
        assert!(
            !screen.contains(" of "),
            "the header invented a total for a list that has none"
        );
    }

    /// The label beside that count is still the button it always was: it says which list
    /// is showing and a click on it moves to the next one. The count is not - it is a
    /// fact about the list rather than something to press - and growing it must not have
    /// turned it into one, or a click meant for the order would land on a number.
    #[test]
    fn the_list_is_still_named_by_a_button_beside_the_count() {
        let mut app = app();
        a_first_page(&mut app, 100, Some(1203));
        let _ = buffer(120, 30, &mut app);

        let label = button(&app, Command::Order);
        assert_eq!(label.width, cells(&[Span::raw("Popular".to_owned())]));
        let (x, y) = middle(label);
        click(&mut app, x, y);
        assert_eq!(app.listing, Listing::Browse(1));
    }

    /// The wheel is one of the ways to the bottom of a column, and the bottom of the
    /// column is what asks for more of the list. It goes through the cursor helper the
    /// keys go through, so this is the whole pointer path - the box the column was drawn
    /// in, the row the pointer was over, the cursor, the request - end to end.
    #[test]
    fn the_wheel_can_reach_the_end_of_the_list_and_ask_for_more() {
        let mut app = app();
        a_first_page(&mut app, 12, Some(40));
        let _ = buffer(120, 30, &mut app);

        let (x, y) = middle(app.regions.series);
        for _ in 0..8 {
            wheel(&mut app, x, y, true);
        }
        assert_eq!(app.series.state.selected(), Some(11));
        assert_eq!(
            app.sent(),
            vec![Request::Catalog {
                listing: Listing::Browse(0),
                start: 12,
                filters: Filters::default(),
            }],
            "the wheel reached the end of the list and asked for nothing"
        );
    }

    /// Looking down a column is not the same as going to work in it, so the wheel leaves
    /// the keyboard where it was.
    #[test]
    fn the_wheel_moves_the_column_under_the_pointer() {
        let mut app = app();
        several_episodes(&mut app, 20);
        let _ = buffer(120, 30, &mut app);

        let (x, y) = middle(app.regions.episodes);
        wheel(&mut app, x, y, true);
        assert_eq!(app.episodes.state.selected(), Some(3));
        assert_eq!(app.focus, Focus::Series, "the wheel took the keyboard away");
        wheel(&mut app, x, y, false);
        assert_eq!(app.episodes.state.selected(), Some(0));
    }

    /// An empty column draws one row that reads as a sentence rather than as an item, so
    /// a click on it may move the keyboard and nothing else.
    #[test]
    fn a_click_on_what_an_empty_column_says_chooses_nothing() {
        let mut app = app();
        app.seasons.clear();
        let _ = buffer(120, 30, &mut app);

        let (x, y) = row(app.regions.seasons, 0);
        click(&mut app, x, y);
        assert_eq!(app.focus, Focus::Seasons);
        assert_eq!(app.seasons.state.selected(), None);
    }

    /// The right button goes back out of the column it was pressed on, wherever the
    /// keyboard happened to be.
    #[test]
    fn the_right_button_goes_back() {
        let mut app = app();
        let _ = buffer(120, 30, &mut app);
        let (x, y) = middle(app.regions.episodes);
        app.on_mouse(pointer(MouseEventKind::Down(MouseButton::Right), x, y));
        assert_eq!(app.focus, Focus::Seasons);
    }

    /// The words along the edges do what their keys do - including after a config file
    /// has moved those keys, which is what says the boxes follow the bindings rather
    /// than a table of their own.
    #[test]
    fn clicking_a_word_does_what_its_key_does() {
        let mut app = app();
        let _ = buffer(120, 30, &mut app);

        let (x, y) = middle(button(&app, Command::Quality));
        click(&mut app, x, y);
        assert_eq!(app.options.video_quality, "720p");

        let (x, y) = middle(button(&app, Command::AudioLanguage));
        click(&mut app, x, y);
        assert!(
            app.picker
                .as_ref()
                .is_some_and(|picker| picker.kind == Picking::Audio)
        );

        let (bindings, warnings) = toml::from_str::<keys::Settings>("quit = \"ctrl-q\"\n")
            .expect("valid config")
            .resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut remapped = themed(Theme::default(), Gallery::detached(false));
        remapped.keys = bindings;
        let _ = buffer(120, 30, &mut remapped);
        let (x, y) = middle(button(&remapped, Command::Quit));
        assert!(matches!(click(&mut remapped, x, y), Action::Quit));
    }

    /// The popup is read and dismissed, so a click anywhere closes it - even one that
    /// landed on a word that would otherwise have answered.
    #[test]
    fn a_click_closes_the_help_and_does_nothing_else() {
        let mut app = app();
        app.show_help = true;
        let _ = buffer(120, 30, &mut app);
        let (x, y) = middle(button(&app, Command::Quit));
        assert!(matches!(click(&mut app, x, y), Action::None));
        assert!(!app.show_help);
        assert!(!app.quit, "the word under the popup answered anyway");
    }

    /// The search box owns the pointer as it owns the keyboard: a click is the way out
    /// of it, and nothing else.
    #[test]
    fn a_click_closes_the_search_box() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('f'));
        let _ = buffer(120, 30, &mut app);
        let (x, y) = middle(app.regions.episodes);
        click(&mut app, x, y);
        assert!(app.editing.is_none());
        assert_eq!(app.focus, Focus::Series, "the click reached a column");
    }

    /// Types a narrowing into the column the keyboard is in, the way a user does it.
    fn narrow(app: &mut App, query: &str) {
        press(app, KeyCode::Char('f'));
        for letter in query.chars() {
            press(app, KeyCode::Char(letter));
        }
    }

    /// A narrowing has to be visible, because the rows it hides are simply not there:
    /// the box says which column it is narrowing while it is open, and the column's own
    /// title says what it was narrowed to and how much of it is left once the box has
    /// gone. A column quietly showing one episode of two is otherwise indistinguishable
    /// from a client that has lost the season.
    #[test]
    fn a_narrowed_column_says_so_in_its_title() {
        let mut app = app();
        app.focus = Focus::Episodes;
        narrow(&mut app, "journ");

        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("Filter Episodes: journ"),
            "the box did not say what it was narrowing, or what with"
        );
        assert!(
            !screen.contains("The Priest's Lie"),
            "a row the narrowing hid was drawn anyway"
        );

        press(&mut app, KeyCode::Enter);
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("Episodes \"journ\" 1/2"),
            "the column's title said nothing about being narrowed"
        );
        assert!(screen.contains("The Journey Ends"));
        assert!(!screen.contains("Filter"), "the box outlived the return");

        // And the way out of it is in the one sentence the status line gets.
        assert!(
            screen.contains("f then esc puts the list back"),
            "nothing said how to get the rest of the season back"
        );
    }

    /// An empty column that has been narrowed to nothing has something to say for
    /// itself, and it is not the sentence an empty column says. A blank box under a
    /// title reading `Episodes "zzz" 0/2` would be the interface knowing the answer and
    /// keeping it.
    #[test]
    fn a_column_narrowed_to_nothing_says_what_it_was_looking_for() {
        let mut app = app();
        app.focus = Focus::Episodes;
        narrow(&mut app, "zzz");

        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("Nothing matches \"zzz\"."));
        assert!(screen.contains("Episodes \"zzz\" 0/2"));
        assert!(
            !screen.contains("Pick a season to list its episodes."),
            "a season is open, so that is not the problem"
        );
        assert!(
            !screen.contains("The Journey Ends"),
            "an episode was drawn, or left in the panel under the columns"
        );
    }

    /// The other half of the invariant, and the half a pure test of the arithmetic
    /// cannot reach: a click on the third row of a narrowed column selects the episode
    /// the third row is showing, and the row the pointer was over is the row the
    /// keyboard is now on. The list widget is handed the rows the narrowing left and
    /// writes its offset back in them, so there is one index space on the screen and
    /// both the pointer and the cursor count in it.
    #[test]
    fn a_click_lands_on_the_row_the_narrowing_left() {
        let mut app = app();
        several_episodes(&mut app, 20);
        app.focus = Focus::Episodes;
        narrow(&mut app, "1");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.episodes.rows(),
            11,
            "E1 and E10 to E19 are the episodes with a 1 in the number"
        );

        let buffer = buffer(120, 30, &mut app);
        let episodes = app.regions.episodes;
        let (x, y) = row(episodes, 2);
        let line: String = (episodes.x..episodes.right())
            .map(|column| buffer[(column, y)].symbol())
            .collect();
        assert!(line.contains("E11"), "the third row reads {line:?}");

        click(&mut app, x, y);
        assert_eq!(app.episodes.state.selected(), Some(2));
        assert_eq!(
            app.episodes.selected().map(|episode| episode.id.clone()),
            Some("E10".to_owned()),
            "the click chose an episode other than the one it was drawn on"
        );
    }

    /// A queue with something in it, put there the way the key puts it there.
    fn with_downloads(app: &mut App, count: usize) {
        app.focus = Focus::Episodes;
        several_episodes(app, count);
        press(app, KeyCode::Char('D'));
        app.focus = Focus::Series;
    }

    /// The queue narrows like any other column, and the strip keeps a row to say so
    /// with: one that vanished as its last row was hidden would read as a queue that had
    /// emptied itself, which is the one thing the panel has to be trusted about. The
    /// query here carries a capital, which is smart case saying it means it.
    #[test]
    fn the_queue_narrows_and_keeps_a_row_to_say_so() {
        let mut app = app();
        with_downloads(&mut app, 2);
        app.focus = Focus::Downloads;
        narrow(&mut app, "E2");
        press(&mut app, KeyCode::Enter);

        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("Downloads \"E2\" 1/2"),
            "the panel said nothing about being narrowed"
        );
        assert!(screen.contains("S01E2"));
        assert!(
            !screen.contains("S01E1"),
            "a row the narrowing hid was drawn"
        );

        narrow(&mut app, "zzz");
        let screen = rendered(120, 30, &mut app);
        assert!(
            screen.contains("Nothing matches \"zzz\"."),
            "the strip went away with its last row"
        );
    }

    /// Where the queue is drawn, and what is on a row of it: what the episode is, how
    /// far it has got and which part of it is moving. A bar and a percentage are the
    /// whole point of the exercise - the interface took them away from indicatif and
    /// has to put them back somewhere.
    #[test]
    fn the_queue_draws_a_bar_and_a_percentage() {
        let mut app = app();
        with_downloads(&mut app, 2);
        app.downloads.items[0].state = State::Running;
        app.downloads.items[0].stages = vec![("video".to_owned(), 1, 4)];

        let screen = rendered(120, 30, &mut app);
        for expected in [
            "Downloads",
            "S01E1",
            "The Journey Ends",
            "25%",
            "video",
            "queued",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
        assert!(
            screen.contains("\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2500}"),
            "no bar was drawn"
        );

        // And what became of it, in the words the panel has room for.
        app.downloads.items[0].state = State::Failed("Crunchyroll said no".to_owned());
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("failed: Crunchyroll said no"));
    }

    /// The panel is not there at all until something is in it, so the three columns are
    /// exactly the size they were before any of this existed - which is most of a run.
    #[test]
    fn an_empty_queue_takes_nothing_from_the_columns() {
        let mut app = app();
        let _ = buffer(120, 30, &mut app);
        let columns = [
            app.regions.series,
            app.regions.seasons,
            app.regions.episodes,
        ];
        assert!(
            !rendered(120, 30, &mut app).contains("Downloads"),
            "a panel with nothing in it"
        );
        assert!(app.regions.downloads.is_empty(), "and nothing to click on");

        with_downloads(&mut app, 1);
        let _ = buffer(120, 30, &mut app);
        assert!(!app.regions.downloads.is_empty(), "the panel was not drawn");
        for (before, after) in columns.iter().zip([
            app.regions.series,
            app.regions.seasons,
            app.regions.episodes,
        ]) {
            assert_eq!(
                before.width, after.width,
                "the columns were squeezed sideways"
            );
            assert!(after.height < before.height, "the strip came from nowhere");
        }

        // Emptying it gives the rows straight back.
        app.downloads.items.clear();
        let _ = buffer(120, 30, &mut app);
        assert_eq!(app.regions.series, columns[0]);
    }

    /// The queue is worked like the columns beside it: a click puts the keyboard on it
    /// and the cursor on a row, and a second click on that row takes it out of the
    /// list. Nothing here is a gesture of its own - it is the columns' own rule, read
    /// through the one command the panel answers to.
    #[test]
    fn the_queue_can_be_worked_by_pointer() {
        let mut app = app();
        with_downloads(&mut app, 3);
        let _ = buffer(120, 30, &mut app);
        let panel = app.regions.downloads;
        assert!(!panel.is_empty());

        let (x, y) = row(panel, 1);
        assert!(matches!(click(&mut app, x, y), Action::None));
        assert_eq!(app.focus, Focus::Downloads, "the click went to the panel");
        assert_eq!(app.downloads.state.selected(), Some(1));
        assert_eq!(app.downloads.items.len(), 3, "a first click dropped a row");

        click(&mut app, x, y);
        assert_eq!(app.downloads.items.len(), 2, "the second click did nothing");

        // The wheel looks without taking the keyboard, the way it does everywhere else.
        app.focus = Focus::Series;
        let _ = buffer(120, 30, &mut app);
        let (x, y) = middle(app.regions.downloads);
        wheel(&mut app, x, y, true);
        assert_eq!(app.focus, Focus::Series, "the wheel took the keyboard away");
    }

    /// The panel says what one row has no room for: the whole of a failure, which is an
    /// anyhow chain rather than a phrase, and which series an episode queued an hour ago
    /// came from.
    #[test]
    fn the_details_panel_explains_the_selected_download() {
        let mut app = app();
        with_downloads(&mut app, 1);
        app.focus = Focus::Downloads;
        app.downloads.items[0].state = State::Failed(
            "get Widevine license for ja-JP: the device provision was refused".to_owned(),
        );
        let screen = rendered(120, 30, &mut app);
        assert!(screen.contains("the device provision was refused"));
        assert!(
            screen.contains("S01E1 · Frieren"),
            "which episode of what, which one row of the queue has no space for"
        );
    }

    /// The strip takes what the body can spare and never the three rows the columns
    /// need, and it is worth more room while someone is reading it than while it is
    /// only being kept an eye on.
    #[test]
    fn the_queue_takes_what_the_body_can_spare() {
        let body = Rect::new(0, 3, 120, 24);
        assert_eq!(downloads_height(body, 0, true), 0, "nothing to show");
        assert_eq!(downloads_height(body, 2, false), 4, "two rows and a border");
        assert_eq!(downloads_height(body, 20, false), 8, "a third of the body");
        assert_eq!(downloads_height(body, 20, true), 12, "half of it, focused");
        // And a terminal with nothing to spare keeps its columns: a border with no room
        // for a row inside it is worse than no panel at all.
        assert_eq!(downloads_height(Rect::new(0, 3, 120, 6), 4, false), 0);
        assert_eq!(downloads_height(Rect::new(0, 3, 120, 3), 4, true), 0);
    }

    /// The bar is a heavy line for what is done on a thin one for what is left, always
    /// the same width between them. A part whose size nothing knows yet is an empty
    /// track rather than a full one, which is the honest reading of not knowing.
    #[test]
    fn the_bar_reads_as_a_progress_bar() {
        let bar = |fraction, width| {
            let (done, left) = meter(fraction, width);
            format!("{done}{left}")
        };
        assert_eq!(bar(Some(0.5), 10), "━━━━━─────");
        assert_eq!(bar(Some(1.0), 10), "━━━━━━━━━━");
        assert_eq!(bar(Some(0.0), 10), "──────────");
        assert_eq!(bar(None, 10), "──────────");
        // Nothing a downloader says can draw outside the bar.
        assert_eq!(bar(Some(4.0), 4), "━━━━");
        assert_eq!(bar(Some(-1.0), 4), "────");
    }

    /// A panel that is only drawn sometimes is a panel that has to survive being drawn
    /// on a terminal with no room for it - including one too small for the columns it
    /// shares the body with.
    #[test]
    fn the_queue_survives_a_cramped_terminal() {
        let mut app = app();
        with_downloads(&mut app, 12);
        app.downloads.items[0].state = State::Running;
        app.downloads.items[0].stages = vec![("Japanese audio".to_owned(), 3, 7)];
        for focus in [Focus::Series, Focus::Downloads] {
            app.focus = focus;
            for (width, height) in [(120, 30), (120, 14), (40, 10), (20, 6), (8, 4), (1, 1)] {
                let screen = rendered(width, height, &mut app);
                assert!(!screen.is_empty(), "{width}x{height}");
                let panel = app.regions.downloads;
                assert!(
                    panel.is_empty() || panel.intersection(Rect::new(0, 0, width, height)) == panel,
                    "the panel was given a box off the edge of a {width}x{height} screen"
                );
            }
        }
    }

    /// An answer about a row that has been dropped names nothing, and a queue that let
    /// one of those put a download back on screen would be a panel nobody could clear.
    #[test]
    fn an_answer_for_a_row_that_has_gone_is_dropped() {
        let mut app = app();
        with_downloads(&mut app, 1);
        let id = app.downloads.items[0].id;
        app.downloads.items.clear();
        // The sentence that said it had been queued is not what is being looked for.
        app.notice = None;
        app.accept(Response::Download {
            id,
            update: Update::Started,
        });
        assert!(app.downloads.items.is_empty());
        assert!(app.notice.is_none(), "it said something about nothing");
    }

    /// The language list follows the columns' rule, and a click that misses it is how it
    /// is cancelled - which is the only way out of it a pointer has.
    #[test]
    fn the_language_list_can_be_worked_by_pointer() {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        let _ = buffer(120, 30, &mut app);
        let popup = app.regions.picker;
        assert!(!popup.is_empty(), "the list wrote down no box");

        // The list is sorted, opens on ja-JP, and offers en-US, fr-FR and ja-JP.
        let (x, y) = row(popup, 0);
        click(&mut app, x, y);
        assert_eq!(
            app.audio(),
            "ja-JP",
            "one click on a locale is a choice, not an answer"
        );
        click(&mut app, x, y);
        assert!(app.picker.is_none(), "choosing closes the list");
        assert_eq!(app.audio(), "en-US");

        press(&mut app, KeyCode::Char('a'));
        let _ = buffer(120, 30, &mut app);
        let (x, y) = middle(app.regions.episodes);
        click(&mut app, x, y);
        assert!(
            app.picker.is_none(),
            "a click beside the list did not cancel"
        );
        assert_eq!(app.audio(), "en-US");
        assert_eq!(
            app.focus,
            Focus::Series,
            "the click that cancelled the list went through to a column as well"
        );
    }
}
