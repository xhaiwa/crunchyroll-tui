use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Block;
use serde::Deserialize;

/// The seven colours the interface draws with.
///
/// The default is the terminal's own sixteen-colour palette rather than any particular
/// hex value: `Color::Yellow` is a slot the terminal resolves, so the interface arrives
/// wearing whatever colourscheme is already installed instead of dragging a brand orange
/// across it. A theme is something the user opts into, not something we ship on by
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Focused borders, headings, the cursor row, and the numbers worth picking out.
    pub accent: Color,
    /// The page the text sits on. `Reset` means the terminal's own, which is not a
    /// colour we can name - see [`Theme::highlight`].
    pub background: Color,
    pub foreground: Color,
    /// The title of a column that does not have the keyboard.
    pub heading: Color,
    /// Secondary text: counts, running times, dates, hints.
    pub dim: Color,
    pub border: Color,
    pub error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Yellow,
            background: Color::Reset,
            foreground: Color::Reset,
            heading: Color::Gray,
            dim: Color::DarkGray,
            border: Color::DarkGray,
            error: Color::LightRed,
        }
    }
}

impl Theme {
    pub fn text(&self, text: impl Into<String>) -> Span<'static> {
        Span::styled(text.into(), Style::new().fg(self.foreground))
    }

    pub fn strong(&self, text: impl Into<String>) -> Span<'static> {
        Span::styled(
            text.into(),
            Style::new()
                .fg(self.foreground)
                .add_modifier(Modifier::BOLD),
        )
    }

    pub fn dim(&self, text: impl Into<String>) -> Span<'static> {
        Span::styled(text.into(), Style::new().fg(self.dim))
    }

    pub fn accent(&self, text: impl Into<String>) -> Span<'static> {
        Span::styled(text.into(), Style::new().fg(self.accent))
    }

    pub fn error(&self, text: impl Into<String>) -> Span<'static> {
        Span::styled(text.into(), Style::new().fg(self.error))
    }

    /// A heading in the accent colour: the title of a focused column, or of an overlay.
    pub fn title(&self, text: impl Into<String>) -> Span<'static> {
        Span::styled(
            text.into(),
            Style::new().fg(self.accent).add_modifier(Modifier::BOLD),
        )
    }

    pub fn bordered(&self, accented: bool) -> Block<'static> {
        Block::bordered().border_style(Style::new().fg(if accented {
            self.accent
        } else {
            self.border
        }))
    }

    /// The row under the cursor.
    ///
    /// A theme names its own page colour, so the accent becomes the background and the
    /// text is painted in it. The terminal's palette names no such colour - there is no
    /// way to ask for "whatever is behind this text" as a foreground - so the terminal
    /// is asked to swap the two itself, which is what `REVERSED` is for. Both land right
    /// on a light colourscheme; painting literal black on the accent does not.
    pub fn highlight(&self, focused: bool) -> Style {
        if !focused {
            return Style::new().fg(self.accent).add_modifier(Modifier::BOLD);
        }
        match self.background {
            Color::Reset => Style::new()
                .fg(self.accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            background => Style::new()
                .fg(background)
                .bg(self.accent)
                .add_modifier(Modifier::BOLD),
        }
    }
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// A scheme written out as `[accent, background, foreground, heading, dim, border,
/// error]`, in the order the fields are declared.
const fn scheme(hex: [u32; 7]) -> Theme {
    Theme {
        accent: rgb(hex[0]),
        background: rgb(hex[1]),
        foreground: rgb(hex[2]),
        heading: rgb(hex[3]),
        dim: rgb(hex[4]),
        border: rgb(hex[5]),
        error: rgb(hex[6]),
    }
}

/// The colourschemes worth carrying: the handful that people actually run. Anything else
/// is a base16 file away, so shipping more of them would only be a longer list to keep
/// current.
pub const THEMES: [(&str, Theme); 6] = [
    (
        "catppuccin-mocha",
        scheme([
            0xfab387, 0x1e1e2e, 0xcdd6f4, 0xa6adc8, 0x6c7086, 0x45475a, 0xf38ba8,
        ]),
    ),
    (
        "catppuccin-latte",
        scheme([
            0xfe640b, 0xeff1f5, 0x4c4f69, 0x6c6f85, 0x9ca0b0, 0xbcc0cc, 0xd20f39,
        ]),
    ),
    (
        "gruvbox",
        scheme([
            0xfe8019, 0x282828, 0xebdbb2, 0xa89984, 0x928374, 0x504945, 0xfb4934,
        ]),
    ),
    (
        "nord",
        scheme([
            0xd08770, 0x2e3440, 0xd8dee9, 0x81a1c1, 0x616e88, 0x4c566a, 0xbf616a,
        ]),
    ),
    (
        "tokyo-night",
        scheme([
            0xff9e64, 0x1a1b26, 0xc0caf5, 0xa9b1d6, 0x565f89, 0x3b4261, 0xf7768e,
        ]),
    ),
    (
        "rose-pine",
        scheme([
            0xf6c177, 0x191724, 0xe0def4, 0x908caa, 0x6e6a86, 0x403d52, 0xeb6f92,
        ]),
    ),
];

/// `Rosé Pine`, `rose_pine` and `rosepine` all name the same scheme, and nobody should
/// have to remember which spelling we chose.
fn slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter_map(|character| match character {
            'é' | 'è' | 'ê' => Some('e'),
            character if character.is_ascii_alphanumeric() => Some(character),
            _ => None,
        })
        .collect()
}

