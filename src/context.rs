//! `[context]` preamble builder — reading, frontmatter, and mode (docs/context.md).
//!
//! The [`crate::config`] schema materializes the operator's `[context]` entries (what files, what
//! grants, what order). This module turns a file's *content* into a preamble section: it splits an
//! optional leading frontmatter block off the body and decides the file's **mode** — **inlined**
//! (the whole body rides in the cached preamble, the CLAUDE.md role) or **listed** (only a one-line
//! stub rides, the body fetched on demand via the `read` tool). Mode is the *file's* call, carried
//! in its frontmatter; the config never decides it (docs/context.md).
//!
//! Frontmatter is recognized **only at the very start of the file** (first line `---`), so a `---`
//! horizontal rule mid-document is never mistaken for it. The parser is deliberately minimal —
//! `key: value` lines plus YAML block scalars (`>-`-style folded / `|`-style literal values, which
//! real Claude-Code skill files use for their `description`) — not a full YAML engine: nested
//! structures stay out of scope. Unknown keys are ignored (their block values consumed whole, so a
//! foreign multiline value can never be misread as keys) so the vocabulary can grow without a
//! parser change.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{ContextEntry, ContextSource};

/// A fully-resolved `[context]` ready to render into the preamble: sections in **document order**
/// (docs/context.md). Each [`Section`] is a bare entry (no heading) or a labeled group.
/// This is what [`crate::env`] builds from disk at boot and freezes into the cached prefix.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Resolved {
    pub sections: Vec<Section>,
}

/// One rendered section: a labeled group (`heading = Some`) or a bare entry (`heading = None`).
/// `description` is the operator's prose hint about how to use these files (e.g. how to read or
/// write a memory), rendered as a `<description>` child element inside the group wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub heading: Option<String>,
    pub items: Vec<Item>,
    pub description: Option<String>,
}

/// A single resolved context item — an inlined file (body in full) or a listed stub (read on demand).
/// Both render as a name tag `<name>…</name>` (docs/context.md); the name is the file's
/// frontmatter `name`, else its basename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Item {
    /// The full body rides in the preamble inside `<name>…</name>`. `path` is kept for provenance /
    /// the boot snapshot, not rendered (the tag name is the identity).
    Inline {
        name: String,
        path: String,
        body: String,
    },
    /// A one-line stub `<name>{description} Read {path} for the full file.</name>`; the body is read
    /// on demand via `path`.
    Listed {
        name: String,
        description: String,
        path: String,
    },
    /// A dynamic section (docs/context.md): `body` is the stdout of `command`, run
    /// **once at boot** with the session cwd — frozen and snapshotted exactly like a static file, so
    /// `Date = { exec = "date" }` honestly means "the date as of session boot". Renders identically
    /// to [`Item::Inline`]; `command` is kept for provenance, not rendered.
    Exec {
        name: String,
        command: String,
        body: String,
    },
}

/// The instructional lead-in before the tagged block — teaches the model that a tag holding a file's
/// full text is present as-is, while a tag holding a short summary ending in "Read PATH" is a pointer
/// to fetch on demand. Fixed text, so it stays byte-stable in the cached prefix.
const LEAD_IN: &str = "The following context is available for this session, wrapped in name tags. \
A tag holding a file's full text is present as-is; a tag with a path= attribute holds a brief summary — \
read the file at that path to load the full content.";

