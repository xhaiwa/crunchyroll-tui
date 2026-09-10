mod api;
mod config;
mod credentials;
mod download;
mod drm;
mod manifest;
mod model;
mod output;
mod play;
mod progress;
mod terminal;
mod tui;
mod util;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser};

use crate::api::CrunchyrollClient;
use crate::config::OneOrMany;
use crate::credentials::Secret;
use crate::download::{DownloadOptions, download_episode, download_season};
use crate::util::{parse_langs, parse_url};

#[derive(Debug, Parser)]
#[command(version, about = "Downloads Crunchyroll anime and outputs MKV files")]
// `--in-terminal` says how a video is drawn rather than that there should be one, so it
// only means anything on a run that plays: `--play`, or the interface, which plays at a
// keypress.
#[command(group = ArgGroup::new("playback").args(["play", "tui"]).multiple(true))]
// Every option that a `[defaults]` entry can set is an `Option` with no clap default of
// its own: a value that is only there because clap put it there cannot be told apart
// from one the user typed, and the config file has to lose to the second and win over
// the first.
struct Cli {
    /// Audio language(s), comma-separated. First is the default track. [default: ja-JP]
    #[arg(long)]
    audio_lang: Option<String>,

    /// Subtitle language(s), comma-separated. First is the default track. [default: en-US]
    #[arg(long = "subs-lang")]
    subtitles_lang: Option<String>,

    /// Closed-caption language(s), comma-separated.
    #[arg(long)]
    cc_lang: Option<String>,

    /// Video quality. [default: 1080p]
    #[arg(long)]
    video_quality: Option<String>,

    /// Audio quality. [default: 192k]
    #[arg(long)]
    audio_quality: Option<String>,

    /// Season number. Ignored for an episode URL.
    #[arg(long, default_value_t = 0)]
    season: i32,

    /// Value of the Crunchyroll etp_rt cookie. Prefer $CRUNCHYROLL_ETP_RT or the config
    /// file: an argument is kept in the shell's history and is shown in any recording of
    /// the terminal.
    #[arg(long = "etp-rt", value_name = "COOKIE")]
    etp_rt: Option<Secret>,

    /// Play the stream with mpv as it arrives instead of writing an MKV file.
    #[arg(long)]
    play: bool,

    /// Draw the video in this terminal rather than in a window, working out the
    /// graphics protocol it speaks and the mpv options that go with it. Needs `--play`
    /// or `--tui`.
    // Written as a flag but taking a value, so that `--in-terminal=false` can turn off
    // what the config file turned on. `require_equals` is what keeps the value from
    // swallowing the next argument.
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        requires = "playback"
    )]
    in_terminal: Option<bool>,

    /// Extra argument passed straight to mpv. Repeat for more than one.
    // mpv options start with a dash, so they have to be taken as values rather than as
    // arguments of our own. They are not tied to `--play`: the interface plays without
    // it, and an argument that never reaches mpv costs nothing.
    #[arg(long = "mpv-arg", value_name = "ARG", allow_hyphen_values = true)]
    mpv_arg: Vec<String>,

    /// Log raw playback JSON and manifest XML.
    #[arg(long)]
    debug_manifest: bool,

    /// Browse the catalogue in a terminal interface instead of naming a URL.
    #[arg(long, conflicts_with_all = ["url", "file"])]
    tui: bool,

    /// Colour scheme for the terminal interface, overriding the config file. Without
    /// one the terminal's own palette is used.
    #[arg(long, value_name = "NAME", requires = "tui")]
    theme: Option<String>,

    /// Whether the terminal interface draws posters and episode stills, overriding the
    /// config file. `auto` draws them only where the terminal speaks kitty, sixel or
    /// iTerm2; `on` falls back to half-blocks.
    #[arg(long, value_name = "WHEN", requires = "tui")]
    images: Option<tui::art::Setting>,

    /// URL of the episode or series to download.
    #[arg(long, conflicts_with = "file")]
    url: Option<String>,

    /// Text file containing one URL per line.
    #[arg(long, conflicts_with = "url")]
    file: Option<PathBuf>,
}

