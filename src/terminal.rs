//! Video drawn in the terminal that started the program.
//!
//! mpv can put the picture in a terminal instead of a window, but only if it is told
//! which of the graphics protocols the terminal in front of it speaks, only if it is
//! kept from spending its frame budget on scaling nobody asked for, and only if it is
//! stopped from drawing a picture out of more character cells than the terminal can
//! paint in the time a frame lasts. All three are mpv options that a reading of its
//! manual turns up and nothing else does, which is what `--in-terminal` is for: the
//! terminal is asked what it can draw and how big it is, and the options that go with
//! the answers are put in front of whatever else was passed.

use std::env;
use std::io::{IsTerminal, stdin, stdout};

use ratatui::crossterm::terminal::size as terminal_size;
use ratatui_image::picker::{Picker, ProtocolType};

/// Drawn as text, a frame costs around 35 bytes for every character cell it covers -
/// measured, and near enough the same at every size, because each cell carries its own
/// colour and its own half block. A terminal filling a large screen is some 16000
/// cells, so 13 MB/s of escape sequences at 24 frames a second for the terminal to
/// parse, lay out and paint. No terminal keeps up with that, and the frames mpv drops
/// waiting for the ones that are still going out are the picture stuttering. 4000 cells
/// is nearer 3 MB/s, which they do keep up with.
const DRAWN_CELLS: u32 = 4_000;

/// Asks the terminal what it can draw, for a run with no interface to ask on its behalf.
///
/// The question is escape sequences written to stdout and the answer comes back off
/// stdin, so there is nothing to ask when either of them is a pipe - and nothing to draw
/// into either.
pub fn detect() -> ProtocolType {
    if !stdout().is_terminal() || !stdin().is_terminal() {
        return ProtocolType::Halfblocks;
    }
    // A terminal that will not answer is one that draws no graphics, which is what
    // `Halfblocks` amounts to here.
    Picker::from_query_stdio()
        .map(|picker| picker.protocol_type())
        .unwrap_or(ProtocolType::Halfblocks)
}

/// The mpv options that put the picture inside a terminal speaking `protocol`, and
/// anything the user should hear about the answer it gave.
pub fn mpv_args(protocol: ProtocolType) -> (Vec<String>, Option<String>) {
    options(protocol, over_ssh(), terminal_size().ok())
}

/// The box mpv is told to draw into, in character cells, or `None` for a terminal that
/// is already small enough to paint a frame in the time a frame lasts.
///
/// Both sides shrink by the same amount, so the box keeps the shape of the terminal and
/// mpv fits the video inside it exactly as it would have. All that changes is how many
/// cells the picture is made of: the same picture with larger pixels, which is the one
/// thing here that buys back a frame rate.
fn drawn_size(columns: u16, rows: u16) -> Option<(u16, u16)> {
    let cells = u32::from(columns) * u32::from(rows);
    if cells <= DRAWN_CELLS {
        return None;
    }
    let shrink = (f64::from(DRAWN_CELLS) / f64::from(cells)).sqrt();
    // Rounded down on both sides rather than to nearest, so that two roundings up cannot
    // between them hand back a box worth more cells than the budget it came from.
    let scale = |side: u16| ((f64::from(side) * shrink) as u16).max(1);
    Some((scale(columns), scale(rows)))
}

/// `--vo` takes a list and uses the first output that starts, so every one of these ends
/// in `tct` - true colour drawn as text, which needs nothing of the terminal and nothing
/// of the build. A distribution's mpv without sixel compiled in is common enough to be
/// worth the four characters.
fn options(
    protocol: ProtocolType,
    over_ssh: bool,
    size: Option<(u16, u16)>,
) -> (Vec<String>, Option<String>) {
    let mut args = Vec::new();
    let mut warning = None;
    // Shared memory hands the terminal a file rather than a screenful of escape
    // sequences, which is most of what lets kitty output keep up with a frame rate. The
    // terminal has to be able to open that file, though, so there is only something to
    // hand over when it is on this machine.
    let shared_memory = protocol == ProtocolType::Kitty && !over_ssh;
    match protocol {
        ProtocolType::Kitty => {
            args.push("--vo=kitty,tct".to_owned());
            if shared_memory {
                args.push("--vo-kitty-use-shm=yes".to_owned());
            }
        }
        // mpv has no output of its own for the iTerm2 protocol, and every terminal that
        // speaks it - iTerm2, WezTerm, Konsole - speaks sixel as well.
        ProtocolType::Sixel | ProtocolType::Iterm2 => args.push("--vo=sixel,tct".to_owned()),
        ProtocolType::Halfblocks => {
            args.push("--vo=tct".to_owned());
            warning = Some(
                "--in-terminal: this terminal answers to no graphics protocol, \
                 so the video is drawn as coloured text."
                    .to_owned(),
            );
        }
    }
    // Whichever one it ends up being, the frames are scaled on the CPU and then written
    // out as text, so the scaler is what decides whether the picture keeps up at all.
    // `sw-fast` is mpv's own name for the cheap one.
    args.push("--profile=sw-fast".to_owned());
    if let Some((columns, rows)) = size.and_then(|(columns, rows)| drawn_size(columns, rows)) {
        // `tct` is what every answer above falls back to and is compiled into every mpv
        // there is, so its size is worth setting whichever output ends up starting.
        args.push(format!("--vo-tct-width={columns}"));
        args.push(format!("--vo-tct-height={rows}"));
        // Down a shared memory file the size costs nothing on the wire, and the picture
        // is only better for being drawn out of more of them.
        if protocol == ProtocolType::Kitty && !shared_memory {
            args.push(format!("--vo-kitty-cols={columns}"));
            args.push(format!("--vo-kitty-rows={rows}"));
        }
        // Sixel gets no size of its own: `--vo-sixel-*` is an unknown option to an mpv
        // built without libsixel, and mpv treats an unknown option as a fatal error
        // rather than ignoring it. That build is exactly the one that falls through to
        // `tct`, which has its size set above.
    }
    (args, warning)
}

