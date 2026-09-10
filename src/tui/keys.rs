use std::collections::BTreeMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

/// Something the interface can be asked to do.
///
/// The name of a variant is the name it is written under in the config, so adding one
/// adds a line someone can remap. The declaration order is the order warnings come out
/// in, and the order a command's keys appear in the help popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Command {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Open,
    Back,
    NextColumn,
    Search,
    Order,
    Reload,
    Play,
    PlayRest,
    Download,
    DownloadSeason,
    AudioLanguage,
    SubtitleLanguage,
    NextAudio,
    NextSubtitle,
    Quality,
    Images,
    Help,
    Quit,
}

impl Command {
    /// What the command is called in the config file, and in anything said about it.
    /// The match is exhaustive on purpose: a new command cannot be added without being
    /// given a name here and a key in [`DEFAULTS`].
    pub const fn name(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::PageUp => "page-up",
            Self::PageDown => "page-down",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Open => "open",
            Self::Back => "back",
            Self::NextColumn => "next-column",
            Self::Search => "search",
            Self::Order => "order",
            Self::Reload => "reload",
            Self::Play => "play",
            Self::PlayRest => "play-rest",
            Self::Download => "download",
            Self::DownloadSeason => "download-season",
            Self::AudioLanguage => "audio-language",
            Self::SubtitleLanguage => "subtitle-language",
            Self::NextAudio => "next-audio",
            Self::NextSubtitle => "next-subtitle",
            Self::Quality => "quality",
            Self::Images => "images",
            Self::Help => "help",
            Self::Quit => "quit",
        }
    }
}

/// Every command and the keys it answers to out of the box - vim's, with the arrows
/// beside them. They are written the way a user would write them in the config and read
/// by the same parser, so the defaults cannot mean something the config file cannot say.
pub const DEFAULTS: [(Command, &[&str]); 24] = [
    (Command::Up, &["up", "k"]),
    (Command::Down, &["down", "j"]),
    (Command::PageUp, &["pgup"]),
    (Command::PageDown, &["pgdn"]),
    (Command::Top, &["home", "g"]),
    (Command::Bottom, &["end", "G"]),
    (Command::Open, &["enter", "right", "l"]),
    (Command::Back, &["left", "h", "esc"]),
    (Command::NextColumn, &["tab"]),
    (Command::Search, &["/"]),
    (Command::Order, &["o"]),
    (Command::Reload, &["r"]),
    (Command::Play, &["p"]),
    (Command::PlayRest, &["P"]),
    (Command::Download, &["d"]),
    (Command::DownloadSeason, &["D"]),
    (Command::AudioLanguage, &["a"]),
    (Command::SubtitleLanguage, &["s"]),
    (Command::NextAudio, &["A"]),
    (Command::NextSubtitle, &["S"]),
    (Command::Quality, &["v"]),
    (Command::Images, &["i"]),
    (Command::Help, &["?"]),
    (Command::Quit, &["q"]),
];

/// One keypress: the key, and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    code: KeyCode,
    modifiers: KeyModifiers,
}

/// The modifiers worth telling apart. A terminal reports others - a keypad flag, the
/// state of caps lock - only once the enhanced protocol is asked for, which it is not,
/// and a chord that carried one would match nothing.
const HELD: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SHIFT);

