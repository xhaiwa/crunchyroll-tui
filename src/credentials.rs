use std::env;
use std::fmt;
use std::process::{Command, Stdio};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

use crate::config::{self, Config};

/// The environment variable the cookie is read from.
pub const ENV_VAR: &str = "CRUNCHYROLL_ETP_RT";

static UUID: Lazy<Regex> = Lazy::new(|| {
    Regex::new("^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$").expect("valid UUID regex")
});

/// A value that must not reach the screen.
///
/// The etp_rt cookie is a session: whoever reads it is signed in as the account it came
/// from until it is revoked. `Debug` therefore prints that there is something rather
/// than what it is, so a stray `{:?}` - of the parsed arguments, of the config, of a
/// struct in a panic - cannot put the cookie in a screenshot or an asciinema recording.
/// There is deliberately no `Display`: the one place that needs the value asks for it by
/// name.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value itself, for the cookie header that has to carry it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

impl FromStr for Secret {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_owned()))
    }
}

/// Where a cookie came from, so a complaint can name the thing to go and fix without
/// quoting the thing that is wrong with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Flag,
    Environment,
    ConfigFile,
    Command,
}

impl Source {
    fn describe(self) -> String {
        match self {
            Source::Flag => "--etp-rt".to_owned(),
            Source::Environment => format!("${ENV_VAR}"),
            Source::ConfigFile => "etp_rt in the config file".to_owned(),
            Source::Command => "the first line of etp_rt_command".to_owned(),
        }
    }
}

/// What the sources between them have to offer: a cookie, or the command that prints
/// one.
enum Choice<'a> {
    Value(Source, &'a str),
    Command(&'a str),
}

/// Picks the source to use. The flag wins, so a one-off can override the rest; then the
/// environment, which unlike an argument is not world-readable in `/proc` and does not
/// land in the shell's history; then the file's own value; and last the command, which
/// is the only one that costs a subprocess and so is only reached when nothing else
/// answered.
fn select<'a>(
    flag: Option<&'a str>,
    environment: Option<&'a str>,
    config: &'a Config,
) -> Option<Choice<'a>> {
    let present = |value: Option<&'a str>| value.map(str::trim).filter(|value| !value.is_empty());
    if let Some(value) = present(flag) {
        return Some(Choice::Value(Source::Flag, value));
    }
    if let Some(value) = present(environment) {
        return Some(Choice::Value(Source::Environment, value));
    }
    if let Some(value) = present(config.etp_rt.as_ref().map(Secret::expose)) {
        return Some(Choice::Value(Source::ConfigFile, value));
    }
    present(config.etp_rt_command.as_deref()).map(Choice::Command)
}

/// Finds the cookie, and says what was worth saying about how it was found. The
/// complaints are returned rather than printed because the TUI owns the terminal and
/// shows them on its status line instead.
pub fn resolve(flag: Option<&Secret>, config: &Config) -> Result<(Secret, Vec<String>)> {
    let environment = env::var(ENV_VAR).ok();
    let Some(choice) = select(flag.map(Secret::expose), environment.as_deref(), config) else {
        bail!(missing());
    };
    let (source, cookie) = match choice {
        Choice::Value(source, value) => (source, value.to_owned()),
        Choice::Command(command) => (Source::Command, from_command(command)?),
    };

    let mut warnings = Vec::new();
    match source {
        Source::Flag => warnings.push(format!(
            "! --etp-rt leaves the cookie in the shell's history and in any recording of this terminal. ${ENV_VAR}, or etp_rt_command in the config file, keeps it out of both."
        )),
        Source::ConfigFile => warnings.extend(world_readable()),
        _ => {}
    }
    warnings.extend(check(&cookie, source)?);
    Ok((Secret::new(cookie), warnings))
}

/// Runs `etp_rt_command` and takes the cookie off the first line of its output.
///
/// The command goes through `sh` so a pipeline is allowed, and stdin and stderr stay on
/// the terminal, because a `pass` that has to ask gpg for a passphrase needs a tty to
/// ask on. Only stdout is captured, and none of it is ever quoted back: it is the
/// secret.
fn from_command(command: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("run etp_rt_command ({command})"))?
        .wait_with_output()
        .with_context(|| format!("wait for etp_rt_command ({command})"))?;
    if !output.status.success() {
        bail!("etp_rt_command ({command}) failed: {}", output.status);
    }
    let printed = String::from_utf8(output.stdout).with_context(|| {
        format!("etp_rt_command ({command}) printed something that is not text")
    })?;
    // `pass show` prints the whole entry with the secret on the first line, which is the
    // shape every other tool that reads a password out of a command expects.
    let cookie = printed.lines().next().unwrap_or_default().trim();
    if cookie.is_empty() {
        bail!("etp_rt_command ({command}) printed nothing");
    }
    Ok(cookie.to_owned())
}