/// Whether the terminal is at the far end of an ssh connection, and so on a machine that
/// shares no memory with this one.
fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::{DRAWN_CELLS, ProtocolType, drawn_size, options};

    /// A terminal too small to be worth shrinking, so that a test says what it means to
    /// say and nothing about sizes.
    const SMALL: Option<(u16, u16)> = Some((80, 24));

    /// Every answer has to name an output mpv actually has, and every one of them has to
    /// leave a way out for a build that was compiled without it.
    #[test]
    fn each_protocol_gets_the_output_mpv_knows_it_by() {
        for (protocol, expected) in [
            (ProtocolType::Kitty, "--vo=kitty,tct"),
            (ProtocolType::Sixel, "--vo=sixel,tct"),
            (ProtocolType::Iterm2, "--vo=sixel,tct"),
            (ProtocolType::Halfblocks, "--vo=tct"),
        ] {
            let (args, _) = options(protocol, false, SMALL);
            assert_eq!(
                args.first().map(String::as_str),
                Some(expected),
                "{protocol:?}"
            );
            assert!(
                args.contains(&"--profile=sw-fast".to_owned()),
                "{protocol:?} is scaled on the CPU like every other one"
            );
        }
    }

    /// A terminal that draws no graphics still gets a picture, and the user gets to hear
    /// why it looks like that rather than being left to wonder.
    #[test]
    fn only_a_terminal_without_graphics_is_worth_a_word() {
        assert!(options(ProtocolType::Kitty, false, SMALL).1.is_none());
        let (_, warning) = options(ProtocolType::Halfblocks, false, SMALL);
        assert!(
            warning.is_some_and(|warning| warning.contains("--in-terminal")),
            "the warning names the flag it is about"
        );
    }

    /// Shared memory over ssh is a file the terminal cannot open, which mpv reports as a
    /// video output that will not start rather than as a picture that never arrives.
    #[test]
    fn shared_memory_is_left_out_over_ssh() {
        let shm = "--vo-kitty-use-shm=yes".to_owned();
        assert!(options(ProtocolType::Kitty, false, SMALL).0.contains(&shm));
        assert!(!options(ProtocolType::Kitty, true, SMALL).0.contains(&shm));
        // It is a kitty option, so it has no business being passed for anything else.
        assert!(!options(ProtocolType::Sixel, false, SMALL).0.contains(&shm));
    }

    /// The picture gets smaller until the terminal can paint it, and keeps the shape of
    /// the terminal while it does - a box of another shape would only letterbox.
    #[test]
    fn a_terminal_too_big_to_paint_is_drawn_out_of_fewer_cells() {
        let (columns, rows) = drawn_size(240, 68).expect("far too many cells to paint");
        let cells = u32::from(columns) * u32::from(rows);
        assert!(cells <= DRAWN_CELLS, "{columns}x{rows} is {cells} cells");
        // Within the rounding of two whole numbers of cells.
        let shape = |wide: f64, high: f64| wide / high;
        assert!(
            (shape(240.0, 68.0) - shape(columns.into(), rows.into())).abs() < 0.1,
            "{columns}x{rows} is not the shape of 240x68"
        );
    }

    /// A terminal already small enough is left at its own size, rather than being given
    /// back a picture with fewer cells than it asked for.
    #[test]
    fn a_terminal_that_keeps_up_is_left_alone() {
        assert_eq!(drawn_size(80, 24), None);
        let (args, _) = options(ProtocolType::Halfblocks, false, SMALL);
        assert!(
            !args.iter().any(|arg| arg.starts_with("--vo-tct-width")),
            "{args:?}"
        );
        // Asking the terminal its size can fail, and a size nobody knows caps nothing.
        let (args, _) = options(ProtocolType::Halfblocks, false, None);
        assert!(
            !args.iter().any(|arg| arg.starts_with("--vo-tct-")),
            "{args:?}"
        );
    }

    /// `tct` is the fallback under every protocol, so it is sized whichever one was
    /// detected. kitty is sized only when its frames go out as escape sequences: down a
    /// shared memory file the cells are free, and fewer of them would only be blurrier.
    #[test]
    fn every_output_that_writes_its_frames_out_is_sized() {
        let big = Some((240, 68));
        for protocol in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
            ProtocolType::Halfblocks,
        ] {
            let (args, _) = options(protocol, false, big);
            assert!(
                args.iter().any(|arg| arg.starts_with("--vo-tct-width="))
                    && args.iter().any(|arg| arg.starts_with("--vo-tct-height=")),
                "{protocol:?} falls back to tct: {args:?}"
            );
        }
        let sized = |args: &[String]| args.iter().any(|arg| arg.starts_with("--vo-kitty-cols="));
        assert!(!sized(&options(ProtocolType::Kitty, false, big).0));
        assert!(sized(&options(ProtocolType::Kitty, true, big).0));
        // Sizing an output this mpv may not have been built with is a fatal error.
        assert!(
            !options(ProtocolType::Sixel, false, big)
                .0
                .iter()
                .any(|arg| arg.starts_with("--vo-sixel-"))
        );
    }
}
