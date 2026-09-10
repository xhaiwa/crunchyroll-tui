use std::env;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::tui::{art, keys, theme};

/// `config.toml`. Nothing in it is required, and a file that is not there is not a
/// problem - it is how most runs go.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub theme: theme::Settings,
    /// Which key does what. Anything left out keeps the default.
    #[serde(default)]
    pub keys: keys::Settings,
    /// Whether the posters and episode stills are drawn. `--images` overrides it.
    #[serde(default)]
    pub images: art::Setting,
    /// The directory the file was read from, so a relative path inside it points at the
    /// file the user meant rather than at the working directory.
    #[serde(skip)]
    pub directory: Option<PathBuf>,
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
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::style::Color;

    use super::{Config, art};
    use crate::tui::keys::Command;

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
    fn reads_a_keys_section() {
        let config: Config = toml::from_str(
            "\
[keys]
down = \"e\"
up = \"u\"
download = [\"d\", \"ctrl-d\"]
",
        )
        .expect("valid config");
        let (bindings, warnings) = config.keys.resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            bindings.command(KeyEvent::from(KeyCode::Char('e'))),
            Some(Command::Down)
        );
        assert_eq!(bindings.label(Command::Download), "d ctrl-d");
    }

    /// A file with no `[theme]` in it is a valid file, and a misspelt key is worth
    /// refusing rather than silently ignoring.
    #[test]
    fn takes_an_empty_config_and_names_a_typo() {
        assert!(toml::from_str::<Config>("").is_ok());
        let error = toml::from_str::<Config>("[theme]\naccnet = \"red\"\n")
            .expect_err("an unknown key is refused");
        assert!(error.message().contains("accnet"), "{}", error.message());
    }
}
