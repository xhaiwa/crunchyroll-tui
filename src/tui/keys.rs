//! Which key does what.
//!
//! Everything the interface can be asked to do has a name, a default key and a place in
//! the help overlay, so a `[keys]` section in the config file can move any of it
//! somewhere else and the overlay still tells the truth about where it went.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

use crate::config::OneOrMany;

/// Everything a key can be pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
    Open,
    Back,
    NextPane,
    Search,
    Sort,
    Reload,
    Play,
    PlaySeason,
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

/// Every command: the name a `[keys]` entry calls it by, and the keys it answers to when
/// the config file says nothing about it.
///
/// `ctrl-c` is deliberately not in here. It is the terminal's own way out and stays
/// wired to quitting whatever the config file does, so a half-finished `[keys]` section
/// cannot leave someone stuck in the alternate screen.
pub const COMMANDS: [(Command, &str, &[&str]); 24] = [
    (Command::Up, "up", &["up", "k"]),
    (Command::Down, "down", &["down", "j"]),
    (Command::PageUp, "page-up", &["page-up"]),
    (Command::PageDown, "page-down", &["page-down"]),
    (Command::First, "first", &["home", "g"]),
    (Command::Last, "last", &["end", "G"]),
    (Command::Open, "open", &["enter", "right", "l"]),
    (Command::Back, "back", &["left", "h", "esc"]),
    (Command::NextPane, "next-pane", &["tab"]),
    (Command::Search, "search", &["/"]),
    (Command::Sort, "sort", &["o"]),
    (Command::Reload, "reload", &["r"]),
    (Command::Play, "play", &["p"]),
    (Command::PlaySeason, "play-season", &["P"]),
    (Command::Download, "download", &["d"]),
    (Command::DownloadSeason, "download-season", &["D"]),
    (Command::AudioLanguage, "audio-language", &["a"]),
    (Command::SubtitleLanguage, "subtitle-language", &["s"]),
    (Command::NextAudio, "next-audio", &["A"]),
    (Command::NextSubtitle, "next-subtitle", &["S"]),
    (Command::Quality, "quality", &["v"]),
    (Command::Images, "images", &["i"]),
    (Command::Help, "help", &["?"]),
    (Command::Quit, "quit", &["q"]),
];

/// The help overlay, a row at a time: the commands whose keys the row shows, and what it
/// says they do. Commands are grouped the way someone reading the list thinks about them
/// - play and play-the-rest-of-the-season are one line, not two.
pub const HELP: [(&[Command], &str); 15] = [
    (&[Command::Up, Command::Down], "move the cursor"),
    (&[Command::Open], "open the selection, and play an episode"),
    (&[Command::Back], "go back a column, and leave a search"),
    (&[Command::NextPane], "cycle the columns"),
    (&[Command::Search], "search the catalogue"),
    (&[Command::Sort], "change the browse order"),
    (
        &[Command::Play, Command::PlaySeason],
        "play the episode / the rest of the season",
    ),
    (
        &[Command::Download, Command::DownloadSeason],
        "download the episode / the whole season",
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
    (
        &[Command::First, Command::Last],
        "jump to the first or last item",
    ),
    (&[Command::Quit], "quit"),
];

/// The one-line reminder along the bottom of the screen: the same idea as [`HELP`], cut
/// down to what fits and to what someone actually reaches for.
pub const HINTS: [(&[Command], &str); 9] = [
    (&[Command::Up, Command::Down], "move"),
    (&[Command::Open], "open/play"),
    (&[Command::Back], "back"),
    (&[Command::Search], "search"),
    (&[Command::Download], "download"),
    (
        &[Command::AudioLanguage, Command::SubtitleLanguage],
        "language",
    ),
    (&[Command::Quality], "quality"),
    (&[Command::Help], "keys"),
    (&[Command::Quit], "quit"),
];

impl Command {
    pub fn name(self) -> &'static str {
        COMMANDS
            .iter()
            .find(|(command, ..)| *command == self)
            .map_or("", |(_, name, _)| *name)
    }

    fn from_name(name: &str) -> Option<Self> {
        COMMANDS
            .iter()
            .find(|(_, key, _)| *key == name)
            .map(|(command, ..)| *command)
    }
}

