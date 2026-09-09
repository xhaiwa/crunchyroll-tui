# Crunchyroll Downloader (Rust)

Rust port of `CuteTenshii/crunchyroll-downloader`. It downloads Crunchyroll episodes and seasons and creates MKV files.

## Features

- Terminal interface for browsing the catalogue and starting playback, in your own colourscheme
- Series posters and episode stills drawn in the terminal, over kitty, sixel or iTerm2
- Multiple audio, subtitle and closed-caption tracks in one MKV
- Playback with mpv while the stream downloads, instead of writing a file
- Selectable video and audio quality
- Widevine support with either a `.wvd` file or `client_id.bin` plus `private_key.pem`
- Segmented and single-file/on-demand DASH manifests
- Ten parallel segment workers with bounded memory use and retries
- Concurrent video, audio and subtitle downloads
- Automatic access-token refresh
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

## Usage

```shell
cargo run --release -- \
  --url https://www.crunchyroll.com/series/GJ0H7Q5ZJ/hells-paradise \
  --season 1 \
  --etp-rt YOUR_COOKIE_VALUE
```

Download one episode:

```shell
cargo run --release -- \
  --url https://www.crunchyroll.com/watch/GE00198973JAJP/dawn-and-confusion \
  --etp-rt YOUR_COOKIE_VALUE
```

Download several tracks, with the first audio and regular subtitle track marked as default:

```shell
cargo run --release -- \
  --url EPISODE_URL \
  --etp-rt YOUR_COOKIE_VALUE \
  --audio-lang ja-JP,en-US \
  --subs-lang en-US,es-419,de-DE \
  --cc-lang en-US
```

Use `all` to request every available audio, subtitle or closed-caption locale:

```shell
cargo run --release -- --url EPISODE_URL --etp-rt YOUR_COOKIE_VALUE \
  --audio-lang all --subs-lang all --cc-lang all
```

### Browsing the catalogue

`--tui` opens a terminal interface instead of taking a URL: three columns for series,
seasons and episodes, with playback and downloading on a key.

```shell
cargo run --release -- --tui --etp-rt YOUR_COOKIE_VALUE
```

| Key | What it does |
| --- | --- |
| `↑` `↓`, `j` `k` | Move the cursor. `g`/`G` jump to the first or last item |
| `⏎`, `→`, `l` | Open the selection, and play the episode under the cursor |
| `←`, `h`, `esc` | Go back a column, and leave a search |
| `tab` | Cycle the columns |
| `/` | Search the catalogue. An empty search goes back to browsing |
| `o` | Change the browse order: popular, recently added, A to Z |
| `p`, `P` | Play the episode, or the rest of the season one episode after another |
| `d`, `D` | Download the episode, or the whole season |
| `a`, `s` | Pick the audio or subtitle language from a list. `tab` swaps lists, `⏎` applies, `esc` cancels |
| `A`, `S` | Step to the next audio or subtitle locale without opening the list |
| `v` | Cycle the video quality |
| `i` | Show or hide the poster and the episode still |
| `r` | Reload the current column |
| `?` | Show the keys |
| `q` | Quit |

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
cargo run --release -- --tui --etp-rt YOUR_COOKIE_VALUE --images on
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
cargo run --release -- --tui --etp-rt YOUR_COOKIE_VALUE --mpv-arg --vo=kitty
```

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
cargo run --release -- --url EPISODE_URL --etp-rt YOUR_COOKIE_VALUE --play
```

Every track option still applies, so the audio and subtitle locales you ask for all show up as switchable tracks in mpv. `--mpv-arg` passes options through, repeat it for more than one:

```shell
cargo run --release -- --url EPISODE_URL --etp-rt YOUR_COOKIE_VALUE --play \
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
cargo run --release -- --file list.txt --etp-rt YOUR_COOKIE_VALUE
```

Run `cargo run --release -- --help` for every option.

## Finding `etp_rt`

Log in to Crunchyroll in a browser, open Developer Tools, inspect the Crunchyroll cookies under Storage/Application, and copy the value of the `etp_rt` cookie.

## Tests

```shell
cargo test
cargo clippy --all-targets -- -D warnings
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
# crunchyroll-tui