pub fn named(name: &str) -> Option<Theme> {
    let wanted = match slug(name).as_str() {
        "catppuccin" => "catppuccinmocha".to_owned(),
        "gruvboxdark" => "gruvbox".to_owned(),
        other => other.to_owned(),
    };
    THEMES
        .iter()
        .find(|(key, _)| slug(key) == wanted)
        .map(|(_, theme)| *theme)
}

/// A base16 or base24 scheme file, in the flat pre-0.11 shape (`base00: "282828"`) or in
/// the tinted-theming one (a `palette:` mapping of `base00: "#282828"`). Only the sixteen
/// `baseXX` entries are of any use here, so the file is read a line at a time rather than
/// pulling in a YAML parser for two shapes that differ by an indent and a `#`.
///
/// The roles follow the base16 styling guidelines: `base09` is the scheme's orange, which
/// is what the accent has always been; `base03` is the comment colour, which is what dim
/// text and an unfocused border want.
pub fn parse_base16(text: &str) -> Option<Theme> {
    let mut palette = [None; 16];
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(slot) = key
            .trim()
            .trim_matches(['"', '\''])
            .strip_prefix("base")
            .filter(|digits| digits.len() == 2)
            .and_then(|digits| usize::from_str_radix(digits, 16).ok())
            .filter(|slot| *slot < palette.len())
        else {
            continue;
        };
        let hex: String = value
            .trim()
            .trim_matches(['"', '\''])
            .trim_start_matches('#')
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        if hex.len() == 6 {
            palette[slot] = Color::from_str(&format!("#{hex}")).ok();
        }
    }
    Some(Theme {
        accent: palette[0x9]?,
        background: palette[0x0]?,
        foreground: palette[0x5]?,
        heading: palette[0x4]?,
        dim: palette[0x3]?,
        border: palette[0x3]?,
        error: palette[0x8]?,
    })
}

/// The `[theme]` section of the config file. Everything is optional, and what is left out
/// keeps whatever the layer underneath it decided: the terminal palette, then a named
/// scheme or a base16 file, then these overrides one colour at a time.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    /// One of [`THEMES`].
    pub name: Option<String>,
    /// A base16/tinted-theming scheme file. A relative path is taken from the directory
    /// the config file is in.
    pub base16: Option<PathBuf>,
    pub accent: Option<String>,
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub heading: Option<String>,
    pub dim: Option<String>,
    pub border: Option<String>,
    pub error: Option<String>,
}

/// `#f47521`, `f47521`, `blue`, `bright-blue` and `208` all name a colour. The bare hex
/// form is worth accepting because base16 files are written that way and people copy
/// values out of them.
fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    let hexadecimal = value.len() == 6 && value.chars().all(|c| c.is_ascii_hexdigit());
    Color::from_str(&if hexadecimal {
        format!("#{value}")
    } else {
        value.to_owned()
    })
    .ok()
}

