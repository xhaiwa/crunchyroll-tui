use std::env;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::tui::{art, keys, theme};
use crate::util::parse_langs;

/// `config.toml`. Nothing in it is required, and a file that is not there is not a
/// problem - it is how most runs go.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Whether the posters and episode stills are drawn. `--images` overrides it.
    #[serde(default)]
    pub images: art::Setting,
    /// What a run starts with when the command line does not say.
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub theme: theme::Settings,
    #[serde(default)]
    pub keys: keys::Settings,
    /// The directory the file was read from, so a relative path inside it points at the
    /// file the user meant rather than at the working directory.
    #[serde(skip)]
    pub directory: Option<PathBuf>,
}

/// A setting that is naturally a list but is usually one thing, written either way:
/// `subs-lang = "en-US"` and `subs-lang = ["en-US", "de-DE"]` both mean the same.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    /// The entries as written, which is what an mpv option or a key needs: neither can
    /// be split on anything without breaking a value that contains it.
    pub fn list(&self) -> Vec<String> {
        match self {
            Self::One(single) => vec![single.clone()],
            Self::Many(many) => many.clone(),
        }
    }

    /// The entries as locales, so a value copied straight off the command line -
    /// `"ja-JP,en-US"` - means what it does there.
    pub fn langs(&self) -> Vec<String> {
        self.list()
            .iter()
            .flat_map(|part| parse_langs(part))
            .collect()
    }
}

/// The `[defaults]` section: the command-line options a run starts with. Each one is
/// named after the flag that overrides it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Defaults {
    pub audio_lang: Option<OneOrMany>,
    pub subs_lang: Option<OneOrMany>,
    pub cc_lang: Option<OneOrMany>,
    pub video_quality: Option<String>,
    pub audio_quality: Option<String>,
    /// Extra arguments for mpv, one per entry, as `--mpv-arg` passes them.
    #[serde(alias = "mpv-arg")]
    pub mpv_args: Option<OneOrMany>,
}

/// `$XDG_CONFIG_HOME/crunchyroll-downloader/config.toml`, falling back to
/// `~/.config/crunchyroll-downloader/config.toml`.
pub fn path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("crunchyroll-downloader").join("config.toml"))
}

/// Reads the config, and says what went wrong rather than stopping. A config file is a
/// convenience; a broken one should not stand between the user and the catalogue.
pub fn load() -> (Config, Vec<String>) {
    let Some(path) = path() else {
        return (Config::default(), Vec::new());
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // Not having a config file is the normal case, so only a file that exists and
        // cannot be read is worth saying anything about.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Config::default(), Vec::new());
        }
        Err(error) => {
            return (
                Config::default(),
                vec![format!("cannot read {}: {error}", path.display())],
            );
        }
    };
    match toml::from_str::<Config>(&text) {
        Ok(mut config) => {
            config.directory = path.parent().map(PathBuf::from);
            (config, Vec::new())
        }
        Err(error) => (
            Config::default(),
            vec![format!(
                "{} is not valid config: {}",
                path.display(),
                error.message()
            )],
        ),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::{Config, art};

    #[test]
    fn reads_a_theme_section() {
        let config: Config = toml::from_str(
            "\
[theme]
name = \"gruvbox\"
accent = \"#f47521\"
dim = \"bright black\"
",
        )
        .expect("valid config");
        let (theme, warnings) = config.theme.resolve(None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(theme.accent, Color::Rgb(0xf4, 0x75, 0x21));
        assert_eq!(theme.dim, Color::DarkGray);
        assert_eq!(theme.background, Color::Rgb(0x28, 0x28, 0x28));
    }

    #[test]
    fn reads_the_artwork_setting() {
        for (text, expected) in [
            ("", art::Setting::Auto),
            ("images = \"on\"\n", art::Setting::On),
            ("images = \"off\"\n", art::Setting::Off),
        ] {
            let config: Config = toml::from_str(text).expect("valid config");
            assert_eq!(config.images, expected, "{text:?}");
        }
        assert!(toml::from_str::<Config>("images = \"yes\"\n").is_err());
    }

    #[test]
    fn reads_the_run_defaults_written_either_way() {
        let config: Config = toml::from_str(
            "\
[defaults]
audio-lang = \"ja-JP,en-US\"
subs-lang = [\"en-US\", \"de-DE\"]
video-quality = \"720p\"
mpv-args = [\"--fullscreen\", \"--vf=lavfi=[hqdn3d]\"]
",
        )
        .expect("valid config");
        let defaults = &config.defaults;
        assert_eq!(
            defaults.audio_lang.as_ref().expect("audio").langs(),
            ["ja-JP", "en-US"],
            "a value copied off the command line still means two locales"
        );
        assert_eq!(
            defaults.subs_lang.as_ref().expect("subs").langs(),
            ["en-US", "de-DE"]
        );
        assert_eq!(defaults.video_quality.as_deref(), Some("720p"));
        assert_eq!(defaults.audio_quality, None);
        assert_eq!(
            defaults.mpv_args.as_ref().expect("mpv args").list(),
            ["--fullscreen", "--vf=lavfi=[hqdn3d]"],
            "an mpv option is passed on whole, commas and all"
        );
    }

    #[test]
    fn reads_a_keys_section() {
        let config: Config = toml::from_str("[keys]\nplay = \"o\"\nquit = [\"q\", \"ctrl-q\"]\n")
            .expect("valid config");
        assert_eq!(config.keys.0.len(), 2);
        assert_eq!(
            config.keys.0.get("quit").expect("quit").list(),
            ["q", "ctrl-q"]
        );
    }

    /// A file with no `[theme]` in it is a valid file, and a misspelt key is worth
    /// refusing rather than silently ignoring.
    #[test]
    fn takes_an_empty_config_and_names_a_typo() {
        assert!(toml::from_str::<Config>("").is_ok());
        let error = toml::from_str::<Config>("[theme]\naccnet = \"red\"\n")
            .expect_err("an unknown key is refused");
        assert!(error.message().contains("accnet"), "{}", error.message());
        let error = toml::from_str::<Config>("[defaults]\nvideo_quality = \"720p\"\n")
            .expect_err("an unknown key is refused");
        assert!(
            error.message().contains("video_quality"),
            "{}",
            error.message()
        );
    }
}
