# Crunchyroll Downloader (Rust)

Rust port of `CuteTenshii/crunchyroll-downloader`. It downloads Crunchyroll episodes and seasons and creates MKV files.

## Features

- Terminal interface for browsing the catalogue and starting playback
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
| `a`, `s`, `v` | Cycle the audio locale, the subtitle locale and the video quality |
| `r` | Reload the current column |
| `?` | Show the keys |
| `q` | Quit |

The locales offered by `a` and `s` are the ones the selected season lists, so they
follow whatever the series actually has. Every other option keeps the value it was given
on the command line, so `--audio-lang`, `--subs-lang`, `--cc-lang` and `--audio-quality`
still set what the interface starts with.

Playing hands the terminal to mpv and takes it back when mpv quits; downloading does the
same with the progress bars.

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