impl Settings {
    /// Builds the theme, and says what it could not do rather than refusing to draw. A
    /// misspelt colour is worth a word on the status line; it is not worth taking the
    /// interface away from someone who only wanted to browse.
    pub fn resolve(&self, base: Option<&Path>) -> (Theme, Vec<String>) {
        let mut theme = Theme::default();
        let mut warnings = Vec::new();

        if let Some(name) = &self.name {
            match named(name) {
                Some(named) => theme = named,
                None => warnings.push(format!(
                    "unknown theme {name:?}; try one of {}",
                    THEMES
                        .iter()
                        .map(|(key, _)| *key)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }

        if let Some(path) = &self.base16 {
            let path = expand(path, base);
            match fs::read_to_string(&path) {
                Ok(text) => match parse_base16(&text) {
                    Some(parsed) => theme = parsed,
                    None => warnings.push(format!(
                        "{} is missing base16 colours; expected base00 through base09",
                        path.display()
                    )),
                },
                Err(error) => warnings.push(format!("cannot read {}: {error}", path.display())),
            }
        }

        let mut apply = |name: &str, value: &Option<String>, slot: &mut Color| {
            let Some(value) = value else { return };
            match parse_color(value) {
                Some(color) => *slot = color,
                None => warnings.push(format!("theme.{name}: {value:?} is not a colour")),
            }
        };
        apply("accent", &self.accent, &mut theme.accent);
        apply("background", &self.background, &mut theme.background);
        apply("foreground", &self.foreground, &mut theme.foreground);
        apply("heading", &self.heading, &mut theme.heading);
        apply("dim", &self.dim, &mut theme.dim);
        apply("border", &self.border, &mut theme.border);
        apply("error", &self.error, &mut theme.error);

        (theme, warnings)
    }
}

/// `~/...` and a path relative to the config file both point where the user meant.
fn expand(path: &Path, base: Option<&Path>) -> PathBuf {
    let path = match path.strip_prefix("~") {
        Ok(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => path.to_path_buf(),
        },
        Err(_) => path.to_path_buf(),
    };
    match base.filter(|_| path.is_relative()) {
        Some(base) => base.join(path),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::{Color, Modifier, Settings, THEMES, Theme, named, parse_base16};

    /// Spelling a scheme's name is not a memory test.
    #[test]
    fn finds_a_theme_by_any_reasonable_spelling() {
        let expected = named("rose-pine").expect("rose-pine ships");
        for spelling in ["Rosé Pine", "rose_pine", "rosepine", "ROSE-PINE"] {
            assert_eq!(named(spelling), Some(expected), "spelling: {spelling}");
        }
        assert_eq!(named("catppuccin"), named("catppuccin-mocha"));
        assert_eq!(named("gruvbox-dark"), named("gruvbox"));
        assert_eq!(named("dracula"), None);
    }

    /// A shipped theme names every colour it draws with, so nothing falls back to a
    /// palette slot the scheme knows nothing about.
    #[test]
    fn every_shipped_theme_is_complete() {
        for (name, theme) in THEMES {
            for color in [
                theme.accent,
                theme.background,
                theme.foreground,
                theme.heading,
                theme.dim,
                theme.border,
                theme.error,
            ] {
                assert!(matches!(color, Color::Rgb(..)), "{name} left {color:?}");
            }
            assert_ne!(theme.accent, theme.background, "{name} is unreadable");
        }
    }

    /// Both shapes of base16 file in the wild: the flat pre-0.11 one, and the
    /// tinted-theming one with its `palette` mapping and `#` prefixes.
    #[test]
    fn reads_either_shape_of_base16_file() {
        let flat = "\
scheme: \"Gruvbox dark, medium\"
author: \"Dawid Kurek\"
base00: \"282828\"
base03: \"665c54\"
base04: \"bdae93\"
base05: \"d5c4a1\"
base08: \"fb4934\"
base09: \"fe8019\"
";
        let tinted = "\
system: \"base16\"
name: \"Gruvbox dark, medium\"
variant: \"dark\"
palette:
  base00: \"#282828\"
  base03: \"#665c54\"
  base04: \"#bdae93\"
  base05: \"#d5c4a1\"
  base08: \"#fb4934\"
  base09: \"#fe8019\"
  base10: \"#1d2021\"
";
        let expected = Theme {
            accent: Color::Rgb(0xfe, 0x80, 0x19),
            background: Color::Rgb(0x28, 0x28, 0x28),
            foreground: Color::Rgb(0xd5, 0xc4, 0xa1),
            heading: Color::Rgb(0xbd, 0xae, 0x93),
            dim: Color::Rgb(0x66, 0x5c, 0x54),
            border: Color::Rgb(0x66, 0x5c, 0x54),
            error: Color::Rgb(0xfb, 0x49, 0x34),
        };
        assert_eq!(parse_base16(flat), Some(expected));
        assert_eq!(parse_base16(tinted), Some(expected));

        // A file that is not a scheme is not half a scheme.
        assert_eq!(parse_base16("hello: world\n"), None);
        assert_eq!(
            parse_base16(flat.trim_end_matches("base09: \"fe8019\"\n")),
            None
        );
    }

    #[test]
    fn layers_a_name_then_a_file_then_the_overrides() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        writeln!(
            file,
            "base00: \"101010\"\nbase03: \"303030\"\nbase04: \"404040\"\nbase05: \"505050\"\nbase08: \"808080\"\nbase09: \"909090\""
        )
        .expect("write scheme");

        // The name goes in first, the file paints over it, and the override has the
        // last word on the one colour it names.
        let settings = Settings {
            name: Some("nord".to_owned()),
            base16: Some(file.path().to_path_buf()),
            accent: Some("#f47521".to_owned()),
            ..Settings::default()
        };
        let (theme, warnings) = settings.resolve(None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(theme.accent, Color::Rgb(0xf4, 0x75, 0x21));
        assert_eq!(theme.background, Color::Rgb(0x10, 0x10, 0x10));
        assert_eq!(theme.foreground, Color::Rgb(0x50, 0x50, 0x50));
    }

    /// Every way of naming a colour that someone might reasonably write, including the
    /// palette slots, which are the point of the whole exercise.
    #[test]
    fn takes_a_colour_however_it_is_written() {
        for (written, expected) in [
            ("#f47521", Color::Rgb(0xf4, 0x75, 0x21)),
            ("f47521", Color::Rgb(0xf4, 0x75, 0x21)),
            ("blue", Color::Blue),
            ("bright blue", Color::LightBlue),
            ("208", Color::Indexed(208)),
        ] {
            let settings = Settings {
                accent: Some(written.to_owned()),
                ..Settings::default()
            };
            let (theme, warnings) = settings.resolve(None);
            assert!(warnings.is_empty(), "{written}: {warnings:?}");
            assert_eq!(theme.accent, expected, "{written}");
        }
    }

    /// A config that cannot be honoured is worth saying so about, and worth nothing more
    /// than that: the interface still has to draw.
    #[test]
    fn says_what_it_could_not_do_and_carries_on() {
        let settings = Settings {
            name: Some("dracula".to_owned()),
            base16: Some("/nowhere/at/all.yaml".into()),
            accent: Some("puce".to_owned()),
            ..Settings::default()
        };
        let (theme, warnings) = settings.resolve(None);
        assert_eq!(theme, Theme::default());
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].contains("dracula") && warnings[0].contains("nord"));
        assert!(warnings[1].contains("/nowhere/at/all.yaml"));
        assert!(warnings[2].contains("theme.accent"));
    }

    /// The bug this replaced: literal black on the accent, which a light scheme cannot
    /// read. The row now takes the theme's own page colour, or leaves the swap to the
    /// terminal when there is no page colour to name.
    #[test]
    fn never_paints_the_cursor_row_black() {
        let latte = named("catppuccin-latte").expect("catppuccin-latte ships");
        let style = latte.highlight(true);
        assert_eq!(style.bg, Some(latte.accent));
        assert_eq!(style.fg, Some(latte.background));

        let style = Theme::default().highlight(true);
        assert_eq!(style.bg, None);
        assert!(style.add_modifier.contains(Modifier::REVERSED));
    }
}
