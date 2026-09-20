mod app;
pub mod art;
pub mod keys;
mod mouse;
pub mod theme;
mod ui;
mod worker;

use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};

use crate::api::CrunchyrollClient;
use crate::config::Config;
use crate::download::{DownloadOptions, download_episode, episode_info, playing_options};
use crate::model::SeasonEpisode;

use app::{Action, App};
use worker::Worker;

/// The orders the catalogue can be browsed in: the value Crunchyroll wants, and the
/// name shown for it.
pub const SORTS: [(&str, &str); 3] = [
    ("popularity", "Popular"),
    ("newly_added", "Recently added"),
    ("alphabetical", "A to Z"),
];

pub const QUALITIES: [&str; 5] = ["1080p", "720p", "480p", "360p", "240p"];

/// How long a redraw waits for a key before going round again. Short enough for the
/// spinner to turn and for a finished request to show up promptly.
const TICK: Duration = Duration::from_millis(100);

/// Browses the catalogue and hands episodes to mpv, or to the downloader.
///
/// `config` arrives with whatever the command line overrode already applied, and
/// `complaints` carries what reading it - and finding the cookie - had to say: the
/// terminal is about to belong to the alternate screen, so those are shown on the status
/// line rather than printed.
pub fn run(
    client: CrunchyrollClient,
    mut options: DownloadOptions,
    config: Config,
    mut complaints: Vec<String>,
) -> Result<()> {
    let (theme, warnings) = config.theme.resolve(config.directory.as_deref());
    complaints.extend(warnings);
    let (bindings, warnings) = config.keys.resolve();
    complaints.extend(warnings);
    let images = config.images;

    // Anything the client would print lands on top of the frame, so collect it and let
    // the status line show it instead.
    let notices = Arc::new(Mutex::new(complaints));
    let sink = Arc::clone(&notices);
    let client = client.with_notices(Arc::new(move |message: &str| {
        sink.lock()
            .expect("notices poisoned")
            .push(message.to_owned());
    }));

    let mut terminal = ratatui::try_init().context("set up the terminal")?;
    // The terminal is asked what graphics it can draw by writing escape sequences and
    // reading the answer back off stdin, so this belongs after the alternate screen is
    // entered and before the event loop starts taking keys off the same stdin.
    let gallery = art::Gallery::open(images);
    // The gallery has just asked the terminal what it can draw, so mpv can be told the
    // same thing without a second round of escape sequences going out over stdin.
    if config.defaults.in_terminal.unwrap_or(false) {
        let (args, warning) = crate::terminal::mpv_args(gallery.protocol());
        if let Some(warning) = warning {
            notices.lock().expect("notices poisoned").push(warning);
        }
        // Ahead of what was asked for by hand: mpv keeps the last value of an option it
        // is given twice, so `--mpv-arg --vo=gpu` still opens a window.
        options.mpv_args.splice(0..0, args);
    }
    // After the gallery, not before: it asks the terminal what it can draw and reads the
    // answer straight off stdin, and a mouse report arriving in the middle of that
    // answer is an answer lost.
    let mouse = config.mouse.unwrap_or(true) && mouse::enable();
    if mouse {
        mouse::restore_on_panic();
    }
    let app = App::new(
        Worker::spawn(client.clone()),
        options,
        theme,
        bindings,
        notices,
        gallery,
    );
    let result = event_loop(&mut terminal, &client, app, mouse);
    // Before the terminal is given back, never after: `try_restore` turns raw mode off
    // first, and a report landing in the moment between the two is printed on the user's
    // shell as `^[[<0;40;12M`.
    if mouse {
        mouse::disable();
    }
    // Give the terminal back whether or not the loop ended well, so an error message
    // is not printed into the alternate screen that is about to disappear.
    ratatui::try_restore().context("restore the terminal")?;
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    client: &CrunchyrollClient,
    mut app: App,
    mouse: bool,
) -> Result<()> {
    while !app.quit {
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .context("draw the interface")?;
        app.drain();
        if event::poll(TICK).context("wait for a key")? {
            // A wheel spun hard, or a pointer dragged down a column, arrives as a burst,
            // and answering each one with a frame of its own draws the same picture a
            // dozen times over to show one movement. So everything already waiting is
            // taken in before the next frame. The first one that needs the terminal ends
            // the burst: what is queued behind it was aimed at a screen mpv is about to
            // draw over.
            loop {
                let action = match event::read().context("read a key")? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Event::Mouse(pointer) => app.on_mouse(pointer),
                    // A resize leaves every box the last frame wrote down describing a
                    // screen that is gone, so nothing may be clicked until the redraw at
                    // the top of the loop has put them back.
                    Event::Resize(..) => {
                        app.forget_layout();
                        Action::None
                    }
                    _ => Action::None,
                };
                match action {
                    Action::None => {}
                    Action::Quit => {
                        app.quit = true;
                        break;
                    }
                    Action::Play(episodes) => {
                        let options = app.options.clone();
                        let outcome =
                            suspend(terminal, mouse, || play(client, &options, &episodes));
                        app.art.forget();
                        app.forget_layout();
                        report(&mut app, outcome, "Playback");
                        break;
                    }
                }
                if !event::poll(Duration::ZERO).context("wait for a key")? {
                    break;
                }
            }
        }
        app.tick = app.tick.wrapping_add(1);
    }
    Ok(())
}