/// One key, as the terminal reports it and as the config file writes it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Key {
    /// Puts a key into the one shape a lookup can compare.
    ///
    /// A shifted character arrives as the character itself - `P`, not `p` plus a flag -
    /// and terminals are not consistent about whether the flag comes with it, so the
    /// shift is folded into the character and dropped. Ctrl goes the other way: the
    /// terminal always reports the unshifted letter, so `ctrl-C` and `ctrl-c` are the
    /// same key however they were typed.
    fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        let mut modifiers = modifiers
            & (KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SHIFT
                | KeyModifiers::SUPER);
        let code = match code {
            KeyCode::Char(character) if modifiers.contains(KeyModifiers::CONTROL) => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::Char(character.to_ascii_lowercase())
            }
            KeyCode::Char(character) if modifiers.contains(KeyModifiers::SHIFT) => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::Char(character.to_uppercase().next().unwrap_or(character))
            }
            code => code,
        };
        Self { code, modifiers }
    }

    pub fn from_event(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }

    /// `q`, `ctrl-c`, `alt-enter`, `page-up`. A lone character is always the key itself,
    /// so `-` and `+` can be bound without any escaping.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut rest = spec.trim();
        let mut modifiers = KeyModifiers::NONE;
        while rest.chars().nth(1).is_some() {
            let Some((head, tail)) = rest.split_once(['-', '+']) else {
                break;
            };
            let modifier = match head.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "meta" | "option" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                "super" | "cmd" | "win" => KeyModifiers::SUPER,
                _ => break,
            };
            // `ctrl-` with nothing after it names no key.
            if tail.is_empty() {
                return None;
            }
            modifiers |= modifier;
            rest = tail;
        }
        // Before the name table, so the case of a letter survives: `G` is not `g`.
        let mut characters = rest.chars();
        if let (Some(character), None) = (characters.next(), characters.next()) {
            return Some(Self::new(KeyCode::Char(character), modifiers));
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" | "cr" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" | "back-tab" | "shift-tab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "page-up" | "pgup" => KeyCode::PageUp,
            "pagedown" | "page-down" | "pgdn" | "pgdown" => KeyCode::PageDown,
            name => {
                let number = name.strip_prefix('f')?.parse::<u8>().ok()?;
                if !(1..=24).contains(&number) {
                    return None;
                }
                KeyCode::F(number)
            }
        };
        Some(Self::new(code, modifiers))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, name) in [
            (KeyModifiers::CONTROL, "ctrl-"),
            (KeyModifiers::ALT, "alt-"),
            (KeyModifiers::SHIFT, "shift-"),
            (KeyModifiers::SUPER, "super-"),
        ] {
            if self.modifiers.contains(modifier) {
                formatter.write_str(name)?;
            }
        }
        match self.code {
            KeyCode::Char(' ') => formatter.write_str("space"),
            KeyCode::Char(character) => write!(formatter, "{character}"),
            KeyCode::Enter => formatter.write_str("\u{23ce}"),
            KeyCode::Esc => formatter.write_str("esc"),
            KeyCode::Tab => formatter.write_str("tab"),
            KeyCode::BackTab => formatter.write_str("shift-tab"),
            KeyCode::Backspace => formatter.write_str("bksp"),
            KeyCode::Delete => formatter.write_str("del"),
            KeyCode::Insert => formatter.write_str("ins"),
            KeyCode::Up => formatter.write_str("\u{2191}"),
            KeyCode::Down => formatter.write_str("\u{2193}"),
            KeyCode::Left => formatter.write_str("\u{2190}"),
            KeyCode::Right => formatter.write_str("\u{2192}"),
            KeyCode::Home => formatter.write_str("home"),
            KeyCode::End => formatter.write_str("end"),
            KeyCode::PageUp => formatter.write_str("pgup"),
            KeyCode::PageDown => formatter.write_str("pgdn"),
            KeyCode::F(number) => write!(formatter, "f{number}"),
            other => write!(formatter, "{other:?}"),
        }
    }
}

/// The `[keys]` section: a command name against the key, or the keys, that reach it.
///
/// Not a struct with a field per command, because a name that is not a command is worth
/// a word on the status line rather than a config file the program refuses to read.
/// Ordered, so which of two commands claiming the same key wins does not change between
/// runs.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct Settings(pub BTreeMap<String, OneOrMany>);

/// What every key currently does.
pub struct Bindings {
    lookup: HashMap<Key, Command>,
    /// The same thing in the order it was written down, so the help overlay lists a
    /// command's keys the way its owner wrote them rather than however a hash landed.
    order: Vec<(Key, Command)>,
}

