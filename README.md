# 🍿 Crunchyroll TUI

**Anime night, straight from your terminal.**

Browse Crunchyroll, watch with mpv, or download episodes as MKVs — all from a cozy, keyboard-first Rust TUI. No browser tabs required.

## ✨ Highlights

- **Browse your way** — switch between a poster wall and classic series / seasons / episodes columns. Search, filter, explore simulcasts, and jump into your watchlist or history.
- **Hit play** — stream through mpv, resume where you left off, and sync watch progress with your account. Video can even stay *inside* your terminal.
- **Queue it up** — download episodes or whole seasons in the background without leaving the catalogue. Interrupted downloads keep reusable progress.
- **Keep every track** — choose video quality and multiple audio, subtitle, or closed-caption languages in a single MKV.
- **Make it yours** — themes, remappable keys, mouse support, and artwork via kitty, sixel, iTerm2, or a half-block fallback.

## 🚀 Get started

You'll need:

- [Rust](https://rustup.rs/) and [FFmpeg](https://ffmpeg.org/) in your `PATH`
- [mpv](https://mpv.io/) for playback
- A Crunchyroll account with access to the content
- Your own valid Widevine device provision in the working directory: one `.wvd` file **or** `client_id.bin` + `private_key.pem`

```bash
git clone https://github.com/xhaiwa/crunchyroll-tui.git
cd crunchyroll-tui
cargo install --path .
crunchyroll-tui --tui
```

### 🔑 Sign in

Use the `etp_rt` cookie from a logged-in Crunchyroll browser session (find it in your browser's developer tools under Cookies). **Treat it like a password.**

The recommended setup is to let a password manager provide it:

```toml
# ~/.config/crunchyroll-tui/config.toml
etp_rt_command = "pass show crunchyroll/etp_rt"
```

You can also set `CRUNCHYROLL_ETP_RT` in your environment. Avoid putting the cookie directly on the command line or committing it to a config repository.

## 🎬 What can I do?

```bash
# Browse your library
crunchyroll-tui --tui

# Watch an episode with mpv
crunchyroll-tui --url "EPISODE_URL" --play

# Watch without leaving the terminal
crunchyroll-tui --tui --in-terminal

# Download an episode or a full season
crunchyroll-tui --url "EPISODE_URL"
crunchyroll-tui --url "SERIES_URL" --season 1

# Pick your tracks
crunchyroll-tui --url "EPISODE_URL" \
  --audio-lang ja-JP,en-US --subs-lang en-US --cc-lang en-US

# Download URLs from a file (one per line)
crunchyroll-tui --file list.txt
```

See `crunchyroll-tui --help` for all options.

### ⌨️ Keybindings

| Keys | Action |
| --- | --- |
| `↑ ↓ ← →` / `hjkl` | Navigate |
| `Enter` / `Esc` / `Tab` | Open, go back, switch panels |
| `t` | Toggle columns / poster wall |
| `/` / `f` | Search Crunchyroll / filter the current list |
| `o` / `c` / `n` / `u` | Sort & browse / genre / anime season / simulcasts |
| `p` / `P` | Play an episode / the rest of the season |
| `Space` / `d` / `D` | Select episodes / queue selection / queue season |
| `w` / `m` / `M` | Toggle watchlist / watched / unwatched |
| `a` / `s` / `v` | Audio / subtitles / video quality |
| `i` | Toggle artwork |
| `?` / `q` | All shortcuts / quit |

The footer shows contextual shortcuts, and the mouse works too. Press **`?`** for the full, remappable key list.

## 🎨 Make it yours

Configuration lives at `~/.config/crunchyroll-tui/config.toml` (or `$XDG_CONFIG_HOME/crunchyroll-tui/config.toml`). You don't need a config file unless you want to customize something.

```toml
etp_rt_command = "pass show crunchyroll/etp_rt"
view = "covers"          # "columns" or "covers"
images = "auto"          # "auto", "on", "off"
mouse = true

[defaults]
audio-lang = "ja-JP"
subs-lang = "en-US"
video-quality = "1080p"

[theme]
name = "catppuccin-mocha"
```

Choose from Catppuccin, Gruvbox, Nord, Tokyo Night, Rose Pine, your terminal palette, or a custom Base16 scheme. Remap any key in `[keys]`.

**Want every knob and switch?** Check [`config.example.toml`](config.example.toml).

## 📦 A few useful details

- Downloads produce `Series S01E01 - Title [1080p].mkv` files. Unfinished work stays in `.part` files; rerunning the download reuses compatible progress when possible.
- The TUI download queue runs in the background, so you can keep browsing while episodes download.
- `--play` streams instead of saving an MKV and requires mpv. Playback is Unix-only; downloading is unaffected.
- Terminal artwork automatically picks the best supported image protocol, falling back to coloured half-blocks.

Only download content you're authorized to access, and follow Crunchyroll's terms and applicable law.

## 🛠️ Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

Originally a Rust port of [CuteTenshii/crunchyroll-downloader](https://github.com/CuteTenshii/crunchyroll-downloader).

**License:** [GPL-3.0-or-later](LICENSE).
