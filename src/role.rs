//! Role config — the cascade's foundational unit (backlog/permissions/role-config.md).
//!
//! A session is parameterized by `(role-config, cwd, orchestrator)`. The **role** is
//! the portable-policy half: what a session is *for* and what it may do, reusable
//! across projects and sessions. It is deliberately kept separate from `cwd` (the
//! per-invocation spatial scope, see [`crate::env`]) so the role stays portable —
//! tweak in place or fork by copy.
//!
//! The file is KDL, parsed into the typed structs below by [`knus`]. The types *are*
//! the schema; a malformed file surfaces a `knus::Error` (a `miette::Diagnostic`),
//! which the binary renders with a source-span underline — the load-time diagnostic
//! that stands in for KDL's thin editor tooling.
//!
//! ## What the role holds
//!
//! One artifact configures every cascade rung (backlog/permissions/README.md):
//!
//! - [`Role::brief`] — the positive scope, **shared with the acting session** for
//!   self-alignment. It projects into the cached preamble ([`Role::preamble_section`]).
//! - [`Role::judge_rubric`] — the red-flag rubric, **judge-only**. Kept off the
//!   session's preamble so the session can't teach to the test. Consumed by the judge
//!   rung (not built yet).
//! - [`Role::allow`] — the prefix-allowlist the deterministic fast-path matches against
//!   (backlog/permissions/prefix-fastpath.md). Consumed by that rung (not built yet).
//! - [`Role::trust_threshold`] — where the watch-window/gate cutoffs sit. Consumed by
//!   the judge/watch-window rungs (not built yet).
//!
//! Per the convention in CLAUDE.md, the schema fields land *before* the rungs that
//! read them — "wire the seam ahead of the feature".
//!
//! ## Caching discipline (load-bearing)
//!
//! The brief projects into the system preamble, which sits in the cached prefix
//! (`tools → system → messages`). [`Role::preamble_section`] must therefore be
//! **byte-stable for the whole session** (see the invariant in
//! backlog/permissions/README.md, kin to `project(log)` in docs/substrate.md). It is —
//! the role is loaded once at startup and frozen, and the rendering is a pure function
//! of the brief text. Changing roles is a cold cache write, which is fine: roles do not
//! change mid-session.

use std::path::{Path, PathBuf};

/// A role: portable policy describing what a session is for and what it may do
/// (backlog/permissions/role-config.md). Decoded from a KDL document; every field is
/// optional so a minimal — or empty — role file is valid, and an absent file yields
/// [`Role::default`].
#[derive(knus::Decode, Debug, Default, Clone)]
pub struct Role {
    /// Positive scope, shared with the acting session (self-alignment). Projects into
    /// the cached preamble via [`Role::preamble_section`]. KDL: `brief "…"`.
    #[knus(child, unwrap(argument), default)]
    pub brief: Option<String>,

    /// The judge's red-flag rubric — judge-only, never shown to the acting session (no
    /// teaching-to-the-test). KDL: `judge-rubric "…"`. Consumed by the judge rung.
    #[knus(child, unwrap(argument), default)]
    pub judge_rubric: Option<String>,

    /// Trust posture → where the watch-window/gate cutoffs sit. `None` = unset (a real
    /// `0` would mean something specific, so absence is kept distinct). KDL:
    /// `trust-threshold 0.5`. Consumed by the judge/watch-window rungs.
    #[knus(child, unwrap(argument), default)]
    pub trust_threshold: Option<f64>,

    /// The prefix-allowlist: `(program, safe-arg-prefix)` entries the deterministic
    /// fast-path clears with zero model cost (backlog/permissions/prefix-fastpath.md).
    /// KDL: `allow "cargo"` (blanket) or `allow "git" "diff"` (sub-verb prefix).
    #[knus(children(name = "allow"))]
    pub allow: Vec<Allow>,

    /// Structured tools the fast-path clears by name. The path-bearing ones
    /// (`read`/`write`/`search`/edit) additionally require their target to fall under a
    /// [`Role::paths`] grant; the daemon-internal ones (`consolidate`, `kill_process`, …)
    /// clear on the name alone. KDL: `tool "read"`. Nothing is sanctioned in code — even
    /// `kill_process` is just a config entry, so it's removable by editing the file.
    #[knus(children(name = "tool"))]
    pub tools: Vec<ToolGrant>,