fn process_url(
    client: &CrunchyrollClient,
    opts: &DownloadOptions,
    url: &str,
    season: i32,
) -> Result<()> {
    let (content_type, content_id) = parse_url(url)
        .ok_or_else(|| anyhow::anyhow!("Invalid URL (must contain /watch/ or /series/): {url}"))?;

    if content_type == "watch" {
        let info = client.episode_info(&content_id)?;
        download_episode(client, &content_id, &info, opts)
    } else {
        let primary_audio = opts
            .audio_langs
            .first()
            .map(String::as_str)
            .unwrap_or("ja-JP");
        let primary_sub = opts
            .subtitles_langs
            .first()
            .map(String::as_str)
            .unwrap_or("en-US");
        let seasons = client.seasons(&content_id, primary_audio, primary_sub)?;

        if season != 0 {
            let selected = seasons
                .iter()
                .find(|candidate| candidate.season_number == season)
                .ok_or_else(|| anyhow::anyhow!("This anime has no season {season}!"))?;
            let episodes = client.season_episodes(&selected.id, primary_audio, primary_sub)?;
            download_season(client, opts, &episodes)
        } else {
            println!("No season number specified, downloading all seasons...");
            for selected in seasons {
                let episodes = client.season_episodes(&selected.id, primary_audio, primary_sub)?;
                download_season(client, opts, &episodes)?;
            }
            Ok(())
        }
    }
}