fn report(app: &mut App, outcome: Result<String>, what: &str) {
    match outcome {
        Ok(message) => {
            app.notice = Some(app::Notice {
                text: message,
                error: false,
            })
        }
        Err(error) => app.complain(format!("{what} failed: {error:#}")),
    }
}

/// Hands the terminal back to mpv and takes it again afterwards. mpv may have cleared
/// the artwork the terminal was holding on our behalf, so the caller drops what it had
/// encoded.
///
/// Playing is the only thing left that wants the terminal. A download used to take it
/// too, for an hour of indicatif bars with the whole interface frozen behind them; it
/// now runs on the queue's own thread and draws into a panel, which is what this exists
/// to make possible rather than something it has to arrange.
///
/// The pointer stops being reported for the whole of it, and before raw mode goes rather
/// than after. mpv is given this terminal's stdin so that its own keys work, and an SGR
/// report is `^[[<0;40;12M` - in which `<` and `>` are mpv's previous and next file. A
/// mouse merely moved during playback would otherwise skip episodes.
///
/// The panic hook ratatui installs is set up once, by `try_init`, so the screen is
/// re-entered by hand rather than by initialising a second time.
fn suspend<T>(
    terminal: &mut DefaultTerminal,
    mouse: bool,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if mouse {
        mouse::disable();
    }
    ratatui::try_restore().context("hand the terminal over")?;
    let result = action();
    enable_raw_mode().context("take the terminal back")?;
    execute!(stdout(), EnterAlternateScreen).context("re-enter the alternate screen")?;
    if mouse {
        mouse::enable();
    }
    // Whatever was already on its way when the terminal changed hands is still queued,
    // and none of it was meant for the screen about to be drawn.
    while event::poll(Duration::ZERO).unwrap_or(false) {
        let _ = event::read();
    }
    // Nothing on screen is ours any more, so redraw all of it rather than the diff.
    terminal.clear().context("clear the terminal")?;
    result
}

fn play(
    client: &CrunchyrollClient,
    options: &DownloadOptions,
    episodes: &[SeasonEpisode],
) -> Result<String> {
    let options = DownloadOptions {
        play: true,
        ..options.clone()
    };
    for episode in episodes {
        let info = episode_info(episode);
        println!(
            "Playing S{:02}E{:02} - {}",
            info.episode_metadata.season_number, info.episode_metadata.episode_number, info.title
        );
        // Asked for here rather than taken from the row that was drawn: the column may
        // have been painted an hour ago, or before the episode was watched half through
        // on the phone, and this is the moment the answer is acted on.
        let episode_options = playing_options(client, &options, &episode.id, episode.duration_ms);
        // Quitting mpv ends one episode normally, so the list carries on to the next.
        download_episode(client, &episode.id, &info, &episode_options)?;
    }
    Ok(match episodes {
        [single] => format!("Played {}", single.title),
        _ => format!("Played {} episodes", episodes.len()),
    })
}