    /// Filesystem permission grants (backlog/permissions/role-config.md). Each grants
    /// read and/or write over a subtree; the fast-path clears a path-bearing tool only
    /// when its target resolves under a grant carrying the needed permission. cwd is not
    /// special in code — it is special only because the config says `path "." …`. KDL:
    /// `path "." read=true write=true`.
    #[knus(children(name = "path"))]
    pub paths: Vec<PathGrant>,

    /// Reserved ancestor pointers (tsconfig/eslint-style explicit inheritance). Parsed
    /// so a file can declare them, but the merge engine is **not built** — build it only
    /// when a second file needs it, and make it deterministic (the resolved config feeds
    /// the cached preamble). KDL: `extends "base.kdl"`.
    #[knus(children(name = "extends"))]
    pub extends: Vec<Extends>,
}

/// One prefix-allowlist entry: a `program` and an optional `safe-arg-prefix`
/// (backlog/permissions/prefix-fastpath.md). An empty prefix blanket-trusts the whole
/// arg-space of `program` (sound only for programs whose entire arg-space is
/// recoverable, e.g. `cargo`); a non-empty prefix trusts only invocations whose leading
/// argument tokens match (e.g. `git diff`, leaving `git push`/`reset` to fall through).
/// The match is exact token-prefix — safety is discontinuous in the command string, so
/// only exact prefixes are soundly memoizable.
#[derive(knus::Decode, Debug, Clone)]
pub struct Allow {
    #[knus(argument)]
    pub program: String,
    #[knus(arguments)]
    pub prefix: Vec<String>,
}

/// A structured tool the fast-path clears by name (backlog/permissions/prefix-fastpath.md).
/// Just a name; the read/write semantics live with [`Role::paths`] and the per-tool
/// classifier (the not-yet-built rung), not here.
#[derive(knus::Decode, Debug, Clone)]
pub struct ToolGrant {
    #[knus(argument)]
    pub name: String,
}

/// A filesystem permission grant over a subtree — the `path` and everything beneath it
/// (backlog/permissions/role-config.md). `write` implies `read`. Both permissions
/// default to `false` when the property is omitted, so a bare `path "x"` grants nothing
/// (honest: you must say what you're granting). KDL: `path "." read=true write=true`.
#[derive(knus::Decode, Debug, Clone)]
pub struct PathGrant {
    #[knus(argument)]
    pub path: String,
    #[knus(property, default)]
    pub read: bool,
    #[knus(property, default)]
    pub write: bool,
}

/// A reserved `extends` ancestor pointer. Carries the path only; resolution/merge is
/// future work (see [`Role::extends`]).
#[derive(knus::Decode, Debug, Clone)]
pub struct Extends {
    #[knus(argument)]
    pub path: String,
}

/// Where a [`Role`] was resolved from, reported on `GET /state` so "which role am I
/// running?" is never ambiguous (backlog/permissions/role-config.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RoleSource {
    /// No config file found — the built-in [`Role::default`] is in effect.
    #[default]
    Default,
    /// Resolved from this path (an explicit `--config`, or the default path).
    Path(PathBuf),
}

/// A resolved role plus where it came from. Stored on the daemon's shared state and
/// reported by `/state`.
#[derive(Debug, Clone, Default)]
pub struct LoadedRole {
    pub role: Role,
    pub source: RoleSource,
}

/// The default role-config path, relative to cwd. Visible and committable (not under
/// the gitignored session dir) — a role is portable, shareable config, not session
/// state. The config is *pointed-at*, not a magic scanned tree: only this one path is
/// probed when `--config` is absent.
pub const DEFAULT_PATH: &str = "efference.kdl";

