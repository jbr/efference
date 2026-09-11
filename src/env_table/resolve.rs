//! Where an `[env]` value comes from (docs/env.md): a literal, or a **reference**
//! resolved by running a command whose stdout is the value.
//!
//! `cmd` is the primitive — one exec-for-a-secret indirection — and a scheme is
//! sugar over it: [`SCHEMES`] maps a URI scheme to a command template, so a bare
//! `NAME = "op://Vault/Item/field"` resolves through `op read`. Only registered
//! schemes trigger resolution, so `postgres://…` written as a bare string stays a
//! literal; `{ value = "op://…" }` is the escape hatch that forces a literal even
//! for a registered scheme.
//!
//! Resolution is eager (boot, and again when a config edit is adopted) and cached
//! by command string for the session, so a vault that prompts (1Password's Touch
//! ID) prompts once at session open rather than inside a turn.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::EnvEntry;

/// Registered reference schemes: prefix → command template, `{ref}` replaced by
/// the whole reference, shell-quoted. Adding a vault is one entry here.
///
/// `op://` is 1Password's own secret-reference convention, which is why it is the
/// one registered form: `op read` accepts exactly the string an operator already
/// keeps in their notes. Other real conventions exist (`berglas://`, `sm://`, AWS
/// secretsmanager ARNs); each is one line when someone runs one.
pub const SCHEMES: &[(&str, &str)] = &[("op://", "op read {ref}")];

/// How long a resolution command may run before it is killed. Generous because a
/// biometric prompt waits on a human; finite because the boot (and a config-edit
/// refresh) is synchronous behind it.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(120);

/// How much of a failed command's stderr is quoted back in the error.
const STDERR_QUOTED: usize = 400;

/// One `[env]` value the resolver produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedValue {
    pub name: String,
    pub value: String,
    /// True when the value came out of a command — a reference, i.e. something
    /// the operator declared to be a secret. Forces masking on
    /// ([`super::EnvPolicy`]); a literal answers the table's ordinary rules.
    pub secret: bool,
}

/// A reference that did not resolve. Carries the **reference**, never a value —
/// a pointer is safe to print, which is what makes failing loudly possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    pub name: String,
    pub reference: String,
    pub detail: String,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` = `{}` did not resolve: {}",
            self.name, self.reference, self.detail
        )
    }
}

/// What an [`EnvEntry`] says its value is, once the bare-string form has been
/// sniffed against [`SCHEMES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Used verbatim; no command runs.
    Literal(String),
    /// The value is this command's stdout. `reference` is what the operator
    /// wrote (the `op://…` string, or the command itself) — the safe-to-print
    /// half.
    Command { command: String, reference: String },
}

impl EnvEntry {
    /// The entry's value source, or `None` for an entry that only answers
    /// `mask`/`strip`. `cmd` wins over `value`, and both win over the bare
    /// string — an entry that names more than one is a resolution error rather
    /// than a silent pick, reported by [`Resolver::resolve_table`].
    pub fn source(&self) -> Option<Source> {
        if let Some(cmd) = &self.cmd {
            return Some(Source::Command {
                command: cmd.clone(),
                reference: cmd.clone(),
            });
        }
        if let Some(value) = &self.value {
            return Some(Source::Literal(value.clone()));
        }
        let bare = self.bare.as_ref()?;
        Some(match scheme_command(bare) {
            Some(command) => Source::Command {
                command,
                reference: bare.clone(),
            },
            None => Source::Literal(bare.clone()),
        })
    }

    /// True when the entry names two value fields at once.
    fn over_specified(&self) -> bool {
        self.cmd.is_some() && self.value.is_some()
    }
}

/// The command a bare string resolves through, or `None` when no registered
/// scheme matches (`postgres://…` is a literal, not a reference).
pub fn scheme_command(reference: &str) -> Option<String> {
    let (_, template) = SCHEMES
        .iter()
        .find(|(scheme, _)| reference.starts_with(scheme))?;
    Some(template.replace("{ref}", &shell_quote(reference)))
}

/// Single-quote a string for `sh -c`, so a reference is one word whatever it
/// contains.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Runs one resolution command and returns its stdout, or a printable failure.
/// The seam tests replace — a test must never invoke a real vault binary.
pub type Runner = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// The session's reference resolver: the command runner plus the
/// resolved-value cache, keyed by command string.
///
/// The cache is what makes eager resolution cheap to repeat: `serve` resolves the
/// origin config's table (to find its own credential) and then the session
/// snapshot's identical table, and a config edit re-resolves the whole table —
/// all of which hit the cache for every unchanged reference, so a vault prompts
/// only for references it has not seen.
pub struct Resolver {
    run: Runner,
    cache: Mutex<BTreeMap<String, String>>,
}

impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver")
            .field("cached", &self.cache.lock().unwrap().len())
            .finish()
    }
}

impl Default for Resolver {
    fn default() -> Self {
        match std::env::current_dir() {
            Ok(cwd) => Self::at_cwd(cwd),
            Err(e) => Self::new(Box::new(move |_| {
                Err(format!("could not resolve cwd: {e}"))
            })),
        }
    }
}

impl Resolver {
    /// Reference commands run in this session root, including later config edits (docs/env.md).
    pub fn at_cwd(cwd: std::path::PathBuf) -> Self {
        match super::EnvSnapshot::capture() {
            Ok(base) => Self::with_environment(cwd, base),
            Err(error) => Self::new(Box::new(move |_| Err(error.clone()))),
        }
    }

    /// Fix both command cwd and environment for boot and later edits (docs/env.md).
    pub fn with_environment(cwd: std::path::PathBuf, base: super::EnvSnapshot) -> Self {
        assert!(cwd.is_absolute(), "resolver cwd must be absolute");
        Self::new(Box::new(move |command| {
            run_command(&cwd, &base, command, RESOLVE_TIMEOUT)
        }))
    }

    pub fn new(run: Runner) -> Self {
        Self {
            run,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    /// Resolve every value-bearing entry in the table. Returns what resolved and
    /// what failed; the caller decides how loud a failure is (boot refuses to
    /// start, a config edit is rejected back to its author).
    pub fn resolve_table(
        &self,
        user: &BTreeMap<String, EnvEntry>,
    ) -> (Vec<ResolvedValue>, Vec<ResolveError>) {
        let mut values = Vec::new();
        let mut errors = Vec::new();
        for (name, entry) in user {
            if entry.over_specified() {
                errors.push(ResolveError {
                    name: name.clone(),
                    reference: "value + cmd".to_string(),
                    detail: "names both `value` and `cmd`; a value has one source".to_string(),
                });
                continue;
            }
            match entry.source() {
                None => continue,
                Some(Source::Literal(value)) => values.push(ResolvedValue {
                    name: name.clone(),
                    value,
                    secret: false,
                }),
                Some(Source::Command { command, reference }) => {
                    match self.resolve_command(&command) {
                        Ok(value) => values.push(ResolvedValue {
                            name: name.clone(),
                            value,
                            secret: true,
                        }),
                        Err(detail) => errors.push(ResolveError {
                            name: name.clone(),
                            reference,
                            detail,
                        }),
                    }
                }
            }
        }
        (values, errors)
    }

    /// The cached run of one command. A failure is not cached: the operator can
    /// fix the vault (unlock it, log in) and re-trigger by touching the config.
    fn resolve_command(&self, command: &str) -> Result<String, String> {
        if let Some(hit) = self.cache.lock().unwrap().get(command) {
            return Ok(hit.clone());
        }
        let value = (self.run)(command)?;
        self.cache
            .lock()
            .unwrap()
            .insert(command.to_string(), value.clone());
        Ok(value)
    }
}

/// Run `command` under `sh -c` and return its stdout with trailing newlines
/// removed (`op read` and `security -w` both emit one).
///
/// The pipes are drained on their own threads so a chatty command cannot deadlock
/// on a full buffer, and the child is killed if it outlives `timeout` rather than
/// parking the boot forever. Stderr is quoted back in the failure — a vault's
/// diagnostic is the thing an operator needs, and this runs before any masking
/// exists, so it is capped rather than unbounded.
pub fn run_command(
    cwd: &std::path::Path,
    base: &super::EnvSnapshot,
    command: &str,
    timeout: Duration,
) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("sh");
    base.apply(&mut cmd);
    let mut child = cmd
        .current_dir(cwd)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run `{command}`: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let drain = |pipe: &mut dyn Read| {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        buf
    };
    std::thread::scope(|scope| {
        let out = scope.spawn(move || drain(&mut stdout));
        let err = scope.spawn(move || drain(&mut stderr));
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!(
                        "`{command}` did not finish within {}s and was killed",
                        timeout.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => break Err(format!("could not wait for `{command}`: {e}")),
            }
        };
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        match status {
            Err(e) => Err(e),
            Ok(status) if status.success() => Ok(stdout.trim_end_matches(['\n', '\r']).to_string()),
            Ok(status) => Err(format!(
                "`{command}` exited {}: {}",
                status.code().unwrap_or(-1),
                truncate(stderr.trim(), STDERR_QUOTED)
            )),
        }
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

#[cfg(test)]
mod test;
