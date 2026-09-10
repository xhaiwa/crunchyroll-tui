//! Video drawn in the terminal that started the program.
//!
//! mpv can put the picture in a terminal instead of a window, but only if it is told
//! which of the graphics protocols the terminal in front of it speaks, and only if it is
//! kept from spending its frame budget on scaling nobody asked for. Both are mpv options
//! that a reading of its manual turns up and nothing else does, which is what
//! `--in-terminal` is for: the terminal is asked what it can draw, and the options that
//! go with the answer are put in front of whatever else was passed.

use std::env;
use std::io::{IsTerminal, stdin, stdout};

use ratatui_image::picker::{Picker, ProtocolType};

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
    options(protocol, over_ssh())
}

/// `--vo` takes a list and uses the first output that starts, so every one of these ends
/// in `tct` - true colour drawn as text, which needs nothing of the terminal and nothing
/// of the build. A distribution's mpv without sixel compiled in is common enough to be
/// worth the four characters.
fn options(protocol: ProtocolType, over_ssh: bool) -> (Vec<String>, Option<String>) {
    let mut args = Vec::new();
    let mut warning = None;
    match protocol {
        ProtocolType::Kitty => {
            args.push("--vo=kitty,tct".to_owned());
            // Shared memory hands the terminal a file rather than a screenful of escape
            // sequences, which is most of what lets kitty output keep up with a frame
            // rate. The terminal has to be able to open that file, though, so there is
            // only something to hand over when it is on this machine.
            if !over_ssh {
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
    use super::{ProtocolType, options};

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
            let (args, _) = options(protocol, false);
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
        assert!(options(ProtocolType::Kitty, false).1.is_none());
        let (_, warning) = options(ProtocolType::Halfblocks, false);
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
        assert!(options(ProtocolType::Kitty, false).0.contains(&shm));
        assert!(!options(ProtocolType::Kitty, true).0.contains(&shm));
        // It is a kitty option, so it has no business being passed for anything else.
        assert!(!options(ProtocolType::Sixel, false).0.contains(&shm));
    }
}
