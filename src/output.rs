use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::model::EpisodeInfo;
use crate::util::{language_code, language_name};

#[derive(Debug, Clone)]
pub struct MediaTrack {
    pub file: PathBuf,
    pub locale: String,
    pub format: String,
    pub is_cc: bool,
    /// Content key for ffmpeg's `-decryption_key`. Set when `file` is a live pipe
    /// carrying data that is still encrypted, empty when `file` was decrypted already.
    pub key: Option<String>,
}

impl MediaTrack {
    pub fn media(file: PathBuf, locale: String, key: Option<String>) -> Self {
        Self {
            file,
            locale,
            format: String::new(),
            is_cc: false,
            key,
        }
    }
}

/// Builds the ffmpeg call that folds one video, every audio and every subtitle track
/// into a single Matroska stream, minus the output target the caller appends.
pub fn build_mux_command(
    video: &MediaTrack,
    audio_tracks: &[MediaTrack],
    sub_tracks: &[MediaTrack],
    info: &EpisodeInfo,
) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-y"]);
    for track in std::iter::once(video).chain(audio_tracks).chain(sub_tracks) {
        if let Some(key) = &track.key {
            command.arg("-decryption_key").arg(key);
        }
        command.arg("-i").arg(&track.file);
    }

    command.args(["-map", "0:v:0"]);
    for index in 0..audio_tracks.len() {
        command.args(["-map", &format!("{}:a:0", index + 1)]);
    }
    for index in 0..sub_tracks.len() {
        command.args(["-map", &format!("{}:0", index + 1 + audio_tracks.len())]);
    }
    command.args(["-c:v", "copy", "-c:a", "copy"]);
    for (index, track) in sub_tracks.iter().enumerate() {
        command
            .arg(format!("-c:s:{index}"))
            .arg(if track.format.eq_ignore_ascii_case("vtt") {
                "srt"
            } else {
                "copy"
            });
    }

    for (index, track) in audio_tracks.iter().enumerate() {
        command
            .arg(format!("-metadata:s:a:{index}"))
            .arg(format!("language={}", language_code(&track.locale)))
            .arg(format!("-metadata:s:a:{index}"))
            .arg(format!("title={}", language_name(&track.locale)))
            .arg(format!("-disposition:a:{index}"))
            .arg(if index == 0 { "default" } else { "0" });
    }
    let mut default_subtitle_set = false;
    for (index, track) in sub_tracks.iter().enumerate() {
        let title = format!(
            "{}{}",
            language_name(&track.locale),
            if track.is_cc { " [CC]" } else { "" }
        );
        let is_default = !track.is_cc && !default_subtitle_set;
        default_subtitle_set |= is_default;
        command
            .arg(format!("-metadata:s:s:{index}"))
            .arg(format!("language={}", language_code(&track.locale)))
            .arg(format!("-metadata:s:s:{index}"))
            .arg(format!("title={title}"))
            .arg(format!("-disposition:s:{index}"))
            .arg(if is_default { "default" } else { "0" });
    }

    let metadata = &info.episode_metadata;
    command
        .arg("-metadata:g")
        .arg(format!(
            "title=S{:02}E{:02} - {}",
            metadata.season_number, metadata.episode_number, info.title
        ))
        .arg("-metadata:g")
        .arg(format!("show={}", metadata.series_title))
        .arg("-metadata:g")
        .arg(format!("track={}", metadata.episode_number))
        .arg("-metadata:g")
        .arg(format!("season_number={}", metadata.season_number));
    command
}

pub fn merge_everything(
    video: &MediaTrack,
    audio_tracks: &[MediaTrack],
    sub_tracks: &[MediaTrack],
    output_file: &Path,
    info: &EpisodeInfo,
) -> Result<()> {
    let mut command = build_mux_command(video, audio_tracks, sub_tracks, info);
    command.arg(output_file);

    let result = command.output().context("run ffmpeg to create MKV")?;
    if !result.status.success() {
        let _ = std::fs::remove_file(output_file);
        bail!(
            "ffmpeg merge failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }

    println!(
        "\nDownload finished! Output file: {}\n",
        output_file.display()
    );
    Ok(())
}
