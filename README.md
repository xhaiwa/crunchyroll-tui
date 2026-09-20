# Crunchyroll TUI (Rust)

Rust port of `CuteTenshii/crunchyroll-downloader`. It downloads Crunchyroll episodes and seasons and creates MKV files.

> [!NOTE]
> You have to provide your own Widevine .wvd file or client_id.bin and private_key.pem
> By the way, they're easy to find on some forums, etc.

## Features

- Terminal interface for browsing the catalogue and starting playback, in your own colourscheme and on your own keys
- Downloads that run in the background: a queue in a panel of its own, with the catalogue still usable while a season comes down
- The watchlist and the history kept up to date from the interface: a series added or removed, an episode marked watched or unwatched
- The episodes you already have marked in the column, so a season you have downloaded says so without being downloaded again
- Several episodes of a season marked and queued together, in the order the season lists them
- One XDG config file for the colours, the default languages and quality, mpv's options and every key
- Series posters and episode stills drawn in the terminal, over kitty, sixel or iTerm2
- Multiple audio, subtitle and closed-caption tracks in one MKV
- Downloads a later run can pick up: the finished name appears only once the episode is whole, and what a killed run did fetch is kept for the next one
- Playback with mpv while the stream downloads, instead of writing a file
- Resume where you left off, and the position written back to your account while you watch, so this client, the phone and the web player stay in step
- `--in-terminal`: the video drawn in the terminal itself, protocol and mpv options worked out for you
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

The binary is written to `target/release/crunchyroll-tui`.

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
`chmod 600 ~/.config/crunchyroll-tui/config.toml` - a file other users can read is
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

### Picking a download up again

Episodes land in a directory named after the series, as
`Series S01E01 - Title [1080p].mkv`. That name belongs to finished episodes only: while
the download runs, ffmpeg writes to `Series S01E01 - Title [1080p].mkv.part`, and the
file is renamed onto the real name the moment ffmpeg says it is happy. So an MKV you can
see is an MKV you can watch, and the "already downloaded, skipping" check can never be
fooled by half of one.

Beside it, `Series S01E01 - Title [1080p].mkv.part.json` records what has already been
fetched. Each audio, video and subtitle track is buffered to a hidden `.crdl-` file in
the same directory, and a run that does not finish leaves those files where they are
instead of deleting them. Run the same command again and it fetches what is missing:

- A track that came out whole last time is muxed as it is, and its playback session is
  never opened - so a season interrupted eight episodes in costs eight episodes and not
  the ninth's audio as well.
- A video or audio track that was still arriving is carried on from the byte it stopped
  at, when the episode's manifest is the single-file on-demand kind. Those arrive as one
  long ranged response, so the length of the buffer says exactly which byte to ask for
  next.
- A track from a segmented manifest starts again. Its buffer ends somewhere inside a
  segment rather than between two of them, and there is no honest way to tell where from
  a byte count, so the partial buffer is swept up rather than guessed at.
- Subtitles, which are small, are kept and reused the same way.

What is on disk is only reused when it is certainly the right thing. A run asking for a
different video or audio quality, for different audio, subtitle or caption locales, or
even for the same audio locales in a different order - the first is the default track in
the MKV - throws the lot away and starts again, as does a track file whose size has
changed since it was written, and a buffer that does not begin with exactly the
initialization segment this run just fetched. Every one of those refusals costs a
download; letting one through would cost an MKV that looks finished and is not the
episode you asked for.

A download that finishes deletes its `.part`, its `.part.json` and every buffer they
name. One you abandon keeps them, which is the point. To throw one away by hand, delete
both files under the episode's name:

```shell
rm "Some Series/Some Series S01E01 - Title [1080p].mkv.part"*
```

That leaves the hidden `.crdl-` buffers it named, since nothing points at them any more;
when no other download is running, `rm "Some Series"/.crdl-*` clears those too.