/// The etp_rt cookie is a single UUID. A wrong one only shows up as an opaque 403 from
/// the token endpoint, so name the likely mistake before the request goes out - without
/// quoting the value, since these are printed on the terminal the cookie is being kept
/// off.
fn check(value: &str, source: Source) -> Result<Option<String>> {
    if UUID.is_match(value) {
        return Ok(None);
    }
    // Double-clicking the cookie value in dev tools selects it twice often enough to be
    // worth calling out by name. This one is provably wrong, so it is worth refusing.
    if value.len() == 72
        && let Some(half) = value.get(..36)
        && UUID.is_match(half)
        && value.ends_with(half)
    {
        bail!(
            "{} is the cookie twice over; it should be the first 36 characters of what you pasted.",
            source.describe()
        );
    }
    // Any other shape is only a guess on our part, so say so and carry on rather than
    // blocking on an assumption about a format Crunchyroll is free to change.
    Ok(Some(format!(
        "! {} does not look like an etp_rt cookie (expected a 36-character UUID, got {} characters). Trying it anyway.",
        source.describe(),
        value.chars().count()
    )))
}

/// A config file holding the cookie should be no more readable than a private key. This
/// only says so: refusing to read a file the user wrote themselves would be worse than
/// the risk.
fn world_readable() -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    let path = config::path()?;
    let mode = path.metadata().ok()?.permissions().mode();
    (mode & 0o077 != 0).then(|| {
        format!(
            "! {} holds the etp_rt cookie and can be read by other users; chmod 600 it.",
            path.display()
        )
    })
}

fn missing() -> String {
    let path = config::path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the config file".to_owned());
    format!(
        "No etp_rt cookie. Put `etp_rt_command = \"pass show crunchyroll\"`, or `etp_rt = \"...\"`, at the top of {path}, or set ${ENV_VAR}. --etp-rt takes one too, at the price of leaving it in the shell's history."
    )
}

#[cfg(test)]
mod tests {
    use super::{Choice, Config, Secret, Source, check, from_command, select};

    const COOKIE: &str = "e70b3d61-b8bc-4ecb-a9b3-1cf1f0a1b0d1";

    fn config(etp_rt: Option<&str>, command: Option<&str>) -> Config {
        Config {
            etp_rt: etp_rt.map(Secret::new),
            etp_rt_command: command.map(str::to_owned),
            ..Config::default()
        }
    }

    #[track_caller]
    fn value(choice: Option<Choice<'_>>) -> (Source, String) {
        match choice.expect("a source") {
            Choice::Value(source, value) => (source, value.to_owned()),
            Choice::Command(command) => (Source::Command, command.to_owned()),
        }
    }

    #[test]
    fn prefers_the_flag_then_the_environment_then_the_file_then_the_command() {
        let full = config(Some("from-file"), Some("from-command"));
        assert_eq!(
            value(select(Some("from-flag"), Some("from-env"), &full)),
            (Source::Flag, "from-flag".to_owned())
        );
        assert_eq!(
            value(select(None, Some("from-env"), &full)),
            (Source::Environment, "from-env".to_owned())
        );
        assert_eq!(
            value(select(None, None, &full)),
            (Source::ConfigFile, "from-file".to_owned())
        );
        assert_eq!(
            value(select(None, None, &config(None, Some("from-command")))),
            (Source::Command, "from-command".to_owned())
        );
        assert!(select(None, None, &config(None, None)).is_none());
    }

    /// An exported-but-empty variable, or a flag given an empty string, is a source that
    /// is not set rather than a cookie that is empty.
    #[test]
    fn steps_over_a_source_that_is_only_whitespace() {
        let file = config(Some("from-file"), None);
        assert_eq!(
            value(select(Some("  "), Some(""), &file)),
            (Source::ConfigFile, "from-file".to_owned())
        );
    }

    #[test]
    fn takes_the_cookie_off_the_first_line_of_the_command() {
        let printed = from_command("printf '%s\\nsome notes\\n' 'the-cookie'").expect("output");
        assert_eq!(printed, "the-cookie");
    }

    /// The command is named so the failure can be found and fixed - it holds the name of
    /// the entry, not the secret - but whatever it printed before it failed is not
    /// repeated back: on a `pass` that half-succeeded that would be the cookie.
    #[test]
    fn refuses_a_command_that_fails_without_quoting_what_it_printed() {
        let error =
            from_command("printf 'the-cookie' | tr 'a-z' 'A-Z'; exit 3").expect_err("a failure");
        let message = format!("{error:#}");
        assert!(!message.contains("THE-COOKIE"), "{message}");
        assert!(message.contains("exit status: 3"), "{message}");
    }

    #[test]
    fn refuses_a_command_that_prints_nothing() {
        assert!(from_command("true").is_err());
    }

    #[test]
    fn refuses_only_a_doubled_cookie_and_never_prints_it() {
        assert!(
            check(COOKIE, Source::Flag)
                .expect("a plain cookie")
                .is_none()
        );

        let error = check(&format!("{COOKIE}{COOKIE}"), Source::Flag).expect_err("doubled");
        let message = format!("{error:#}");
        assert!(!message.contains(COOKIE), "{message}");

        // Two cookies that are not the same one, and anything else, are only guesses on
        // our part: they are worth a word and no more.
        let warning = check(
            &format!("{COOKIE}00000000-0000-0000-0000-000000000000"),
            Source::Environment,
        )
        .expect("not refused");
        assert!(warning.is_some_and(|warning| !warning.contains(COOKIE)));
        assert!(check("some-other-format", Source::Command).is_ok());
    }

    #[test]
    fn keeps_the_cookie_out_of_debug_output() {
        let printed = format!("{:?}", config(Some(COOKIE), None));
        assert!(!printed.contains(COOKIE), "{printed}");
    }
}
