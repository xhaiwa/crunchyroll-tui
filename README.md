# Crunchyroll Downloader (Rust)

Rust port of `CuteTenshii/crunchyroll-downloader`. It downloads Crunchyroll episodes and seasons and creates MKV files.

## Features

- Terminal interface for browsing the catalogue and starting playback, in your own colourscheme and on your own keys
- Series posters and episode stills drawn in the terminal, over kitty, sixel or iTerm2
- Multiple audio, subtitle and closed-caption tracks in one MKV
- Playback with mpv while the stream downloads, instead of writing a file
- Selectable video and audio quality
- Widevine support with either a `.wvd` file or `client_id.bin` plus `private_key.pem`
- Segmented and single-file/on-demand DASH manifests
- Ten parallel segment workers with bounded memory use and retries
- Concurrent video, audio and subtitle downloads
- Automatic access-token refresh
- The session cookie read from `pass`, the environment or the config file, instead of the command line
- Batch downloads from a text file
- MKV stream language, title and default-track metadata

Only download content you are authorized to access and comply with Crunchyroll's terms and applicable law.

## Requirements

- A current Rust toolchain
- [FFmpeg](https://ffmpeg.org/) available in `PATH`, plus [mpv](https://mpv.io/) for `--play` and for playback from `--tui`
- A Crunchyroll account with access to the requested content
- A valid Widevine device provision in the working directory:
  - one `.wvd` file, or
  - both `client_id.bin` and `private_key.pem`

## Build

```shell
cargo build --release
```

The binary is written to `target/release/crunchyroll-downloader`.

## Signing in

Every run needs the `etp_rt` cookie from a logged-in Crunchyroll session. Log in to
Crunchyroll in a browser, open Developer Tools, find the Crunchyroll cookies under
Storage/Application, and copy the value of the `etp_rt` cookie. It is a 36-character
UUID, and it is a session: whoever holds it is signed in as you until you log out.

It can be given four ways, and the first of them that answers is the one used:

1. `--etp-rt COOKIE`
2. `$CRUNCHYROLL_ETP_RT`
3. `etp_rt` in the config file
4. `etp_rt_command` in the config file, whose first line of output is the cookie

The last one keeps the cookie out of every file this program can see, which is what
`pass`, `gopass` and the rest are for:

```toml
etp_rt_command = "pass show crunchyroll/etp_rt"
```

The command runs through `sh`, so a pipeline is fine, and it keeps the terminal while it
runs, so gpg can ask for a passphrase on it. Only the first line of its output is read -
`pass show` prints the whole entry, with the secret on the first line and notes under it.

Top-level keys have to come before any `[section]`, the way TOML works:

```toml
etp_rt_command = "pass show crunchyroll/etp_rt"

[theme]
name = "gruvbox"
```

`etp_rt = "..."` puts the cookie in the file instead, which is simpler and worth a
`chmod 600 ~/.config/crunchyroll-downloader/config.toml` - a file other users can read is
reported on startup. For a single shell session, the environment does as well, and keeps
the value out of the history that the command line lands in:

```shell
export CRUNCHYROLL_ETP_RT="$(pass show crunchyroll/etp_rt)"
```

`--etp-rt` is the one to avoid. An argument is written to the shell's history, is
readable in `/proc` by anyone on the machine for as long as the program runs, and is on
screen in every screenshot or asciinema recording of the command that started it. It is
still accepted, with a word on stderr, because it is convenient for a one-off.

However it arrives, the cookie is never printed back: not by the interface, not by the
messages about it, and not by a `{:?}` of anything that holds it.

## Usage

```shell
cargo run --release -- \
  --url https://www.crunchyroll.com/series/GJ0H7Q5ZJ/hells-paradise \
  --season 1
```

Download one episode:

```shell
cargo run --release -- \
  --url https://www.crunchyroll.com/watch/GE00198973JAJP/dawn-and-confusion
```

Download several tracks, with the first audio and regular subtitle track marked as default:

```shell
cargo run --release -- \
  --url EPISODE_URL \
  --audio-lang ja-JP,en-US \
  --subs-lang en-US,es-419,de-DE \
  --cc-lang en-US
```

Use `all` to request every available audio, subtitle or closed-caption locale:

```shell
cargo run --release -- --url EPISODE_URL \
  --audio-lang all --subs-lang all --cc-lang all
```

### Browsing the catalogue

`--tui` opens a terminal interface instead of taking a URL: three columns for series,
seasons and episodes, with playback and downloading on a key.

```shell
cargo run --release -- --tui
```

| Key | Action | What it does |
| --- | --- | --- |
| `↑` `↓`, `k` `j` | `up`, `down` | Move the cursor |
| `pgup` `pgdn` | `page-up`, `page-down` | Move a page at a time |
| `home` `end`, `g` `G` | `top`, `bottom` | Jump to the first or last item |
| `⏎`, `→`, `l` | `open` | Open the selection, and play the episode under the cursor |
| `←`, `h`, `esc` | `back` | Go back a column, and leave a search |
| `tab` | `next-column` | Cycle the columns |
| `/` | `search` | Search the catalogue. An empty search goes back to browsing |
| `o` | `order` | Change the browse order: popular, recently added, A to Z |
| `p`, `P` | `play`, `play-rest` | Play the episode, or the rest of the season one episode after another |
| `d`, `D` | `download`, `download-season` | Download the episode, or the whole season |
| `a`, `s` | `audio-language`, `subtitle-language` | Pick the audio or subtitle language from a list. `tab` swaps lists, `⏎` applies, `esc` cancels |
| `A`, `S` | `next-audio`, `next-subtitle` | Step to the next audio or subtitle locale without opening the list |
| `v` | `quality` | Cycle the video quality |
| `i` | `images` | Show or hide the poster and the episode still |
| `r` | `reload` | Reload the current column |
| `?` | `help` | Show the keys, as they are bound |
| `q` | `quit` | Quit |

The Action column is the name the key is written under in the config file; see
[Keys](#keys) for moving any of them. `ctrl-c` quits whatever the config says.

The languages offered by `a` and `s` are the ones the selected season lists, falling back
to the series, then to what was asked for on the command line, then to every locale
Crunchyroll publishes - so the list follows whatever the series actually has. The locale
in use is always among them, marked with a dot, and the list opens on it. Changing a
language asks Crunchyroll for the open list again, since titles come back localised and
an episode carries the dub that was asked for; the cursor stays where it was. Every other
option keeps the value it was given on the command line, so `--audio-lang`, `--subs-lang`,
`--cc-lang` and `--audio-quality` still set what the interface starts with.

Playing hands the terminal to mpv and takes it back when mpv quits; downloading does the
same with the progress bars.

### Cover art

The series poster gets a column of its own beside the lists, and the still from the
selected episode sits in the Details panel. They are drawn as pixels, with whichever
graphics protocol the terminal answers to - kitty, sixel or iTerm2 - which is asked for
once at startup rather than guessed from environment variables.

By default they appear only where one of those protocols is available. Every terminal can
manage half-blocks, but half-blocks are a mosaic of coloured cells rather than a picture,
and they drag a hundred colours of their own across the colourscheme the rest of the
interface is careful to wear - so they are opt-in:

```shell
cargo run --release -- --tui --images on
```

`--images off` turns the artwork off altogether, and `i` toggles it while the interface is
running - which also names the protocol in use, if you are wondering why a picture is not
where you expected it. The same setting lives in the config file, above any `[theme]`
section:

```toml
images = "auto"   # or "on", or "off"

[theme]
name = "gruvbox"
```

Posters and stills come off Crunchyroll's own image CDN, at the smallest size that covers
the panel, on threads of their own so nothing waits on them. Nothing is written to disk.

Pair it with mpv's own kitty output and the whole thing - catalogue, artwork and video -
stays inside the terminal:

```shell
cargo run --release -- --tui --mpv-arg --vo=kitty
```

### Keys

The defaults are vim's, with the arrows beside them, which is a layout and not a law: a
`[keys]` section in the config file moves any of them. That matters if you type Colemak,
Dvorak or Bépo, where `hjkl` is scattered across the keyboard rather than sitting under a
hand.

```toml
[keys]
# Colemak's navigation row - neio - with the arrows kept beside it
back = ["n", "left", "esc"]
down = ["e", "down"]
up = ["i", "up"]
open = ["o", "enter", "right"]
# and somewhere to put the two that `i` and `o` were holding
images = "I"
order = "O"
```

The name on the left is one of the actions in the table above, and the right-hand side is
a key or a list of them. Naming an action replaces what it had rather than adding to it -
`down = "e"` means `j` and `↓` no longer move down, and `down = ["e", "down"]` keeps the
arrow - and the key is taken off whatever else was holding it, so a whole layout can be
moved across without unbinding the old one first. An empty list, `images = []`, turns an
action off.

A key is a single character, one of `up`, `down`, `left`, `right`, `enter`, `esc`, `tab`,
`space`, `backspace`, `home`, `end`, `pgup`, `pgdn`, `del`, `ins`, or `f1` to `f12`, with
`ctrl-`, `alt-` and `shift-` in front of it as needed: `ctrl-r`, `alt+x`, `shift-g` -
which is the same key as `G`.

`?` and the line along the bottom edge show the keys as they are actually bound, so a
remapped layout documents itself. Two things stay where they are: `ctrl-c` always quits,
and the search box takes every letter literally, so `/` then `q` searches for `q`.

An action that does not exist is reported on the status line with the config file left
unread, the way a misspelt theme key is. Taking an action's last key away for something
else is reported too, and otherwise allowed.

### Colours

The interface ships with no colours of its own. It draws with the sixteen palette slots
your terminal already resolves - yellow, grey, red - so it arrives wearing whatever
colourscheme is installed and needs no configuration to match it. If you drive your
palette with base16-shell, tinted-theming or anything else that recolours the sixteen
slots, this follows it already.

To pin the colours anyway, write `~/.config/crunchyroll-downloader/config.toml`
(`$XDG_CONFIG_HOME` is honoured if it is set):

```toml
[theme]
name = "catppuccin-mocha"
```

The schemes carried are `catppuccin-mocha`, `catppuccin-latte`, `gruvbox`, `nord`,
`tokyo-night` and `rose-pine`. Spelling is forgiving: `Rosé Pine`, `rose_pine` and
`rosepine` all name the same one, `catppuccin` means mocha, and `--theme NAME` tries one
without editing the file.

Anything else comes from a [base16 or tinted-theming](https://github.com/tinted-theming/schemes)
scheme file, in either the flat or the `palette:` shape:

```toml
[theme]
base16 = "~/.config/tinted-theming/schemes/base16/everforest.yaml"
```

A relative path is taken from the directory the config file is in. `base09` becomes the
accent, `base00` the background, `base05` the text, `base04` a heading, `base03` the dim
text and borders, and `base08` errors.

Either of those can be overridden a colour at a time. A value is a hex triple, a palette
name, or a 256-colour index, so a single colour can stay with the terminal while the rest
of the scheme is pinned:

```toml
[theme]
name = "gruvbox"
accent = "#f47521"   # or "208", or "bright yellow", or "yellow"
background = "#1d2021"
foreground = "#ebdbb2"
heading = "#a89984"
dim = "#928374"
border = "#504945"
error = "#fb4934"
```

A name that does not exist, a file that cannot be read or a colour that cannot be parsed
is reported on the status line and otherwise ignored - a typo in the config should not
stand between you and the catalogue.

### Playing instead of downloading

`--play` streams the episode straight into mpv rather than writing an MKV. Segments go into named pipes, ffmpeg decrypts and muxes them as they arrive, and mpv starts on the first few seconds instead of waiting for the whole episode:

```shell
cargo run --release -- --url EPISODE_URL --play
```

Every track option still applies, so the audio and subtitle locales you ask for all show up as switchable tracks in mpv. `--mpv-arg` passes options through, repeat it for more than one:

```shell
cargo run --release -- --url EPISODE_URL --play \
  --audio-lang ja-JP,en-US --subs-lang en-US,de-DE \
  --mpv-arg --fullscreen --mpv-arg --slang=fre
```

A series or `--file` URL plays its episodes one after another: quitting mpv moves on to the next.

Notes:

- The stream is a pipe, so it cannot be seeked past what mpv has already buffered. mpv keeps a 256 MiB forward and 128 MiB backward window in memory, which covers a few minutes of seeking either way.
- Nothing is kept: no MKV is written, and the already-downloaded episode check is skipped.
- `--play` needs mpv in `PATH`, and named pipes, so it is Unix-only. Downloading is unaffected.

Batch mode accepts one URL per line and ignores blank or non-HTTP lines:

```shell
cargo run --release -- --file list.txt
```

Run `cargo run --release -- --help` for every option.

## Tests

```shell
cargo test
cargo clippy --all-targets -- -D warnings
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
# crunchyroll-tui