/// Failure to load a role file. The `Parse` arm carries the `knus::Error`, a
/// `miette::Diagnostic` that embeds the source so the binary can render the span
/// underline (see [`crate::role`] module docs).
#[derive(Debug)]
pub enum LoadError {
    /// Reading the file failed (an explicit `--config` that doesn't exist, a permission
    /// error, …). A *missing default* path is not an error — it yields [`RoleSource::Default`].
    Io(PathBuf, std::io::Error),
    /// The file is not valid KDL / does not match the schema.
    Parse(knus::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(path, e) => write!(f, "reading role config {}: {e}", path.display()),
            // Display only the message; the binary renders the full miette report
            // (with the source-span underline) out of the embedded `knus::Error`.
            LoadError::Parse(e) => write!(f, "invalid role config: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl LoadError {
    /// Print this error to stderr in its richest form. A parse error renders through
    /// miette's graphical handler — the source-span underline that points at the
    /// offending node — by reference (the handler takes `&dyn Diagnostic`, so no clone
    /// or ownership of the embedded `knus::Error` is needed). An IO error has no source
    /// to point at, so it prints its message.
    pub fn eprint(&self) {
        match self {
            LoadError::Parse(e) => {
                let mut rendered = String::new();
                if miette::GraphicalReportHandler::new()
                    .render_report(&mut rendered, e)
                    .is_ok()
                {
                    eprintln!("{rendered}");
                } else {
                    eprintln!("{self}");
                }
            }
            LoadError::Io(..) => eprintln!("{self}"),
        }
    }
}

/// Resolve and load the role. With `explicit` (a `--config` path) the file *must*
/// exist — a missing one is an error. Without it, the [`DEFAULT_PATH`] is probed: if
/// present it loads, if absent the built-in [`Role::default`] is used ([`RoleSource::Default`]).
pub fn load(explicit: Option<&Path>) -> Result<LoadedRole, LoadError> {
    match explicit {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| LoadError::Io(path.to_path_buf(), e))?;
            Ok(LoadedRole {
                role: parse(path, &text)?,
                source: RoleSource::Path(path.to_path_buf()),
            })
        }
        None => {
            let path = Path::new(DEFAULT_PATH);
            match std::fs::read_to_string(path) {
                Ok(text) => Ok(LoadedRole {
                    role: parse(path, &text)?,
                    source: RoleSource::Path(path.to_path_buf()),
                }),
                // The default path is optional: absent → the built-in default role.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LoadedRole::default()),
                Err(e) => Err(LoadError::Io(path.to_path_buf(), e)),
            }
        }
    }
}

/// Parse KDL text into a [`Role`]. The `path` is used only as the diagnostic file name
/// (it labels spans in the `knus::Error` report).
fn parse(path: &Path, text: &str) -> Result<Role, LoadError> {
    knus::parse(&path.display().to_string(), text).map_err(LoadError::Parse)
}

