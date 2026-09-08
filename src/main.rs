mod api;
mod download;
mod drm;
mod manifest;
mod model;
mod output;
mod play;
mod progress;
mod util;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::api::CrunchyrollClient;
use crate::download::{DownloadOptions, download_episode, download_season};
use crate::util::{check_etp_rt, parse_langs, parse_url};

#[derive(Debug, Parser)]
#[command(version, about = "Downloads Crunchyroll anime and outputs MKV files")]
struct Cli {
    /// Audio language(s), comma-separated. First is the default track.
    #[arg(long, default_value = "ja-JP")]
    audio_lang: String,

    /// Subtitle language(s), comma-separated. First is the default track.
    #[arg(long = "subs-lang", default_value = "en-US")]
    subtitles_lang: String,

    /// Closed-caption language(s), comma-separated.
    #[arg(long, default_value = "")]
    cc_lang: String,

    /// Video quality.
    #[arg(long, default_value = "1080p")]
    video_quality: String,

    /// Audio quality.
    #[arg(long, default_value = "192k")]
    audio_quality: String,

    /// Season number. Ignored for an episode URL.
    #[arg(long, default_value_t = 0)]
    season: i32,

    /// Value of the Crunchyroll etp_rt cookie.
    #[arg(long = "etp-rt", default_value = "")]
    etp_rt: String,

    /// Play the stream with mpv as it arrives instead of writing an MKV file.
    #[arg(long)]
    play: bool,

    /// Extra argument passed straight to mpv. Repeat for more than one.
    // mpv options start with a dash, so they have to be taken as values rather than as
    // arguments of our own.
    #[arg(
        long = "mpv-arg",
        value_name = "ARG",
        requires = "play",
        allow_hyphen_values = true
    )]
    mpv_arg: Vec<String>,

    /// Log raw playback JSON and manifest XML.
    #[arg(long)]
    debug_manifest: bool,

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

fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.url.is_none() && cli.file.is_none() {
        bail!("one of --url or --file must be supplied");
    }
    let etp_rt = cli.etp_rt.trim();
    if etp_rt.is_empty() {
        bail!(
            "You must specify --etp-rt. Copy the etp_rt cookie from your logged-in Crunchyroll browser session."
        );
    }
    check_etp_rt(etp_rt)?;

    let opts = DownloadOptions {
        audio_langs: {
            let parsed = parse_langs(&cli.audio_lang);
            if parsed.is_empty() {
                vec!["ja-JP".into()]
            } else {
                parsed
            }
        },
        subtitles_langs: parse_langs(&cli.subtitles_lang),
        cc_langs: parse_langs(&cli.cc_lang),
        video_quality: cli.video_quality,
        audio_quality: cli.audio_quality,
        play: cli.play,
        mpv_args: cli.mpv_arg,
    };
    let client = CrunchyrollClient::new(etp_rt.to_owned(), cli.debug_manifest)?;

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