impl Default for Bindings {
    fn default() -> Self {
        let mut bindings = Self {
            lookup: HashMap::new(),
            order: Vec::new(),
        };
        for (command, _, keys) in COMMANDS {
            for spec in keys {
                if let Some(key) = Key::parse(spec) {
                    bindings.bind(key, command);
                }
            }
        }
        bindings
    }
}

impl Bindings {
    /// Points `key` at `command`, and says which command had it before.
    fn bind(&mut self, key: Key, command: Command) -> Option<Command> {
        let previous = self.lookup.insert(key, command);
        self.order.retain(|(bound, _)| *bound != key);
        self.order.push((key, command));
        previous
    }

    /// Takes every key away from `command`, so a `[keys]` entry replaces the defaults
    /// rather than piling onto them - which is the only way to give a default key back
    /// to something else.
    fn unbind(&mut self, command: Command) {
        self.lookup.retain(|_, bound| *bound != command);
        self.order.retain(|(_, bound)| *bound != command);
    }

    pub fn command(&self, event: KeyEvent) -> Option<Command> {
        self.lookup.get(&Key::from_event(event)).copied()
    }

    /// The keys that reach `command`, in the order they were written down.
    pub fn keys(&self, command: Command) -> Vec<Key> {
        self.order
            .iter()
            .filter(|(_, bound)| *bound == command)
            .map(|(key, _)| *key)
            .collect()
    }