None of this cares where the download came from. An episode queued in the interface is
written by the same code as one asked for on the command line, so quitting mid-download -
which the interface warns you about - leaves that episode's work where the next run will
find it. See [The download queue](#the-download-queue).

`--play` has none of this. It writes no file and keeps nothing, so there is nothing to
come back to.

### Browsing the catalogue

`--tui` opens a terminal interface instead of taking a URL: three columns for series,
seasons and episodes, with playback and downloading on a key.

```shell
cargo run --release -- --tui
```

It opens on Continue watching - the series the account was last watching, newest
first - since that is what a video client is usually wanted for. An account that has
watched nothing, or a history Crunchyroll will not hand over, opens on the catalogue
instead and says so on the status line.

| Key | Action | What it does |
| --- | --- | --- |
| `↑` `↓`, `k` `j` | `up`, `down` | Move the cursor |
| `pgup` `pgdn` | `page-up`, `page-down` | Move a page at a time |
| `home` `end`, `g` `G` | `top`, `bottom` | Jump to the first or last item |
| `⏎`, `→`, `l` | `open` | Open the selection, play the episode under the cursor, drop a row of the queue |
| `←`, `h`, `esc` | `back` | Go back a column, and leave a search |
| `tab` | `next-column` | Cycle the columns: series, seasons, episodes, downloads |
| `/` | `search` | Search the catalogue. An empty search goes back to browsing |
| `o` | `order` | Change the list: popular, recently added, A to Z, the account's watchlist, then Continue watching |
| `p`, `P` | `play`, `play-rest` | Play the episode, or the rest of the season one episode after another |
| `space` | `mark` | Mark the episode under the cursor for downloading, or take the mark off |
| `d`, `D` | `download`, `download-season` | Put the marked episodes on the download queue, or the episode under the cursor if none are marked, or the whole season |
| `w` | `watchlist` | Put the selected series on the watchlist, or take it off if it is already there |
| `m`, `M` | `mark-watched`, `mark-unwatched` | Mark the episode under the cursor watched, or unwatched again |
| `a`, `s` | `audio-language`, `subtitle-language` | Pick the audio or subtitle language from a list. `tab` swaps lists, `⏎` applies, `esc` cancels |
| `A`, `S` | `next-audio`, `next-subtitle` | Step to the next audio or subtitle locale without opening the list |
| `v` | `quality` | Cycle the video quality |
| `i` | `images` | Show or hide the poster and the episode still |
| `r` | `reload` | Reload the current column |
| `?` | `help` | Show the keys, as they are bound |
| `q` | `quit` | Quit |

The Action column is the name the key is written under in the config file; see
[Keys](#keys) for moving any of them. `ctrl-c` quits whatever the config says.

Every one of these can be moved somewhere else; see [Keys](#keys) below.

`o` walks one ring: the three browse orders, then the watchlist of whatever account the
`etp_rt` cookie belongs to, then Continue watching, which is that account's own history
and the list the interface opens on. The watchlist holds only the series on it - a film
there has no seasons behind it and would be a row that does nothing when opened. A search
is not on that ring; it is left with `back` rather than cycled past, and leaving one comes
back to the browse order that was in use when it started.

`w` acts on the series the catalogue column has selected, whoever has the keyboard: the
seasons and the episodes on screen are that series' own. `m` and `M` act on the episode
under the cursor, and what they do is move its playhead - Crunchyroll counts an episode
watched once the playhead has reached the end, so `m` puts it there and `M` puts it back
to the start. An episode Crunchyroll gives no running time for has no end to aim at, and
`m` says so rather than sending a playhead of zero, which is what unwatched means. All
three go to Crunchyroll in the background and say what became of them on the status line:
`Added Frieren to the watchlist`, `Marked E4 watched`.

`space` marks episodes, for the seasons where what you are after is five of the
twenty-four. It works in the Episodes column, on the episode under the cursor, and it
says how many of the season are marked as it goes; `d` then queues the marked ones
instead of the one under the cursor, in the order the season lists them rather than the
order they were marked, so the queue reads down the season the way you would watch it.
The status line says which of the two just happened - `Queued the 5 marked episodes for
download` against `Queued 1 episode for download` - since the panel underneath looks the
same either way. With nothing marked, `d` is the key it always was. `D` is not affected
either way: the whole season is the one thing a handful of marks cannot mean, so it
stays the way to ask for it. Marks are kept against the episodes themselves rather than
against rows, so they come off when the column moves to another season and survive `r`
or a change of language, which are the same season asked for again. They also stay on
after the episodes are queued, so a mark you want gone is one you take off yourself.

The languages offered by `a` and `s` are the ones the selected season lists, falling back
to the series, then to what was asked for on the command line, then to every locale
Crunchyroll publishes - so the list follows whatever the series actually has. The locale
in use is always among them, marked with a dot, and the list opens on it. Changing a
language asks Crunchyroll for the open list again, since titles come back localised and
an episode carries the dub that was asked for; the cursor stays where it was. Every other
option keeps the value it was given on the command line or in the config file, so
`--audio-lang`, `--subs-lang`, `--cc-lang` and `--audio-quality` still set what the
interface starts with.

Playing hands the terminal to mpv and takes it back when mpv quits. Downloading does
not: see [The download queue](#the-download-queue) below.

The Episodes column says what your account has already made of each one: a check for an
episode you have finished, and the time to pick it up from for one you left partway
through - which is where playing it opens. The marker takes the running time's place
rather than a column of its own, so the titles stay where they are on a narrow terminal.
See [Picking up where you left off](#picking-up-where-you-left-off).

It also says what is already on this disk, in two cells of its own ahead of that: a
filled circle `●` for an episode whose MKV is here, a half-filled `◐` for one that has
been started and not finished - a download running now, or the `.part` and `.part.json`
a run that stopped left behind, which is exactly what the next run would pick up. What
you have and what you have watched are different things - an episode can be downloaded
and never watched, or watched on the phone and never downloaded - so they are drawn side
by side rather than sharing a slot.

The files are looked for where downloading would write them, under the directory you
started the program in. The disk is asked when a season is opened, when `v` changes the
quality - the quality is part of the file name, so it is a different file - and at the
moments a queued download changes what is there, so an episode you queue turns half-filled
once it is properly under way and filled as it lands, without the column being reloaded.
It is not asked once per frame:
that would be a couple of hundred questions a second about files that change twice an
hour. An episode downloaded outside the program, or by a run that was going while this
one sat open, shows up the next time the season is opened or reloaded with `r`.

A marked episode carries a bar `▌` in front of its number, at the left edge beside the
cursor. That bar does get a cell of its own, and the column takes one only while
something in the season is marked: a gutter standing empty down every season nobody is
marking would cost every title a cell for the sake of the seasons that are. The disk
markers spend their two cells whether there is a file or not, which is the opposite
decision for the opposite reason - what is on the disk differs from row to row, so a
cell that came and went row by row would leave the titles ragged, while a mark is a fact
about the whole season and the column gains the cell with the first one and loses it
with the last. The three markers are three different things and a row can wear all of
them: `▌ E4  ◐   14:02  Title` is an episode half downloaded, half watched, and marked to
be downloaded again.

#### With a mouse

The interface answers the mouse, and a mouse alone is enough to drive it - quitting
included.

| Gesture | What it does |
| --- | --- |
| Click a row | Move the cursor there, and put the keyboard on that column |
| Click it again | Open it: the seasons, the episodes, or mpv |
| Drag | Move the cursor down the column the drag started in |
| Wheel | Scroll the column under the pointer, leaving the keyboard where it is |
| Right click | Go back out of the column it was pressed on |
| Click a word along an edge | What its key does: `open/play`, `back`, `search`, `download`, `language`, `quality`, `keys`, `quit`, and `audio`, `subs` and `video` at the top right |
| Click a queue row, then again | Move the cursor there, then take that download out of the queue |
| Click the listing label | Move on to the next list, or leave a search |
| Click beside the language list | Cancel it, the way `esc` does |

Opening takes a second click rather than a quick double click, so a slow hand and a slow
link work the same as a fast one - and a first click into a column can only ever choose,
which is what keeps mpv from starting by surprise. Two quick clicks are still two clicks
on the same row, so a double click does what you expect.

Nothing the pointer does is anything a key cannot do, so `?` remains the whole list of
what the interface can be asked for.

Asking the terminal to report the pointer takes away its own click-and-drag text
selection. Hold shift while you drag to get it back, or turn the whole thing off with
`--mouse=false` for one run, or `mouse = false` in the config file. Inside tmux, nothing
reaches the interface until tmux itself is told `set -g mouse on`.

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
where you expected it. The same setting lives in the [config file](#configuration), as
`images = "auto"`.

Posters and stills come off Crunchyroll's own image CDN, at the smallest size that covers
the panel, on threads of their own so nothing waits on them. Nothing is written to disk.

Pair it with [`--in-terminal`](#video-in-the-terminal) and the whole thing - catalogue, artwork and video - stays
inside the terminal:

```shell
cargo run --release -- --tui --in-terminal
```

### The download queue

`d` and `D` do not take the terminal away any more. The episode goes on a queue, the
queue is worked by a thread of its own, and the catalogue stays where it is: look
something else up, change the subtitle language, queue another season, all while an
episode is being written.

`tab` reaches the Downloads panel, which appears under the three columns as soon as
there is anything in it and takes nothing from them while there is not. One row per
episode - what it is, and whether it is queued, downloading with a bar and a
percentage, done, or failed with the reason. The bar names the part of the episode
being waited on: the subtitles, the video, one audio track per locale, then the mux.
The Details panel underneath says the whole of a failure, which rarely fits on a row.

Episodes are downloaded one at a time, in the order they were asked for. A single
download is already as parallel inside as the connection will take - ten segment
workers, three audio versions - so running two of them at once would only split the
same pipe in half. An episode that fails takes itself and nothing else: the queue
carries on to the next one, and the status line says what went wrong.

The options a download runs with are the ones that were in force when it was queued.
Change the quality or the audio language afterwards and the episodes already on the
queue keep what they were asked for - only the ones queued from then on get the new
setting.

`⏎` on a row of the panel takes it out of the queue, as does a second click on it.
Anything but the episode that is currently downloading can go: that one is inside an
hour of segments on a thread of its own and there is no calling it back, so it says so
and stays. Quitting while a download is running asks a second time for the same reason,
since leaving ends the thread with everything else. `ctrl-c` never argues.

What that episode had already fetched is not thrown away when you go, and the next run
carries on rather than starting it over: see
[Picking a download up again](#picking-a-download-up-again).

### Playing instead of downloading

`--play` streams the episode straight into mpv rather than writing an MKV. Segments go into named pipes, ffmpeg decrypts and muxes them as they arrive, and mpv starts on the first few seconds instead of waiting for the whole episode:

```shell
cargo run --release -- --url EPISODE_URL --play
```

Every track option still applies, so the audio and subtitle locales you ask for all show up as switchable tracks in mpv. `--in-terminal` keeps the picture in the terminal; `--mpv-arg` passes options through, repeat it for more than one, and `[defaults] mpv-args` in the [config file](#configuration) sets them once and for all:

```shell
cargo run --release -- --url EPISODE_URL --play \
  --audio-lang ja-JP,en-US --subs-lang en-US,de-DE \
  --mpv-arg --fullscreen --mpv-arg --slang=fre
```

A series or `--file` URL plays its episodes one after another: quitting mpv moves on to the next.

#### Picking up where you left off

Before an episode plays, Crunchyroll is asked where the account left off in it, and mpv
opens there. While it plays the position goes back every fifteen seconds, and once more
when mpv quits, so the phone and the web player carry on from where you stopped - and so
does this, next time around. Both ends of an episode are exceptions: one you are less than
thirty seconds into starts at the beginning, one you are within thirty seconds of the end
of has nothing left to resume, and one Crunchyroll has marked as watched is left alone
whatever its position says.

Resuming is not instant. The stream is a named pipe and cannot be seeked, so `--start=` is
a forward seek mpv serves by reading through its demuxer cache. It lands in the right
place, but a long jump has the whole of the episode up to that point to read through
first, at the speed the segments come down. Dropping those segments at the source is what
would make it instant, and that is a larger change than this one - it reaches into the
DASH segment loop and the decryption path either side of it.

A position that cannot be read costs the resume and nothing else, and one that cannot be
reported is not worth interrupting a video for: either way the episode plays.

Notes:

- The stream is a pipe, so it cannot be seeked past what mpv has already buffered. mpv keeps a 256 MiB forward and 128 MiB backward window in memory, which covers a few minutes of seeking either way.
- Nothing is kept: no MKV is written, and the already-downloaded episode check is skipped.
- `--play` needs mpv in `PATH`, and named pipes, so it is Unix-only. Downloading is unaffected.

### Video in the terminal

`--in-terminal` plays the episode where you are rather than in a window of its own. The
terminal is asked which graphics protocol it speaks - the same question the artwork asks,
asked once - and mpv is handed the video output that goes with the answer:

| The terminal speaks | mpv is given |
| --- | --- |
| kitty | `--vo=kitty,tct`, plus `--vo-kitty-use-shm=yes` when the terminal is on this machine |
| sixel | `--vo=sixel,tct` |
| iTerm2 | `--vo=sixel,tct` - mpv has no iTerm2 output, and a terminal that speaks it speaks sixel too |
| nothing | `--vo=tct`, and a word on the status line saying why the picture looks like that |

Each one ends in `tct` - true colour drawn as text - because `--vo` takes a list and uses
the first output that starts, so a build of mpv without sixel compiled in still shows the
episode. All of them are scaled on the CPU frame by frame, so `--profile=sw-fast` goes
with every one: it is what decides whether the picture keeps up.

Except on kitty with shared memory, a frame reaches the terminal as escape sequences -
around 35 bytes for every character cell it covers, because each cell carries its own
colour. A terminal filling a large screen is some 16000 cells, which at 24 frames a
second is 13 MB/s of text to parse, lay out and paint; no terminal keeps up, and the
frames mpv drops waiting are the picture stuttering. So the video is drawn into a box of
at most 4000 cells - the shape of your terminal, shrunk - which is nearer 3 MB/s. The
picture is made of fewer, larger cells and it runs at the frame rate instead of lurching.
`--mpv-arg --vo-tct-width=200 --mpv-arg --vo-tct-height=56` asks for a bigger one if your
terminal can take it.

```shell
cargo run --release -- --url EPISODE_URL --play --in-terminal
```

It says how a video is drawn, never that there should be one, so it needs `--play` or
`--tui` and leaves a download a download. `in-terminal = true` in the [config
file](#configuration) sets it once and for all, and `--in-terminal=false` turns it back
off for a single run. Anything you pass with `--mpv-arg` comes after it and mpv keeps the
last value of an option it is given twice, so `--mpv-arg --vo=gpu` still opens a window.

Batch mode accepts one URL per line and ignores blank or non-HTTP lines:

```shell
cargo run --release -- --file list.txt
```

Run `cargo run --release -- --help` for every option.

## Configuration

Everything the interface can be told lives in one file, so a setup can go in a dotfiles
repo and follow you to the next machine:

```
$XDG_CONFIG_HOME/crunchyroll-tui/config.toml
~/.config/crunchyroll-tui/config.toml     # when $XDG_CONFIG_HOME is not set
```

Nothing in it is required, and not having one at all is the normal case. **The command
line wins over the file**, so a flag is how you try something without editing it.

A file that cannot be read, a key that is not a key, a colour that cannot be parsed: each
one is reported - on the interface's status line, or on stderr for a plain download - and
otherwise ignored. A typo should not stand between you and the catalogue. A name that is
not a setting at all is the exception: it costs the whole file, so the message that names
it is worth reading.

[`config.example.toml`](config.example.toml) is that file written out in full, commented,
with every value at the one the program uses anyway - so copying it changes nothing and
you can delete your way down to what you actually care about:

```shell
mkdir -p ~/.config/crunchyroll-tui
cp config.example.toml ~/.config/crunchyroll-tui/config.toml
```

A test parses it on every run of the suite and checks each value against the built-in
default, so it cannot quietly rot.

The shape of it:

```toml
# Where the etp_rt cookie comes from. See Signing in above - naming a password
# manager keeps it out of the file altogether.
etp_rt_command = "pass show crunchyroll"

# Posters and episode stills: "auto", "on" or "off". Top level, so it has to come
# before the first section - that is TOML, not us.
images = "auto"

# Whether the interface answers the mouse. Turning it off gives the terminal back
# its own click-and-drag text selection. Top level too.
mouse = true

# What a run starts with when the command line does not say. Each one is named after
# the flag that overrides it.
[defaults]
audio-lang = "ja-JP"          # or ["ja-JP", "en-US"], or "all"
subs-lang = "en-US"
cc-lang = []
video-quality = "1080p"       # 1080p, 720p, 480p, 360p, 240p
audio-quality = "192k"
in-terminal = false           # play in the terminal rather than in a window
mpv-args = []                 # ["--fullscreen", "--slang=fre"]

[theme]
# See Colours below.

[keys]
# See Keys below.
```

Anything that is a list can be written as one value when there is only one, so
`subs-lang = "en-US"` and `subs-lang = ["en-US"]` are the same thing. A language entry
may also be a comma-separated string, the way the flag takes it:
`audio-lang = "ja-JP,en-US"`.

`mpv-args` holds one option per entry, as `--mpv-arg` passes them, and is never split on
anything - so a value with a comma in it survives. Passing `--mpv-arg` on the command
line replaces the list rather than adding to it. `in-terminal` is [the shortcut for
playing in the terminal](#video-in-the-terminal), and whatever it works out goes ahead of
`mpv-args`, which therefore overrules it.

### Keys

The defaults are vim's, with the arrows beside them, which is a layout and not a law: a
`[keys]` section moves any of them. That matters if you type Colemak, Dvorak or Bépo,
where `hjkl` is scattered across the keyboard rather than sitting under a hand.

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

The name on the left is a command, and the right-hand side is a key or a list of them.
Naming a command replaces what it had rather than adding to it - `down = "e"` means `j`
and `↓` no longer move down, and `down = ["e", "down"]` keeps the arrow - and the key is
taken off whatever else was holding it, so a whole layout can be moved across without
unbinding the old one first. An empty list, `images = []`, turns a command off.

The commands, and the keys they answer to out of the box:

| Command | Default | What it does |
| --- | --- | --- |
| `up` `down` | `↑` `k`, `↓` `j` | Move the cursor |
| `page-up` `page-down` | `pgup`, `pgdn` | Move it ten rows |
| `top` `bottom` | `home` `g`, `end` `G` | Jump to the first or last item |
| `open` | `enter` `right` `l` | Open the selection, play an episode, drop a download |
| `back` | `left` `h` `esc` | Go back a column, and leave a search |
| `next-column` | `tab` | Cycle the columns |
| `search` | `/` | Search the catalogue |
| `order` | `o` | Change the list the catalogue shows |
| `reload` | `r` | Reload the current column |
| `play` `play-rest` | `p`, `P` | Play the episode, or the rest of the season |
| `mark` | `space` | Mark the episode for downloading, or unmark it |
| `download` `download-season` | `d`, `D` | Queue the marked episodes, or the episode under the cursor, or the whole season |
| `watchlist` | `w` | Put the series on the watchlist, or take it off |
| `mark-watched` `mark-unwatched` | `m`, `M` | Mark the episode watched, or unwatched |
| `audio-language` `subtitle-language` | `a`, `s` | Open the language list |
| `next-audio` `next-subtitle` | `A`, `S` | Step to the next locale without the list |
| `quality` | `v` | Cycle the video quality |
| `images` | `i` | Show or hide the poster and the episode still |
| `help` | `?` | Show the keys |
| `quit` | `q` | Quit |

A key is a single character, one of `up`, `down`, `left`, `right`, `enter`, `esc`, `tab`,
`backtab`, `space`, `backspace`, `home`, `end`, `pgup`, `pgdn`, `del`, `ins`, or `f1` to
`f12`, with `ctrl-`, `alt-` and `shift-` in front of it as needed: `ctrl-r`, `alt+x`,
`shift-g` - which is the same key as `G`. `+` separates as well as `-`, and a lone `-` or
`+` is the key itself rather than a separator.

`?` and the line along the bottom edge show the keys as they are actually bound, so a
remapped layout documents itself. Two things stay where they are: `ctrl-c` always quits,
and the search box takes every letter literally, so `/` then `q` searches for `q`.

The language list uses the same bindings: `up`/`down`/`top`/`bottom` move, `open`
applies, `next-column` swaps between the audio and subtitle lists, and `back` or `quit`
closes it.

The mouse has nothing of its own in here. Every word it can click runs one of the
commands above, so moving a key moves what the word beside it says and changes nothing
else - see [With a mouse](#with-a-mouse).

A command that does not exist is reported on the status line with the config file left
unread, the way a misspelt theme key is. Taking a command's last key away for something
else is reported too, and otherwise allowed.

### Colours

The interface ships with no colours of its own. It draws with the sixteen palette slots
your terminal already resolves - yellow, grey, red - so it arrives wearing whatever
colourscheme is installed and needs no configuration to match it. If you drive your
palette with base16-shell, tinted-theming or anything else that recolours the sixteen
slots, this follows it already.

To pin the colours anyway:

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
is reported on the status line and otherwise ignored.

## Tests

```shell
cargo test
cargo clippy --all-targets -- -D warnings
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