impl Resolved {
    /// Render the whole `[context]` block appended to the system preamble, or `None` when nothing is
    /// configured (so the caller appends no empty block). **Byte-deterministic** — a pure function of
    /// the resolved sections, safe for the cached prefix (docs/log-and-projection.md). Every file renders as a
    /// `<name>…</name>` tag (name-as-tag pseudo-XML — the file's name, or the operator's group
    /// heading, used **verbatim**: spaces and leading digits and all, since nothing parses it); a
    /// group wraps its members in `<Heading>…</Heading>`. Inlined bodies ride verbatim inside their
    /// tag, so a body's own markdown can't blur the boundary. Sections join with a blank line, in
    /// document order. The format is provisional (docs/context.md).
    pub fn render(&self) -> Option<String> {
        if self.sections.is_empty() {
            return None;
        }
        let mut blocks = vec![LEAD_IN.to_string()];
        for section in &self.sections {
            let mut inner = String::new();
            if let Some(desc) = &section.description {
                inner.push_str(&format!("<description>{desc}</description>\n"));
            }
            inner.push_str(
                &section
                    .items
                    .iter()
                    .map(render_item)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            blocks.push(match &section.heading {
                Some(heading) => format!("<{heading}>\n{inner}\n</{heading}>"),
                None => inner,
            });
        }
        Some(blocks.join("\n\n"))
    }
}

/// Render one item as its `<name>…</name>` tag: an inlined file holds its verbatim body (trimmed of
/// surrounding blank space); a listed file holds its summary and a read pointer.
///
/// The fork peek's surfacing roster (docs/redaction-fork.md) renders through here too, so
/// an item the model meets mid-session is spelled exactly as it was in the boot preamble.
/// A bare name reads as a *capability* rather than a file to open, and gets built into a
/// tool call (journal/2026-08-07-session-key-review.md).
pub(crate) fn render_item(item: &Item) -> String {
    match item {
        Item::Inline { name, body, .. } | Item::Exec { name, body, .. } => {
            format!("<{name}>\n{}\n</{name}>", body.trim())
        }
        Item::Listed {
            name,
            description,
            path,
        } => format!("<{name} path=\"{path}\">{description}</{name}>"),
    }
}

/// A failure resolving the `[context]` entries at boot — the preamble builder fails **closed** on
/// these (docs/context.md): configured context that can't be materialized is a config
/// error, never a silently-dropped section.
#[derive(Debug)]
pub enum ResolveError {
    /// A listed path (or a directory's child) could not be statted or read.
    Io(PathBuf, std::io::Error),
    /// A file's frontmatter opened (`---`) but never closed.
    Frontmatter(PathBuf),
    /// An `exec` section's command failed — couldn't spawn, exited non-zero, timed out, or
    /// produced non-UTF-8 stdout. The detail says which.
    Exec { command: String, detail: String },
    /// A group's `sort` override isn't `<field> [asc|desc]`, comma-separated. Fails closed rather
    /// than falling back to the default order (docs/context.md).
    Sort(SortSpecError),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Io(p, e) => write!(f, "reading context path {}: {e}", p.display()),
            ResolveError::Frontmatter(p) => write!(
                f,
                "context file {} has an unterminated `---` frontmatter block",
                p.display()
            ),
            ResolveError::Exec { command, detail } => {
                write!(f, "context exec `{command}`: {detail}")
            }
            ResolveError::Sort(e) => write!(f, "context sort override: {e}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve the config's `[context]` entries against `base` (the session cwd) into a renderable
/// [`Resolved`], reading each file and deciding its mode from frontmatter. **Fails closed**: a listed
/// path that can't be read/statted, or a file whose frontmatter opens but never closes, fails the
/// whole load — the preamble never silently drops configured context.
///
/// Tag-name precedence: the operator's label (when a labeled entry resolves to a *single* file) →
/// the file's frontmatter `name` (skills-compat) → the basename. A labeled entry with *many* files
/// becomes a `<Heading>` group whose members are named individually; a bare entry never gets a
/// wrapper. Directory paths expand to their `.md` children, sorted name-ascending; a group's
/// SQL-ish `sort` override then reorders the whole entry ([`sort`]). (Globs are a later increment;
/// a `*` path just won't stat and fails closed.)
///
/// An `exec` entry runs its command **here, once at boot** ([`run_exec`]) — its stdout is the
/// section body, frozen and snapshotted like everything else (docs/context.md). It
/// always resolves to a single section that collapses like a labeled single file (the label is the
/// tag, no wrapper), and it fails closed like a file would: a command that can't run is a config
/// error, never a silently dropped section.
pub fn resolve(entries: &[ContextEntry], base: &Path) -> Result<Resolved, ResolveError> {
    resolve_with_child_env(entries, base, &crate::env_table::ChildEnv::default())
}

/// [`resolve`] with the `[env]` table's spawn policy applied to every `exec`
/// entry's child (docs/env.md). A `[context]` exec is a subprocess like any
/// other, and its stdout lands in the cached preamble, so it answers the same
/// spawn-time question the `bash` tool does. `resolve` itself keeps the
/// two-argument shape the dev views and tests use.
pub fn resolve_with_child_env(
    entries: &[ContextEntry],
    base: &Path,
    child_env: &crate::env_table::ChildEnv,
) -> Result<Resolved, ResolveError> {
    let mut sections = Vec::new();
    for entry in entries {
        let paths = match &entry.source {
            ContextSource::Files(paths) => paths,
            // The guide overlay (docs/context.md, [`crate::guide`]): born resolved. The
            // items are compiled-in `(name, description, body)` triples, so this reuses the
            // *render* half of the pipeline and never touches the disk or the frontmatter
            // parser. The operator's label is the heading; their `description`, if they
            // wrote one, replaces the mount's own.
            ContextSource::Builtin(_) => {
                let heading = entry
                    .heading
                    .clone()
                    .expect("validate gives every builtin entry a heading");
                let mut section = crate::guide::section(&heading);
                if entry.description.is_some() {
                    section.description = entry.description.clone();
                }
                sections.push(section);
                continue;
            }
            ContextSource::Exec(command) => {
                let name = entry
                    .heading
                    .clone()
                    .expect("validate gives every exec entry a heading");
                let body = run_exec(command, base, EXEC_TIMEOUT, child_env)?;
                sections.push(Section {
                    heading: None,
                    description: entry.description.clone(),
                    items: vec![Item::Exec {
                        name,
                        command: command.clone(),
                        body,
                    }],
                });
                continue;
            }
        };
        let files = resolve_files(paths, base, &entry.exclude)?;
        // The operator's label is a file's own tag only when the entry resolves to exactly one file;
        // otherwise the label is a group wrapper and members are named individually.
        let labeled_single = entry.heading.is_some() && files.len() == 1;
        let mut items = Vec::new();
        // Rows parallel to `items`, carrying the columns a `sort` override addresses. Built
        // unconditionally (they cost one `metadata` call each) so the sort branch below is a pure
        // reordering of what was already read.
        let mut rows: Vec<sort::Row> = Vec::new();
        for (display, full) in &files {
            let text =
                std::fs::read_to_string(full).map_err(|e| ResolveError::Io(full.clone(), e))?;
            let split =
                split_frontmatter(&text).map_err(|_| ResolveError::Frontmatter(full.clone()))?;
            let basename = fallback_name(Path::new(display));
            let name = if labeled_single {
                entry
                    .heading
                    .clone()
                    .expect("labeled_single implies a heading")
            } else {
                split
                    .frontmatter
                    .as_ref()
                    .and_then(|fm| fm.name.clone())
                    .unwrap_or_else(|| basename.clone())
            };
            rows.push(sort::Row {
                // The sort's `name` column is the file's own name — its frontmatter `name`, else
                // its basename — never the operator's collapsed label, which is a property of the
                // entry rather than of the row.
                name: split
                    .frontmatter
                    .as_ref()
                    .and_then(|fm| fm.name.clone())
                    .unwrap_or_else(|| basename.clone()),
                display: display.clone(),
                modified: std::fs::metadata(full)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64()),
                fields: split
                    .frontmatter
                    .as_ref()
                    .map(|fm| fm.fields.clone())
                    .unwrap_or_default(),
            });
            items.push(match split.mode(&basename) {
                Mode::Inline => Item::Inline {
                    name,
                    path: display.clone(),
                    body: split.body.to_string(),
                },
                Mode::Listed { description, .. } => Item::Listed {
                    name,
                    description,
                    path: display.clone(),
                },
            });
        }
        if items.is_empty() {
            continue;
        }
        // The override replaces the default order (`name asc` by display path for a directory
        // expansion, written order for an explicit list) for this entry only.
        if let Some(spec) = &entry.sort {
            let keys = sort::parse(spec).map_err(ResolveError::Sort)?;
            sort::apply(&mut items, &mut rows, &keys);
        }
        let heading =
            (entry.heading.is_some() && !labeled_single).then(|| entry.heading.clone().unwrap());
        sections.push(Section {
            heading,
            description: entry.description.clone(),
            items,
        });
    }
    Ok(Resolved { sections })
}

/// Resolve one entry's path list into `(display, full)` pairs: `display` is the model-facing path
/// (relative, as the operator wrote it) used in a stub's `Read {path}`; `full` is `base`-joined for
/// reading. A directory expands to every `.md` file **recursively** beneath it ([`walk_md`]) —
/// which is what makes a Claude-Code-style skills tree (`skills/<name>/SKILL.md`, one dir per
/// skill) land as one listing per skill — sorted name-ascending by display path; an explicit list
/// keeps its order. (Globs are a later increment; a `*` path just won't stat and fails closed.)
///
/// `exclude` prunes the expansion (docs/context.md): each entry names a path relative
/// to the configured directory, matched component-wise, and a matched file or subtree is skipped
/// **during the walk** — never read, so an unreadable file inside an excluded subtree cannot fail
/// boot. Explicit paths are never filtered: an operator who lists a file and excludes it wrote a
/// contradiction the explicit mention wins.
fn resolve_files(
    paths: &[String],
    base: &Path,
    exclude: &[String],
) -> Result<Vec<(String, PathBuf)>, ResolveError> {
    let mut out = Vec::new();
    for p in paths {
        let full = base.join(p);
        let meta = std::fs::metadata(&full).map_err(|e| ResolveError::Io(full.clone(), e))?;
        if meta.is_dir() {
            let mut found = Vec::new();
            let mut seen = std::collections::HashSet::new();
            if let Ok(canon) = full.canonicalize() {
                seen.insert(canon);
            }
            let root = Path::new(p);
            walk_md(&full, root, root, exclude, &mut found, &mut seen)?;
            found.sort_by(|a, b| a.0.cmp(&b.0));
            out.extend(found);
        } else {
            out.push((p.clone(), full));
        }
    }
    Ok(out)
}

/// Collect every `.md` file under `dir` (recursively) into `(display, full)` pairs, `display`
/// rooted at `display_dir` (the path as the operator wrote it). **Hidden children** (dot-prefixed)
/// are skipped — a `.git` or cache dir inside a listed tree is never content, though the named
/// root itself may be hidden (`~/.claude/skills`). **Symlinks are followed** — a symlinked skill
/// collection is a real layout — with a cycle guard: `seen` holds every canonical directory
/// already entered, so a link cycle (or a diamond) walks each real directory once. Children are
/// visited in name order, so which path a diamond's files get is deterministic. A child the walk
/// can't stat/read fails closed like any other unreadable listed path.
fn walk_md(
    dir: &Path,
    display_dir: &Path,
    root: &Path,
    exclude: &[String],
    out: &mut Vec<(String, PathBuf)>,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), ResolveError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| ResolveError::Io(dir.to_path_buf(), e))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let child = entry.path();
        let child_display = display_dir.join(&name);
        // Pruned before the stat: an excluded child (file or subtree) is skipped unread,
        // so nothing inside it can fail the load. `root` is the configured directory the
        // exclude entries are relative to; `starts_with` matches whole components.
        if let Ok(rel) = child_display.strip_prefix(root)
            && exclude.iter().any(|e| rel.starts_with(e))
        {
            continue;
        }
        // `metadata` follows symlinks, so a link to a dir/file behaves as what it points at.
        let meta = std::fs::metadata(&child).map_err(|e| ResolveError::Io(child.clone(), e))?;
        if meta.is_dir() {
            let canon = child
                .canonicalize()
                .map_err(|e| ResolveError::Io(child.clone(), e))?;
            if seen.insert(canon) {
                walk_md(&child, &child_display, root, exclude, out, seen)?;
            }
        } else if meta.is_file() && child.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push((child_display.to_string_lossy().into_owned(), child));
        }
    }
    Ok(())
}

