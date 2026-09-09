mod app;
mod ui;
mod worker;

use std::io::{self, Write, stdout};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};

use crate::api::CrunchyrollClient;
use crate::download::{DownloadOptions, download_episode, episode_info};
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
pub fn run(client: CrunchyrollClient, options: DownloadOptions) -> Result<()> {
    // Anything the client would print lands on top of the frame, so collect it and let
    // the status line show it instead.
    let notices = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notices);
    let client = client.with_notices(Arc::new(move |message: &str| {
        sink.lock().expect("notices poisoned").push(message.to_owned());
    }));

    let app = App::new(Worker::spawn(client.clone()), options, notices);
    let mut terminal = ratatui::try_init().context("set up the terminal")?;
    let result = event_loop(&mut terminal, &client, app);
    // Give the terminal back whether or not the loop ended well, so an error message
    // is not printed into the alternate screen that is about to disappear.
    ratatui::try_restore().context("restore the terminal")?;
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    client: &CrunchyrollClient,
    mut app: App,
) -> Result<()> {
    while !app.quit {
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .context("draw the interface")?;
        app.drain();
        if event::poll(TICK).context("wait for a key")? {
            match event::read().context("read a key")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match app.on_key(key) {
                    Action::None => {}
                    Action::Quit => app.quit = true,
                    Action::Play(episodes) => {
                        let outcome = suspend(terminal, || play(client, &app.options, &episodes));
                        report(&mut app, outcome, "Playback");
                    }
                    Action::Download(episodes) => {
                        let outcome =
                            suspend(terminal, || download(client, &app.options, &episodes));
                        report(&mut app, outcome, "Download");
                    }
                },
                _ => {}
            }
        }
        app.tick = app.tick.wrapping_add(1);
    }
    Ok(())
}

fn report(app: &mut App, outcome: Result<String>, what: &str) {
    match outcome {
        Ok(message) => app.notice = Some(app::Notice {
            text: message,
            error: false,
        }),
        Err(error) => app.complain(format!("{what} failed: {error:#}")),
    }
}

/// Hands the terminal back to whatever needs to draw on it - mpv, or a progress bar -
/// and takes it again afterwards.
///
/// The panic hook ratatui installs is set up once, by `try_init`, so the screen is
/// re-entered by hand rather than by initialising a second time.
fn suspend<T>(terminal: &mut DefaultTerminal, action: impl FnOnce() -> Result<T>) -> Result<T> {
    ratatui::try_restore().context("hand the terminal over")?;
    let result = action();
    enable_raw_mode().context("take the terminal back")?;
    execute!(stdout(), EnterAlternateScreen).context("re-enter the alternate screen")?;
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
        // Quitting mpv ends one episode normally, so the list carries on to the next.
        download_episode(client, &episode.id, &info, &options)?;
    }
    Ok(match episodes {
        [single] => format!("Played {}", single.title),
        _ => format!("Played {} episodes", episodes.len()),
    })
}

fn download(
    client: &CrunchyrollClient,
    options: &DownloadOptions,
    episodes: &[SeasonEpisode],
) -> Result<String> {
    let options = DownloadOptions {
        play: false,
        ..options.clone()
    };
    let mut failed = 0;
    for episode in episodes {
        let info = episode_info(episode);
        // One episode that cannot be had is not a reason to drop the rest of a season,
        // which is what the command line does too.
        if let Err(error) = download_episode(client, &episode.id, &info, &options) {
            failed += 1;
            eprintln!(
                "Failed to download episode {}: {error:#}",
                episode.episode_number
            );
        }
    }
    println!("\nPress Enter to go back to the catalogue.");
    let _ = io::stdout().flush();
    let _ = io::stdin().read_line(&mut String::new());
    Ok(match (episodes.len(), failed) {
        (_, 0) => format!("Downloaded {} episode(s)", episodes.len()),
        (total, failed) => format!("Downloaded {} of {total} episodes", total - failed),
    })
}