impl Chord {
    /// Shift is already in the character the key produced, so it is dropped there: `G`
    /// and `shift-g` are one chord, and a config that says either is understood.
    fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        let modifiers = modifiers.intersection(HELD);
        match code {
            KeyCode::Char(letter) if modifiers.contains(KeyModifiers::SHIFT) => Self {
                code: KeyCode::Char(letter.to_ascii_uppercase()),
                modifiers: modifiers.difference(KeyModifiers::SHIFT),
            },
            _ => Self { code, modifiers },
        }
    }

    fn of(key: KeyEvent) -> Self {
        Self::new(key.code, key.modifiers)
    }

    /// `k`, `ctrl-r`, `pgdn`. Anything that is not a key is `None`, so the config can be
    /// told which line of it is wrong rather than being refused whole.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut modifiers = KeyModifiers::NONE;
        let mut rest = spec.trim();
        // A modifier is a name and a separator, so a lone `-` or `+` is still a key.
        while let Some((head, tail)) = rest
            .split_once(['-', '+'])
            .filter(|(_, tail)| !tail.is_empty())
        {
            modifiers |= match head.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
                "alt" | "meta" | "m" => KeyModifiers::ALT,
                "shift" | "s" => KeyModifiers::SHIFT,
                _ => break,
            };
            rest = tail;
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "enter" | "return" | "cr" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" | "shift-tab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" | "page-up" => KeyCode::PageUp,
            "pagedown" | "pgdn" | "page-down" => KeyCode::PageDown,
            function if matches!(function.as_bytes(), [b'f', ..]) && function.len() > 1 => {
                KeyCode::F(
                    function[1..]
                        .parse()
                        .ok()
                        .filter(|n| (1..=12).contains(n))?,
                )
            }
            _ => {
                let mut letters = rest.chars();
                match (letters.next(), letters.next()) {
                    (Some(letter), None) => KeyCode::Char(letter),
                    _ => return None,
                }
            }
        };
        Some(Self::new(code, modifiers))
    }

    /// How the key is printed in the help popup and along the bottom edge.
    pub fn label(&self) -> String {
        let key = match self.code {
            KeyCode::Up => "↑".to_owned(),
            KeyCode::Down => "↓".to_owned(),
            KeyCode::Left => "←".to_owned(),
            KeyCode::Right => "→".to_owned(),
            KeyCode::Enter => "⏎".to_owned(),
            KeyCode::Esc => "esc".to_owned(),
            KeyCode::Tab => "tab".to_owned(),
            KeyCode::BackTab => "shift-tab".to_owned(),
            KeyCode::Backspace => "backspace".to_owned(),
            KeyCode::Delete => "del".to_owned(),
            KeyCode::Insert => "ins".to_owned(),
            KeyCode::Home => "home".to_owned(),
            KeyCode::End => "end".to_owned(),
            KeyCode::PageUp => "pgup".to_owned(),
            KeyCode::PageDown => "pgdn".to_owned(),
            KeyCode::Char(' ') => "space".to_owned(),
            KeyCode::Char(letter) => letter.to_string(),
            KeyCode::F(number) => format!("f{number}"),
            other => format!("{other:?}").to_lowercase(),
        };
        let mut label = String::new();
        for (modifier, name) in [
            (KeyModifiers::CONTROL, "ctrl-"),
            (KeyModifiers::ALT, "alt-"),
            (KeyModifiers::SHIFT, "shift-"),
        ] {
            if self.modifiers.contains(modifier) {
                label.push_str(name);
            }
        }
        label.push_str(&key);
        label
    }
}

/// Which key does what, in the order the keys should be shown in.
#[derive(Debug, Clone)]
pub struct Bindings {
    table: Vec<(Chord, Command)>,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            table: DEFAULTS
                .iter()
                .flat_map(|(command, specs)| {
                    specs.iter().map(move |spec| {
                        (Chord::parse(spec).expect("a default key parses"), *command)
                    })
                })
                .collect(),
        }
    }
}

impl Bindings {
    pub fn command(&self, key: KeyEvent) -> Option<Command> {
        let chord = Chord::of(key);
        self.table
            .iter()
            .find(|(bound, _)| *bound == chord)
            .map(|(_, command)| *command)
    }

    /// Every key the command answers to, as one label: `↑ k`. Empty if it has none,
    /// which is what a `down = []` in the config asks for.
    pub fn label(&self, command: Command) -> String {
        self.labels(command).join(" ")
    }

    /// The first of the keys, for the one-line reminder along the bottom edge, where
    /// there is no room to list the alternatives. It is the one written first in the
    /// config, which is the one the user thinks of as the key.
    pub fn first(&self, command: Command) -> String {
        self.labels(command).into_iter().next().unwrap_or_default()
    }

    fn labels(&self, command: Command) -> Vec<String> {
        self.table
            .iter()
            .filter(|(_, bound)| *bound == command)
            .map(|(chord, _)| chord.label())
            .collect()
    }
}

/// A command is given one key, or a list of them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Keys {
    One(String),
    Many(Vec<String>),
}

impl Keys {
    fn specs(&self) -> &[String] {
        match self {
            Self::One(spec) => std::slice::from_ref(spec),
            Self::Many(specs) => specs,
        }
    }
}

