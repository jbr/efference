//! The command backend (docs/secrets.md): the store as the `get`, `set` and `delete`
//! commands the user configuration's `[secrets]` table names.
//!
//! The item's name is an argument, substituted for `{name}`; the value never is, since
//! a command line is visible to every process on the machine. `set` reads the value on
//! stdin, and `get` prints it on stdout, one trailing newline trimmed. Absence is the
//! exit status the table declares, never inferred from a failure: any other non-zero
//! exit is an error carrying the command's stderr.

use std::io::Write as _;
use std::num::NonZeroU8;
use std::process::{Command, Output, Stdio};

use serde::Deserialize;

#[cfg(test)]
mod test;

/// What each argument has replaced by the item's name.
const PLACEHOLDER: &str = "{name}";

/// `[secrets]`: keep the secrets efference writes (sign-ins, the web interface's root
/// secret, keys pasted into setup) with commands you name instead of the OS keychain,
/// as on a server with no Secret Service. With the table present the keychain is never
/// opened. `{name}` in an argument is replaced by the item's name, such as
/// `oauth:codex` or `root`; a value travels on stdin and stdout, never in an argument.
/// Any program running as you can run the same `get`.
// The table as written. The `///` docs here are the published user schema's hover text
// (docs/config-schemas.md); `InferenceConfig::from_toml` calls `validate`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Secrets")]
pub struct SecretCommands {
    /// The program and arguments that print an item's value on stdout, as in
    /// `["pass", "show", "efference/{name}"]`. One trailing newline is trimmed. It runs
    /// directly, without a shell.
    get: Vec<String>,
    /// The program and arguments that store an item, replacing any previous value. The
    /// value arrives on stdin, with no newline added, as in
    /// `["pass", "insert", "--multiline", "--force", "efference/{name}"]`.
    set: Vec<String>,
    /// The program and arguments that remove an item. Optional: without it, an
    /// operation that removes one (an MCP server's `--reauth`, a connection renamed in
    /// `efference setup`) fails, naming the item to remove by hand.
    #[serde(default)]
    delete: Option<Vec<String>>,
    /// The exit status `get` gives for an item that does not exist, such as `254` for
    /// `aws ssm` or `1` for `secret-tool`; `delete` exiting with it is a success too.
    /// Unset, every failure of `get` is an error, including the first read of an item
    /// efference has not stored yet.
    #[serde(default)]
    absent_status: Option<NonZeroU8>,
}

impl SecretCommands {
    /// Refuse a table whose commands could not address an item: an empty argv names no
    /// program, and one without `{name}` would read and write every item as one value,
    /// so storing a sign-in would overwrite the web root.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let commands = [
            ("get", Some(&self.get)),
            ("set", Some(&self.set)),
            ("delete", self.delete.as_ref()),
        ];
        for (key, argv) in commands {
            let Some(argv) = argv else { continue };
            if argv.is_empty() {
                return Err(format!("[secrets] {key} = [] names no program to run"));
            }
            if !argv.iter().any(|arg| arg.contains(PLACEHOLDER)) {
                return Err(format!(
                    "[secrets] {key} has no {PLACEHOLDER} argument, so every item would be \
                     the same one; put {PLACEHOLDER} where the item's name goes"
                ));
            }
        }
        Ok(())
    }

    pub(super) fn get(&self, name: &str) -> Result<Option<String>, String> {
        let output = run(&self.get, name, None)?;
        if self.is_absent(&output) {
            return Ok(None);
        }
        succeeded(&self.get, &output)?;
        let mut value =
            String::from_utf8(output.stdout).map_err(|_| "the value is not UTF-8".to_string())?;
        if value.ends_with('\n') {
            value.pop();
        }
        Ok(Some(value))
    }

    pub(super) fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let output = run(&self.set, name, Some(value))?;
        succeeded(&self.set, &output)
    }

    /// Absent is not an error, here as on every backend.
    pub(super) fn delete(&self, name: &str) -> Result<(), String> {
        let Some(delete) = &self.delete else {
            return Err("[secrets] has no `delete` command; remove the item by hand".to_string());
        };
        let output = run(delete, name, None)?;
        if self.is_absent(&output) {
            return Ok(());
        }
        succeeded(delete, &output)
    }

    fn is_absent(&self, output: &Output) -> bool {
        self.absent_status
            .is_some_and(|status| output.status.code() == Some(i32::from(status.get())))
    }
}

/// Run `argv` for the item `name`, with `input` on stdin when there is one and nothing
/// on it otherwise: a host's own stdin can be an island's ownership pipe
/// (docs/operating.md), which no command should read.
fn run(argv: &[String], name: &str, input: Option<&str>) -> Result<Output, String> {
    let argv: Vec<String> = argv
        .iter()
        .map(|arg| arg.replace(PLACEHOLDER, name))
        .collect();
    let (program, args) = argv
        .split_first()
        .ok_or("[secrets] names an empty command")?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::platform::spawn::std(&mut command)
        .map_err(|error| format!("could not run `{program}`: {error}"))?;
    let stdin = child.stdin.take();
    // The value is written beside the wait rather than before it, so a command that
    // fills its stdout or stderr before reading stdin cannot deadlock against us.
    std::thread::scope(|scope| {
        let writer = stdin
            .zip(input)
            .map(|(mut stdin, value)| scope.spawn(move || stdin.write_all(value.as_bytes())));
        let output = child
            .wait_with_output()
            .map_err(|error| format!("could not wait for `{program}`: {error}"))?;
        let written = writer.map(std::thread::ScopedJoinHandle::join);
        // A command that exits without reading its input reports its own failure,
        // which says more than the broken pipe would.
        if output.status.success()
            && let Some(Ok(Err(error))) = written
        {
            return Err(format!("could not write the value to `{program}`: {error}"));
        }
        Ok(output)
    })
}

/// `Ok` for a zero exit; otherwise the command, how it ended, and its stderr.
fn succeeded(argv: &[String], output: &Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let program = argv.first().map_or_default(String::as_str);
    let ended = match output.status.code() {
        Some(code) => format!("exited with status {code}"),
        None => "was ended by a signal".to_string(),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(if stderr.is_empty() {
        format!("`{program}` {ended} and printed nothing on stderr")
    } else {
        format!("`{program}` {ended}: {stderr}")
    })
}