/// How long an `exec` section's command may run before boot fails closed
/// (docs/context.md): long enough for a real describe-the-world script, short
/// enough that a hung command can't hang boot indefinitely.
const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Run one `exec` section's command and return its stdout — `bash -c`, cwd = `base` (the same
/// entrypoint the bash tool uses, so an operator's command means the same thing in both places).
/// Fails closed on a spawn failure, non-zero exit (stderr quoted in the error), timeout, or
/// non-UTF-8 stdout — binary bytes in a text preamble are a config mistake, not content.
///
/// The child leads its own process group and is reaped on a thread (`wait_with_output` drains the
/// pipes, so a chatty command can't deadlock on a full pipe buffer); on timeout the whole group is
/// killed ([`crate::bash::kill_group`]), so a hung command's children die with it.
fn run_exec(
    command: &str,
    base: &Path,
    timeout: std::time::Duration,
    child_env: &crate::env_table::ChildEnv,
) -> Result<String, ResolveError> {
    use std::os::unix::process::CommandExt;
    let err = |detail: String| ResolveError::Exec {
        command: command.to_string(),
        detail,
    };
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-c")
        .arg(command)
        .current_dir(base)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    // The machine root never reaches a spawned child (docs/web-bind.md), and the
    // `[env]` table's spawn policy applies here as at every other spawn
    // (docs/env.md).
    child_env.apply(&mut cmd);
    cmd.env_remove(crate::auth::ROOT_ENV);
    let child =
        crate::spawn::std(&mut cmd).map_err(|e| err(format!("failed to spawn bash: {e}")))?;
    let pgid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let output = match rx.recv_timeout(timeout) {
        Ok(result) => result.map_err(|e| err(format!("waiting on the command: {e}")))?,
        Err(_) => {
            crate::bash::kill_group(pgid);
            return Err(err(format!("timed out after {}s", timeout.as_secs_f32())));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(err(format!(
            "exited with {}; stderr: {}",
            output.status,
            stderr.trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|_| err("stdout was not valid UTF-8".into()))
}

#[cfg(test)]
mod test;

mod frontmatter;
pub mod sort;

pub use frontmatter::{
    Frontmatter, Mode, Split, UnterminatedFrontmatter, fallback_name, split_frontmatter,
};
pub use sort::SortSpecError;