/// The command line where it said something, the config file where it did not, and the
/// value the program has always used where neither did.
///
/// A value that was written down is kept even when it is empty, since asking for no
/// subtitles at all is a thing to ask for; only the absence of one falls through.
fn langs(cli: Option<&str>, configured: Option<&OneOrMany>, fallback: &str) -> Vec<String> {
    match (cli, configured) {
        (Some(value), _) => parse_langs(value),
        (None, Some(configured)) => configured.langs(),
        (None, None) => parse_langs(fallback),
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.url.is_none() && cli.file.is_none() && !cli.tui {
        bail!("one of --url, --file or --tui must be supplied");
    }
    let (mut config, mut complaints) = config::load();
    let (etp_rt, warnings) = credentials::resolve(cli.etp_rt.as_ref(), &config)?;
    complaints.extend(warnings);
    // The TUI collects these and shows them on its status line, since anything printed
    // now would be scrolled away by the alternate screen before it was read. Without it
    // they are worth saying straight away, before a request fails for the reason one of
    // them names.
    if !cli.tui {
        for complaint in complaints.drain(..) {
            eprintln!("{complaint}");
        }
    }
    let defaults = &config.defaults;
    let in_terminal = cli.in_terminal.or(defaults.in_terminal).unwrap_or(false);

    let mut opts = DownloadOptions {
        audio_langs: {
            let parsed = langs(
                cli.audio_lang.as_deref(),
                defaults.audio_lang.as_ref(),
                "ja-JP",
            );
            if parsed.is_empty() {
                vec!["ja-JP".into()]
            } else {
                parsed
            }
        },
        subtitles_langs: langs(
            cli.subtitles_lang.as_deref(),
            defaults.subs_lang.as_ref(),
            "en-US",
        ),
        cc_langs: langs(cli.cc_lang.as_deref(), defaults.cc_lang.as_ref(), ""),
        video_quality: cli
            .video_quality
            .clone()
            .or_else(|| defaults.video_quality.clone())
            .unwrap_or_else(|| "1080p".to_owned()),
        audio_quality: cli
            .audio_quality
            .clone()
            .or_else(|| defaults.audio_quality.clone())
            .unwrap_or_else(|| "192k".to_owned()),
        play: cli.play,
        // Repeating `--mpv-arg` is how more than one is given, so an empty set is the
        // only way the command line has of saying nothing about them.
        mpv_args: if cli.mpv_arg.is_empty() {
            defaults
                .mpv_args
                .as_ref()
                .map(OneOrMany::list)
                .unwrap_or_default()
        } else {
            cli.mpv_arg.clone()
        },
    };
    // The terminal can only be asked what it draws once it is not about to be handed to
    // something else, and the interface asks on its own account when it opens, so a run
    // that has one leaves this to it.
    if in_terminal && !cli.tui {
        let (args, warning) = terminal::mpv_args(terminal::detect());
        if let Some(warning) = warning {
            eprintln!("{warning}");
        }
        // Ahead of what was asked for by hand: mpv keeps the last value of an option it
        // is given twice, so `--mpv-arg --vo=gpu` still opens a window.
        opts.mpv_args.splice(0..0, args);
    }
    let client = CrunchyrollClient::new(etp_rt, cli.debug_manifest)?;

    if cli.tui {
        // What was asked for on the command line wins over the file it would have come
        // from otherwise.
        if cli.theme.is_some() {
            config.theme.name = cli.theme;
        }
        if let Some(images) = cli.images {
            config.images = images;
        }
        config.defaults.in_terminal = Some(in_terminal);
        return tui::run(client, opts, config, complaints);
    }

    if let Some(path) = cli.file {
        let file = File::open(&path)
            .with_context(|| format!("failed to open URLs file {}", path.display()))?;
        // A line that cannot be read - invalid UTF-8, an I/O error - is worth a word on
        // stderr and nothing more. Stopping at the first one would drop the rest of the
        // file without saying so, which looks exactly like a short list.
        let urls: Vec<String> = BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(index, line)| match line {
                Ok(line) => Some(line),
                Err(error) => {
                    eprintln!(
                        "! skipping line {} of {}: {error}",
                        index + 1,
                        path.display()
                    );
                    None
                }
            })
            .map(|line| line.trim().to_owned())
            .filter(|line| line.starts_with("http"))
            .collect();
        println!("Found {} URLs to download\n", urls.len());
        for (index, url) in urls.iter().enumerate() {
            println!("=== [{}/{}] {} ===", index + 1, urls.len(), url);
            if let Err(error) = process_url(&client, &opts, url, cli.season) {
                eprintln!("Error: {error:#}");
            }
            println!();
        }
        Ok(())
    } else {
        process_url(
            &client,
            &opts,
            cli.url.as_deref().unwrap_or_default(),
            cli.season,
        )
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{Cli, OneOrMany, Parser, langs};

    /// clap checks the shape of the whole command - groups that name arguments which
    /// exist, `requires` that points somewhere - only when asked.
    #[test]
    fn the_command_line_is_wired_up() {
        Cli::command().debug_assert();
    }

    /// `--in-terminal` is a flag, but one a config file can have turned on already, so
    /// there has to be a way of saying no to it for a single run.
    #[test]
    fn in_terminal_is_a_flag_that_can_still_be_turned_off() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(
                std::iter::once("crunchyroll-downloader").chain(args.iter().copied()),
            )
        };
        assert_eq!(
            parse(&["--tui", "--in-terminal"])
                .expect("bare flag")
                .in_terminal,
            Some(true)
        );
        assert_eq!(
            parse(&["--tui", "--in-terminal=false"])
                .expect("turned off")
                .in_terminal,
            Some(false)
        );
        assert_eq!(
            parse(&["--tui"]).expect("left out").in_terminal,
            None,
            "nothing said on the command line leaves the config file to decide"
        );

        // It says how a video is drawn rather than that there should be one, so it takes
        // a run that plays.
        assert!(parse(&["--url", "URL", "--in-terminal"]).is_err());
        assert!(parse(&["--url", "URL", "--play", "--in-terminal"]).is_ok());

        // The value has to be attached: taken loose it would swallow whatever came next.
        assert!(parse(&["--tui", "--in-terminal", "true"]).is_err());
    }

    #[test]
    fn the_command_line_wins_then_the_config_file_then_the_built_in() {
        let configured = OneOrMany::One("fr-FR,de-DE".to_owned());
        assert_eq!(
            langs(Some("en-US"), Some(&configured), "ja-JP"),
            ["en-US"],
            "a flag beats the config file"
        );
        assert_eq!(
            langs(None, Some(&configured), "ja-JP"),
            ["fr-FR", "de-DE"],
            "the config file beats the built-in default"
        );
        assert_eq!(langs(None, None, "ja-JP"), ["ja-JP"]);
        assert!(
            langs(Some(""), Some(&configured), "ja-JP").is_empty(),
            "asking for none of a track is a thing to ask for"
        );
    }
}