    /// The keys of every command in `group`, written the way the help overlay wants
    /// them: a command's own keys separated by spaces, one command from the next by a
    /// slash. `None` when nothing in the group is bound at all, so the row can be left
    /// out rather than drawn with an empty key column.
    pub fn label(&self, group: &[Command], all: bool) -> Option<String> {
        let label = group
            .iter()
            .filter_map(|command| {
                let keys = self.keys(*command);
                let keys = if all {
                    &keys[..]
                } else {
                    &keys[..keys.len().min(1)]
                };
                (!keys.is_empty()).then(|| {
                    keys.iter()
                        .map(Key::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            })
            .collect::<Vec<_>>()
            .join(" / ");
        (!label.is_empty()).then_some(label)
    }
}

/// Builds the bindings, and says what it could not do rather than refusing to start. A
/// misspelt key is worth a word on the status line; it is not worth taking the interface
/// away from someone who only wanted to browse.
pub fn resolve(settings: &Settings) -> (Bindings, Vec<String>) {
    let mut bindings = Bindings::default();
    let mut warnings = Vec::new();
    let configured: HashSet<Command> = settings
        .0
        .keys()
        .filter_map(|name| Command::from_name(name))
        .collect();

    for (name, binding) in &settings.0 {
        let Some(command) = Command::from_name(name) else {
            warnings.push(format!(
                "keys.{name} is not a command; try one of {}",
                COMMANDS
                    .iter()
                    .map(|(_, key, _)| *key)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        };
        bindings.unbind(command);
        for spec in binding.list() {
            let Some(key) = Key::parse(&spec) else {
                warnings.push(format!("keys.{name}: {spec:?} is not a key"));
                continue;
            };
            // Taking a key off a default is the whole point of rebinding, so only two
            // entries in the file fighting over one key is worth mentioning.
            if let Some(taken) = bindings.bind(key, command)
                && taken != command
                && configured.contains(&taken)
            {
                warnings.push(format!(
                    "keys.{} and keys.{name} both bind {key}; {name} wins",
                    taken.name()
                ));
            }
        }
    }
    (bindings, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn parses_the_keys_a_config_file_writes() {
        let cases = [
            ("q", KeyCode::Char('q'), KeyModifiers::NONE),
            ("G", KeyCode::Char('G'), KeyModifiers::NONE),
            ("shift-g", KeyCode::Char('G'), KeyModifiers::NONE),
            ("ctrl-d", KeyCode::Char('d'), KeyModifiers::CONTROL),
            ("Ctrl-D", KeyCode::Char('d'), KeyModifiers::CONTROL),
            ("alt+enter", KeyCode::Enter, KeyModifiers::ALT),
            ("page-up", KeyCode::PageUp, KeyModifiers::NONE),
            ("pgdn", KeyCode::PageDown, KeyModifiers::NONE),
            ("f5", KeyCode::F(5), KeyModifiers::NONE),
            ("space", KeyCode::Char(' '), KeyModifiers::NONE),
            // A lone punctuation mark is the key, not a modifier separator.
            ("-", KeyCode::Char('-'), KeyModifiers::NONE),
            ("+", KeyCode::Char('+'), KeyModifiers::NONE),
            ("ctrl--", KeyCode::Char('-'), KeyModifiers::CONTROL),
        ];
        for (spec, code, modifiers) in cases {
            assert_eq!(
                Key::parse(spec),
                Some(Key::new(code, modifiers)),
                "spec: {spec}"
            );
        }
        for spec in ["", "nonsense", "ctrl-", "f99", "ctrl-nonsense"] {
            assert_eq!(Key::parse(spec), None, "spec: {spec}");
        }
    }

    /// Whether the terminal sends the shift flag along with an already-shifted character
    /// is up to the terminal, and ctrl always reports the unshifted letter.
    #[test]
    fn a_key_is_the_same_key_however_the_terminal_reports_it() {
        let bindings = Bindings::default();
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                bindings.command(event(KeyCode::Char('P'), modifiers)),
                Some(Command::PlaySeason),
                "modifiers: {modifiers:?}"
            );
        }
        assert_eq!(
            bindings.command(event(KeyCode::Char('p'), KeyModifiers::NONE)),
            Some(Command::Play)
        );
    }

    #[test]
    fn the_defaults_are_what_the_interface_has_always_had() {
        let bindings = Bindings::default();
        assert_eq!(
            bindings.command(event(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(Command::Down)
        );
        assert_eq!(
            bindings.command(event(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Command::Back)
        );
        assert_eq!(
            bindings.command(event(KeyCode::F(1), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            bindings
                .label(&[Command::Up, Command::Down], true)
                .as_deref(),
            Some("\u{2191} k / \u{2193} j")
        );
    }

    #[test]
    fn a_rebind_replaces_the_defaults_and_takes_the_key_it_asks_for() {
        let settings: Settings = toml::from_str(
            "\
play = \"o\"
quit = [\"q\", \"ctrl-q\"]
",
        )
        .expect("valid keys");
        let (bindings, warnings) = resolve(&settings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            bindings.command(event(KeyCode::Char('o'), KeyModifiers::NONE)),
            Some(Command::Play),
            "the new key wins over the default that held it"
        );
        assert_eq!(
            bindings.command(event(KeyCode::Char('p'), KeyModifiers::NONE)),
            None,
            "the old key is given up"
        );
        assert_eq!(
            bindings.command(event(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Some(Command::Quit)
        );
        // Everything else keeps what it had.
        assert_eq!(
            bindings.command(event(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(Command::Download)
        );
    }

    /// An empty list is how a key is taken away without being given to anything else.
    #[test]
    fn a_command_can_be_unbound() {
        let settings: Settings = toml::from_str("download-season = []\n").expect("valid keys");
        let (bindings, warnings) = resolve(&settings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            bindings.command(event(KeyCode::Char('D'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(bindings.label(&[Command::DownloadSeason], true), None);
    }

    #[test]
    fn names_a_typo_and_a_fight_over_one_key() {
        let settings: Settings = toml::from_str(
            "\
paly = \"p\"
play = \"nonsense\"
",
        )
        .expect("valid keys");
        let (_, warnings) = resolve(&settings);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("paly"), "{}", warnings[0]);
        assert!(warnings[1].contains("nonsense"), "{}", warnings[1]);

        let settings: Settings = toml::from_str(
            "\
download = \"z\"
play = \"z\"
",
        )
        .expect("valid keys");
        let (bindings, warnings) = resolve(&settings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("both bind z"), "{}", warnings[0]);
        assert_eq!(
            bindings.command(event(KeyCode::Char('z'), KeyModifiers::NONE)),
            Some(Command::Play)
        );
    }

    /// Every command is reachable and every default spelling parses, so a command cannot
    /// be added without a key to reach it by.
    #[test]
    fn every_command_has_a_name_and_a_working_default() {
        let bindings = Bindings::default();
        for (command, name, keys) in COMMANDS {
            assert_eq!(command.name(), name);
            assert_eq!(Command::from_name(name), Some(command));
            assert!(!keys.is_empty(), "{name} has no default key");
            for spec in keys {
                let key = Key::parse(spec).unwrap_or_else(|| panic!("{name}: {spec}"));
                assert_eq!(bindings.lookup.get(&key), Some(&command), "{name}: {spec}");
            }
        }
    }
}