impl Role {
    /// Render the brief into the byte-stable preamble section appended to the system
    /// prompt (backlog/permissions/role-config.md), or `None` if there is no brief. The
    /// header is fixed and the body is the brief verbatim (trimmed), so the output is a
    /// pure, deterministic function of the brief — safe for the cached prefix. The
    /// judge rubric is deliberately *not* projected here: it is judge-only.
    pub fn preamble_section(&self) -> Option<String> {
        let brief = self.brief.as_deref().map(str::trim).filter(|b| !b.is_empty())?;
        Some(format!("Your role for this session:\n\n{brief}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(text: &str) -> Role {
        super::parse(Path::new("test.kdl"), text).expect("valid role")
    }

    #[test]
    fn empty_document_is_a_valid_default_role() {
        let role = parse_str("");
        assert!(role.brief.is_none());
        assert!(role.judge_rubric.is_none());
        assert!(role.trust_threshold.is_none());
        assert!(role.allow.is_empty());
        assert!(role.tools.is_empty());
        assert!(role.paths.is_empty());
        assert!(role.extends.is_empty());
    }

    #[test]
    fn parses_brief_judge_rubric_and_trust_threshold() {
        let role = parse_str(
            r#"
            brief "Work only inside this repo."
            judge-rubric "Flag network egress to non-registry hosts."
            trust-threshold 0.5
            "#,
        );
        assert_eq!(role.brief.as_deref(), Some("Work only inside this repo."));
        assert_eq!(
            role.judge_rubric.as_deref(),
            Some("Flag network egress to non-registry hosts.")
        );
        assert_eq!(role.trust_threshold, Some(0.5));
    }

    #[test]
    fn parses_blanket_and_sub_verb_allow_entries() {
        let role = parse_str(
            r#"
            allow "cargo"
            allow "git" "diff"
            allow "git" "status"
            "#,
        );
        assert_eq!(role.allow.len(), 3);
        // Blanket: program with no prefix tokens.
        assert_eq!(role.allow[0].program, "cargo");
        assert!(role.allow[0].prefix.is_empty());
        // Sub-verb: program plus a leading-arg prefix.
        assert_eq!(role.allow[1].program, "git");
        assert_eq!(role.allow[1].prefix, vec!["diff"]);
        assert_eq!(role.allow[2].prefix, vec!["status"]);
    }

    #[test]
    fn parses_tool_grants() {
        let role = parse_str(
            r#"
            tool "read"
            tool "write"
            tool "kill_process"
            "#,
        );
        let names: Vec<&str> = role.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["read", "write", "kill_process"]);
    }

    #[test]
    fn parses_path_grants_with_perms() {
        let role = parse_str(
            r#"
            path "." read=true write=true
            path "/tmp" read=true
            path "vendor"
            "#,
        );
        assert_eq!(role.paths.len(), 3);
        // cwd: full access.
        assert_eq!(role.paths[0].path, ".");
        assert!(role.paths[0].read && role.paths[0].write);
        // /tmp: read-only (write defaults false).
        assert!(role.paths[1].read && !role.paths[1].write);
        // bare path grants nothing until you say what you're granting.
        assert!(!role.paths[2].read && !role.paths[2].write);
    }

    #[test]
    fn parses_reserved_extends_pointer() {
        let role = parse_str(r#"extends "base.kdl""#);
        assert_eq!(role.extends.len(), 1);
        assert_eq!(role.extends[0].path, "base.kdl");
    }

    #[test]
    fn malformed_kdl_is_a_parse_error_not_a_panic() {
        let err = super::parse(Path::new("bad.kdl"), "brief = oops").unwrap_err();
        assert!(matches!(err, LoadError::Parse(_)));
    }

    #[test]
    fn unknown_node_is_rejected() {
        // A typo'd node name should fail loudly rather than be silently ignored — the
        // typed schema is the contract.
        let err = super::parse(Path::new("bad.kdl"), r#"breif "typo""#).unwrap_err();
        assert!(matches!(err, LoadError::Parse(_)));
    }

    #[test]
    fn preamble_section_renders_the_brief_byte_for_byte() {
        let role = parse_str(r#"brief "Stay in the repo.""#);
        assert_eq!(
            role.preamble_section().as_deref(),
            Some("Your role for this session:\n\nStay in the repo.")
        );
    }

    #[test]
    fn preamble_section_is_none_without_a_brief() {
        assert!(Role::default().preamble_section().is_none());
        // A whitespace-only brief is treated as no brief (no empty section on the wire).
        let role = parse_str("brief \"   \"");
        assert!(role.preamble_section().is_none());
    }

    /// The rubric is judge-only — it must never leak into the session's preamble.
    #[test]
    fn preamble_section_excludes_the_judge_rubric() {
        let role = parse_str(
            r#"
            brief "Do the task."
            judge-rubric "SECRET RED FLAGS"
            "#,
        );
        let section = role.preamble_section().unwrap();
        assert!(section.contains("Do the task."));
        assert!(!section.contains("SECRET RED FLAGS"));
    }

    #[test]
    fn explicit_missing_config_is_an_io_error() {
        let missing = std::env::temp_dir().join(format!("efference-no-such-{}.kdl", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        let err = load(Some(&missing)).unwrap_err();
        assert!(matches!(err, LoadError::Io(_, _)), "explicit path must exist");
    }

    #[test]
    fn explicit_present_config_resolves_with_its_path() {
        let path = std::env::temp_dir().join(format!("efference-role-{}.kdl", std::process::id()));
        std::fs::write(&path, r#"brief "hi""#).unwrap();
        let loaded = load(Some(&path)).unwrap();
        assert_eq!(loaded.role.brief.as_deref(), Some("hi"));
        assert_eq!(loaded.source, RoleSource::Path(path.clone()));
        std::fs::remove_file(&path).unwrap();
    }
}