/// The `[keys]` section: an action, and the key or keys that should do it.
///
/// Naming a command replaces its defaults rather than adding to them - someone who writes
/// `down = "e"` is not asking for `j` as well - and takes the key off whatever else held
/// it, so a whole layout can be moved without having to unbind the old one first.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct Settings(BTreeMap<Command, Keys>);

impl Settings {
    /// Builds the table, and says what it could not do rather than refusing to draw, the
    /// way a misspelt colour does.
    pub fn resolve(&self) -> (Bindings, Vec<String>) {
        let mut bindings = Bindings::default();
        let mut warnings = Vec::new();
        let mut claimed: Vec<Chord> = Vec::new();

        for (command, keys) in &self.0 {
            bindings.table.retain(|(_, bound)| bound != command);
            for spec in keys.specs() {
                let Some(chord) = Chord::parse(spec) else {
                    warnings.push(format!("keys.{}: {spec:?} is not a key", command.name()));
                    continue;
                };
                if let Some(index) = bindings.table.iter().position(|(bound, _)| *bound == chord) {
                    // Displacing a default is the point of the exercise; displacing
                    // something the same file asked for is a contradiction in it.
                    if claimed.contains(&chord) {
                        warnings.push(format!(
                            "keys.{}: {} is already {}",
                            command.name(),
                            chord.label(),
                            bindings.table[index].1.name()
                        ));
                    }
                    bindings.table.remove(index);
                }
                claimed.push(chord);
                bindings.table.push((chord, *command));
            }
        }

        // A command left with nothing cannot be reached. Asking for that is allowed - an
        // empty list is how it is asked for - but arriving at it by taking the last key
        // away for something else is worth a word.
        for (command, _) in DEFAULTS {
            if !self.0.contains_key(&command)
                && !bindings.table.iter().any(|(_, bound)| *bound == command)
            {
                warnings.push(format!("keys: {} has no key left", command.name()));
            }
        }

        (bindings, warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::{Bindings, Chord, Command, DEFAULTS, Settings};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn settings(text: &str) -> Settings {
        toml::from_str(text).expect("valid config")
    }

    fn press(letter: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(letter))
    }

    /// Every command has a key out of the box, none of them share one, and all of them
    /// are written in a form the config parser accepts.
    #[test]
    fn the_defaults_are_a_complete_and_conflict_free_table() {
        let bindings = Bindings::default();
        for (command, specs) in DEFAULTS {
            assert!(
                !specs.is_empty() && !bindings.label(command).is_empty(),
                "{} has no key",
                command.name()
            );
        }
        let mut chords: Vec<String> = bindings
            .table
            .iter()
            .map(|(chord, _)| chord.label())
            .collect();
        let total = chords.len();
        chords.sort();
        chords.dedup();
        assert_eq!(chords.len(), total, "a key is bound twice: {chords:?}");
    }

    /// Every way of writing a key that someone might reasonably write.
    #[test]
    fn takes_a_key_however_it_is_written() {
        for (written, expected) in [
            ("k", Chord::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            ("G", Chord::new(KeyCode::Char('G'), KeyModifiers::NONE)),
            (
                "shift-g",
                Chord::new(KeyCode::Char('G'), KeyModifiers::NONE),
            ),
            (
                "ctrl-r",
                Chord::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            ),
            ("C-r", Chord::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            ("alt+x", Chord::new(KeyCode::Char('x'), KeyModifiers::ALT)),
            ("Enter", Chord::new(KeyCode::Enter, KeyModifiers::NONE)),
            ("pgdn", Chord::new(KeyCode::PageDown, KeyModifiers::NONE)),
            (
                "page-down",
                Chord::new(KeyCode::PageDown, KeyModifiers::NONE),
            ),
            ("f5", Chord::new(KeyCode::F(5), KeyModifiers::NONE)),
            ("space", Chord::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            ("-", Chord::new(KeyCode::Char('-'), KeyModifiers::NONE)),
            (
                "ctrl--",
                Chord::new(KeyCode::Char('-'), KeyModifiers::CONTROL),
            ),
        ] {
            assert_eq!(Chord::parse(written), Some(expected), "{written}");
        }
        for nonsense in ["", "ctrl-", "f13", "grande", "ctrl-grande"] {
            assert_eq!(Chord::parse(nonsense), None, "{nonsense:?}");
        }
    }

    /// The terminal puts shift in the character as well as in the modifiers, so a key
    /// written once has to match the event either way round.
    #[test]
    fn matches_an_uppercase_key_the_way_the_terminal_sends_it() {
        let bindings = Bindings::default();
        let shifted = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(bindings.command(shifted), Some(Command::Bottom));
        assert_eq!(bindings.command(press('G')), Some(Command::Bottom));
        assert_eq!(bindings.command(press('g')), Some(Command::Top));
    }

    /// The point of the section: a layout that is not vim's. Naming a command drops the
    /// keys it had, and takes the new one off whatever was holding it.
    #[test]
    fn remaps_a_command_and_frees_the_key_it_had() {
        let (bindings, warnings) = settings(
            "\
up = \"u\"
down = \"e\"
subtitle-language = [\"s\", \"ctrl-s\"]
",
        )
        .resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(bindings.command(press('e')), Some(Command::Down));
        assert_eq!(bindings.command(press('u')), Some(Command::Up));
        assert_eq!(bindings.command(press('j')), None, "j is no longer down");
        assert_eq!(bindings.command(press('k')), None, "k is no longer up");
        assert_eq!(
            bindings.command(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(Command::SubtitleLanguage)
        );
        // Replacing means replacing: the arrow went with the rest of what `down` had,
        // and someone who wants it keeps it by writing it down.
        assert_eq!(bindings.command(KeyEvent::from(KeyCode::Down)), None);
        assert_eq!(bindings.label(Command::Down), "e");
        // Everything not named keeps what it had.
        assert_eq!(bindings.command(press('q')), Some(Command::Quit));
        assert_eq!(
            bindings.command(KeyEvent::from(KeyCode::Home)),
            Some(Command::Top)
        );
        assert_eq!(bindings.label(Command::Top), "home g");
        assert_eq!(
            bindings.label(Command::SubtitleLanguage),
            "s ctrl-s",
            "the order keys are written in is the order they are shown in"
        );
    }

    /// Taking a key for something else is allowed, and leaving an action unreachable by
    /// doing it is worth saying out loud.
    #[test]
    fn says_what_it_could_not_do_and_carries_on() {
        let (bindings, warnings) = settings(
            "\
up = [\"e\", \"grande\"]
down = \"e\"
images = \"q\"
",
        )
        .resolve();
        assert_eq!(bindings.command(press('e')), Some(Command::Down));
        assert_eq!(bindings.command(press('q')), Some(Command::Images));
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("keys.up") && warnings[0].contains("grande"));
        assert!(warnings[1].contains("keys.down") && warnings[1].contains("up"));
        assert!(
            warnings[2].contains("quit") && warnings[2].contains("no key left"),
            "{}",
            warnings[2]
        );
    }

    /// An empty list is how an action is turned off, and it is not a mistake.
    #[test]
    fn unbinds_an_action_without_complaining() {
        let (bindings, warnings) = settings("images = []\n").resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(bindings.command(press('i')), None);
        assert!(bindings.label(Command::Images).is_empty());
    }

    /// An action that does not exist is refused by name, the way a misspelt theme key is.
    #[test]
    fn names_an_unknown_action() {
        let error = toml::from_str::<Settings>("dwon = \"j\"\n").expect_err("refused");
        assert!(error.message().contains("dwon"), "{}", error.message());
    }

    /// The layout the README offers as an example. A config file that is documented and
    /// does not work is worse than no example at all, so the one in the README is the one
    /// here.
    #[test]
    fn the_colemak_example_is_a_working_config() {
        let (bindings, warnings) = toml::from_str::<Settings>(
            "\
back = [\"n\", \"left\", \"esc\"]
down = [\"e\", \"down\"]
up = [\"i\", \"up\"]
open = [\"o\", \"enter\", \"right\"]
images = \"I\"
order = \"O\"
",
        )
        .expect("valid config")
        .resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        for (letter, expected) in [
            ('n', Command::Back),
            ('e', Command::Down),
            ('i', Command::Up),
            ('o', Command::Open),
            ('I', Command::Images),
            ('O', Command::Order),
        ] {
            assert_eq!(
                bindings.command(KeyEvent::from(KeyCode::Char(letter))),
                Some(expected),
                "{letter}"
            );
        }
        // Nothing was left behind by moving four actions and the two they displaced.
        let (_, none) = Settings::default().resolve();
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(
            Bindings::default().command(KeyEvent::from(KeyCode::Char('i'))),
            Some(Command::Images)
        );
    }
}
