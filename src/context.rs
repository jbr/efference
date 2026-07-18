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

use crate::config::{ContextEntry, ContextSource};

/// The frontmatter keys we read. Everything else in the block is ignored (forward-compatible).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    /// Display name for a listed stub; falls back to the file's basename when absent.
    pub name: Option<String>,
    /// The one-line catalog description. Its **presence** is what makes a file *listed* rather than
    /// inlined (there is a stub line to show); its absence leaves the file inlined.
    pub description: Option<String>,
    /// Explicit mode override: `inline: true` forces a described file to inline anyway.
    pub inline: Option<bool>,
}

/// A file split into its optional frontmatter and its body (everything after the frontmatter block,
/// or the whole file when there is none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split<'a> {
    pub frontmatter: Option<Frontmatter>,
    pub body: &'a str,
}

/// How a context file renders into the preamble (docs/context.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// The whole body rides in the cached preamble (always-on knowledge).
    Inline,
    /// Only a stub rides in the preamble; the body is read on demand. `name`/`description` are the
    /// catalog line.
    Listed { name: String, description: String },
}

/// A frontmatter block that opened (`---` as the first line) but never closed. Malformed — the
/// caller fails the config load closed rather than guess where the body begins.
#[derive(Debug, PartialEq, Eq)]
pub struct UnterminatedFrontmatter;

/// An in-progress YAML block scalar while scanning frontmatter: which key it belongs to, whether
/// it folds (`>`, lines joined with spaces) or is literal (`|`, newlines kept), and the trimmed
/// continuation lines collected so far.
struct BlockScalar {
    key: String,
    folded: bool,
    lines: Vec<String>,
}

/// Split an optional leading frontmatter block off `text`. Frontmatter exists **only** when the very
/// first line is the `---` delimiter; the block runs to the next `---` line, and the body is
/// everything after it. A `---` anywhere but the first line is ordinary markdown (a horizontal
/// rule), never frontmatter. An opened-but-unclosed block is [`UnterminatedFrontmatter`].
///
/// A value of `>`/`>-`/`>+` or `|`/`|-`/`|+` opens a YAML block scalar: the indented (or blank)
/// lines that follow are its content, closed by the first flush-left line or the `---` terminator.
/// Block content is consumed for **every** key, known or not — an unknown key's multiline value
/// must never be misread as `key: value` lines of its own.
pub fn split_frontmatter(text: &str) -> Result<Split<'_>, UnterminatedFrontmatter> {
    let first_line_end = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
    if text[..first_line_end].trim() != "---" {
        return Ok(Split {
            frontmatter: None,
            body: text,
        });
    }
    let mut fm = Frontmatter::default();
    let mut block: Option<BlockScalar> = None;
    let mut offset = first_line_end;
    for line in text[first_line_end..].split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed == "---" {
            // Closing delimiter: the body is everything after this line.
            if let Some(b) = block.take() {
                close_block_scalar(&mut fm, b);
            }
            return Ok(Split {
                frontmatter: Some(fm),
                body: &text[offset + line.len()..],
            });
        }
        if block.is_some() {
            if trimmed.is_empty() || line.starts_with([' ', '\t']) {
                block.as_mut().unwrap().lines.push(trimmed.to_string());
                offset += line.len();
                continue;
            }
            // First flush-left line ends the block; fall through to parse this line normally.
            close_block_scalar(&mut fm, block.take().unwrap());
        }
        block = parse_frontmatter_line(line, &mut fm);
        offset += line.len();
    }
    Err(UnterminatedFrontmatter)
}

/// Parse one `key: value` frontmatter line. Splits on the *first* colon (so a value may itself
/// contain colons) and trims. A block-scalar indicator value (`>`/`|` with an optional `-`/`+`
/// chomp) returns the opened [`BlockScalar`] for the caller to fill; otherwise the value has one
/// layer of matching surrounding quotes stripped and is assigned via [`set_frontmatter_key`].
/// Lines without a colon are ignored, as are **indented** lines: a top-level frontmatter key is
/// flush-left, so an indented `key: value` is the interior of some nested YAML structure (e.g. a
/// `metadata:` map) — skipping it keeps a nested `name:`/`description:` from polluting ours.
fn parse_frontmatter_line(line: &str, fm: &mut Frontmatter) -> Option<BlockScalar> {
    if line.starts_with([' ', '\t']) {
        return None;
    }
    let (key, value) = line.split_once(':')?;
    let value = value.trim();
    if let Some(folded) = block_scalar_style(value) {
        return Some(BlockScalar {
            key: key.trim().to_string(),
            folded,
            lines: Vec::new(),
        });
    }
    set_frontmatter_key(fm, key.trim(), unquote(value));
    None
}

/// Whether `value` is a YAML block-scalar indicator, and if so which style: `Some(true)` for
/// folded (`>`, `>-`, `>+`), `Some(false)` for literal (`|`, `|-`, `|+`), `None` for a plain value.
fn block_scalar_style(value: &str) -> Option<bool> {
    match value {
        ">" | ">-" | ">+" => Some(true),
        "|" | "|-" | "|+" => Some(false),
        _ => None,
    }
}

/// Assemble a finished block scalar and assign it: folded joins the non-blank lines with spaces
/// (the single-paragraph `description: >-` case real skill files use); literal keeps line breaks.
/// Chomping indicators are ignored — trailing whitespace never matters to these keys.
fn close_block_scalar(fm: &mut Frontmatter, block: BlockScalar) {
    let value = if block.folded {
        block
            .lines
            .iter()
            .filter(|l| !l.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        block.lines.join("\n").trim_end().to_string()
    };
    set_frontmatter_key(fm, &block.key, &value);
}

/// Assign one frontmatter value to the key it names. Unknown keys are ignored; an unrecognized
/// `inline` value leaves the flag unset.
fn set_frontmatter_key(fm: &mut Frontmatter, key: &str, value: &str) {
    match key {
        "name" => fm.name = Some(value.to_string()),
        "description" => fm.description = Some(value.to_string()),
        "inline" => {
            fm.inline = match value.to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => fm.inline,
            }
        }
        _ => {}
    }
}

/// Strip one layer of matching `"` or `'` surrounding quotes, if present.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

impl Split<'_> {
    /// Decide the file's [`Mode`]. A file **lists** iff its frontmatter carries a `description` and
    /// does not explicitly opt back in with `inline: true`; otherwise it **inlines** (no
    /// frontmatter, no description, or `inline: true`). `fallback_name` (the file's basename) names
    /// a listed stub when the frontmatter omits `name`.
    pub fn mode(&self, fallback_name: &str) -> Mode {
        match &self.frontmatter {
            Some(fm) if fm.description.is_some() && fm.inline != Some(true) => Mode::Listed {
                name: fm
                    .name
                    .clone()
                    .unwrap_or_else(|| fallback_name.to_string()),
                description: fm.description.clone().unwrap_or_default(),
            },
            _ => Mode::Inline,
        }
    }
}

/// The display name a listed file falls back to when its frontmatter omits `name`: the file's
/// basename without extension (`docs/verify.md` → `verify`), or the whole basename when it has no
/// stem, or `"context"` for a pathological path.
pub fn fallback_name(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "context".to_string())
}

/// A fully-resolved `[context]` ready to render into the preamble: sections in **document order**
/// (docs/context.md). Each [`Section`] is a bare entry (no heading) or a labeled group.
/// This is what [`crate::env`] builds from disk at boot and freezes into the cached prefix.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolved {
    pub sections: Vec<Section>,
}

/// One rendered section: a labeled group (`heading = Some`) or a bare entry (`heading = None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: Option<String>,
    pub items: Vec<Item>,
}

/// A single resolved context item — an inlined file (body in full) or a listed stub (read on demand).
/// Both render as a name tag `<name>…</name>` (docs/context.md); the name is the file's
/// frontmatter `name`, else its basename.
#[derive(Debug, Clone, PartialEq, Eq)]
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
A tag holding a file's full text is present as-is; a tag holding a one-line summary that ends in \
\"Read PATH …\" is a pointer — read that path to load the full file when relevant.";

impl Resolved {
    /// Render the whole `[context]` block appended to the system preamble, or `None` when nothing is
    /// configured (so the caller appends no empty block). **Byte-deterministic** — a pure function of
    /// the resolved sections, safe for the cached prefix (docs/substrate.md). Every file renders as a
    /// `<name>…</name>` tag (name-as-tag pseudo-XML — the file's name, or the operator's group
    /// heading, used **verbatim**: spaces and leading digits and all, since nothing parses it); a
    /// group wraps its members in `<Heading>…</Heading>`. Inlined bodies ride verbatim inside their
    /// tag, so a body's own markdown can't blur the boundary. Sections join with a blank line, in
    /// document order (backlog/preamble-format-experiment.md records why this format is provisional).
    pub fn render(&self) -> Option<String> {
        if self.sections.is_empty() {
            return None;
        }
        let mut blocks = vec![LEAD_IN.to_string()];
        for section in &self.sections {
            let inner = section
                .items
                .iter()
                .map(render_item)
                .collect::<Vec<_>>()
                .join("\n");
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
fn render_item(item: &Item) -> String {
    match item {
        Item::Inline { name, body, .. } | Item::Exec { name, body, .. } => {
            format!("<{name}>\n{}\n</{name}>", body.trim())
        }
        Item::Listed {
            name,
            description,
            path,
        } => format!("<{name}>{description} Read {path} for the full file.</{name}>"),
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
/// wrapper. Directory paths expand to their `.md` children, sorted name-ascending (the default; the
/// SQL-ish `sort` override and globs are later increments).
///
/// An `exec` entry runs its command **here, once at boot** ([`run_exec`]) — its stdout is the
/// section body, frozen and snapshotted like everything else (docs/context.md). It
/// always resolves to a single section that collapses like a labeled single file (the label is the
/// tag, no wrapper), and it fails closed like a file would: a command that can't run is a config
/// error, never a silently dropped section.
pub fn resolve(entries: &[ContextEntry], base: &Path) -> Result<Resolved, ResolveError> {
    let mut sections = Vec::new();
    for entry in entries {
        let paths = match &entry.source {
            ContextSource::Files(paths) => paths,
            ContextSource::Exec(command) => {
                let name = entry
                    .heading
                    .clone()
                    .expect("validate gives every exec entry a heading");
                let body = run_exec(command, base, EXEC_TIMEOUT)?;
                sections.push(Section {
                    heading: None,
                    items: vec![Item::Exec {
                        name,
                        command: command.clone(),
                        body,
                    }],
                });
                continue;
            }
        };
        let files = resolve_files(paths, base)?;
        // The operator's label is a file's own tag only when the entry resolves to exactly one file;
        // otherwise the label is a group wrapper and members are named individually.
        let labeled_single = entry.heading.is_some() && files.len() == 1;
        let mut items = Vec::new();
        for (display, full) in &files {
            let text =
                std::fs::read_to_string(full).map_err(|e| ResolveError::Io(full.clone(), e))?;
            let split =
                split_frontmatter(&text).map_err(|_| ResolveError::Frontmatter(full.clone()))?;
            let basename = fallback_name(Path::new(display));
            let name = if labeled_single {
                entry.heading.clone().expect("labeled_single implies a heading")
            } else {
                split
                    .frontmatter
                    .as_ref()
                    .and_then(|fm| fm.name.clone())
                    .unwrap_or_else(|| basename.clone())
            };
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
        let heading = (entry.heading.is_some() && !labeled_single)
            .then(|| entry.heading.clone().unwrap());
        sections.push(Section { heading, items });
    }
    Ok(Resolved { sections })
}

/// Resolve one entry's path list into `(display, full)` pairs: `display` is the model-facing path
/// (relative, as the operator wrote it) used in a stub's `Read {path}`; `full` is `base`-joined for
/// reading. A directory expands to every `.md` file **recursively** beneath it ([`walk_md`]) —
/// which is what makes a Claude-Code-style skills tree (`skills/<name>/SKILL.md`, one dir per
/// skill) land as one listing per skill — sorted name-ascending by display path; an explicit list
/// keeps its order. (Globs are a later increment; a `*` path just won't stat and fails closed.)
fn resolve_files(paths: &[String], base: &Path) -> Result<Vec<(String, PathBuf)>, ResolveError> {
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
            walk_md(&full, Path::new(p), &mut found, &mut seen)?;
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
        // `metadata` follows symlinks, so a link to a dir/file behaves as what it points at.
        let meta = std::fs::metadata(&child).map_err(|e| ResolveError::Io(child.clone(), e))?;
        let child_display = display_dir.join(&name);
        if meta.is_dir() {
            let canon = child
                .canonicalize()
                .map_err(|e| ResolveError::Io(child.clone(), e))?;
            if seen.insert(canon) {
                walk_md(&child, &child_display, out, seen)?;
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
) -> Result<String, ResolveError> {
    use std::os::unix::process::CommandExt;
    let err = |detail: String| ResolveError::Exec {
        command: command.to_string(),
        detail,
    };
    let child = std::process::Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(base)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| err(format!("failed to spawn bash: {e}")))?;
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
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_is_the_whole_body_and_inlines() {
        let text = "# Title\n\nBody paragraph.\n";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(split.frontmatter, None);
        assert_eq!(split.body, text);
        assert_eq!(split.mode("file"), Mode::Inline);
    }

    #[test]
    fn frontmatter_with_description_lists_and_splits_the_body() {
        let text = "---\nname: verify\ndescription: Drive the flow end to end.\n---\nThe body.\n";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(
            split.frontmatter,
            Some(Frontmatter {
                name: Some("verify".into()),
                description: Some("Drive the flow end to end.".into()),
                inline: None,
            })
        );
        assert_eq!(split.body, "The body.\n");
        assert_eq!(
            split.mode("fallback"),
            Mode::Listed {
                name: "verify".into(),
                description: "Drive the flow end to end.".into(),
            }
        );
    }

    #[test]
    fn description_without_name_uses_the_fallback() {
        let text = "---\ndescription: A thing.\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(
            split.mode("my-doc"),
            Mode::Listed {
                name: "my-doc".into(),
                description: "A thing.".into(),
            }
        );
    }

    #[test]
    fn inline_true_overrides_a_description_back_to_inline() {
        let text = "---\ndescription: A thing.\ninline: true\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(split.frontmatter.as_ref().unwrap().inline, Some(true));
        assert_eq!(split.mode("d"), Mode::Inline);
    }

    #[test]
    fn frontmatter_without_a_description_inlines() {
        // A name but no description: nothing to catalog, so it inlines.
        let text = "---\nname: just-a-name\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(split.mode("d"), Mode::Inline);
    }

    #[test]
    fn a_dash_rule_mid_document_is_not_frontmatter() {
        // The file does not *start* with the delimiter, so the `---` below is a horizontal rule.
        let text = "# Title\n\n---\n\nMore text.\n";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(split.frontmatter, None);
        assert_eq!(split.body, text);
    }

    #[test]
    fn unterminated_frontmatter_is_an_error() {
        let text = "---\nname: oops\nno closing delimiter\n";
        assert_eq!(split_frontmatter(text), Err(UnterminatedFrontmatter));
    }

    #[test]
    fn a_value_may_contain_colons_and_be_quoted() {
        let text = "---\ndescription: \"Use this: for X, Y, and Z\"\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(
            split.frontmatter.unwrap().description.as_deref(),
            Some("Use this: for X, Y, and Z"),
        );
    }

    /// The shape real Claude-Code skill files use: `description: >-` folded over several
    /// indented lines. The folded value joins with spaces and still drives listed mode.
    #[test]
    fn folded_block_scalar_description_lists() {
        let text = "---\nname: ferritin\ndescription: >-\n  Look up real Rust documentation\n  \
                    instead of guessing APIs.\n---\nbody\n";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(
            split.mode("fallback"),
            Mode::Listed {
                name: "ferritin".into(),
                description: "Look up real Rust documentation instead of guessing APIs.".into(),
            }
        );
        assert_eq!(split.body, "body\n");
    }

    /// A literal (`|`) block scalar keeps its line breaks; the chomp indicator is ignored.
    #[test]
    fn literal_block_scalar_keeps_newlines() {
        let text = "---\ndescription: |-\n  line one\n  line two\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(
            split.frontmatter.unwrap().description.as_deref(),
            Some("line one\nline two")
        );
    }

    /// An unknown key's block value is consumed whole — an indented `description:` line inside
    /// it is that block's content, never a frontmatter key of its own.
    #[test]
    fn unknown_key_block_scalar_cannot_pollute_known_keys() {
        let text = "---\nfuture_key: >-\n  description: sneaky\n  more text\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(split.mode("d"), Mode::Inline);
        assert_eq!(split.frontmatter.unwrap().description, None);
    }

    /// A block scalar closed by the frontmatter terminator itself (no flush-left line after it)
    /// still lands.
    #[test]
    fn block_scalar_closed_by_the_terminator() {
        let text = "---\ndescription: >\n  right at the end\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(
            split.frontmatter.unwrap().description.as_deref(),
            Some("right at the end")
        );
    }

    /// A nested map's interior lines are indented, so its `name:`/`description:` never pollute the
    /// top-level keys — the shape of a memory file's `metadata:` map.
    #[test]
    fn nested_map_keys_cannot_pollute_top_level_keys() {
        let text = "---\nmetadata:\n  name: sneaky\n  type: user\ndescription: Real one.\n---\nb";
        let split = split_frontmatter(text).unwrap();
        let fm = split.frontmatter.unwrap();
        assert_eq!(fm.name, None);
        assert_eq!(fm.description.as_deref(), Some("Real one."));
    }

    #[test]
    fn unknown_frontmatter_keys_are_ignored() {
        let text = "---\ndescription: A.\nfuture_key: whatever\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        let fm = split.frontmatter.unwrap();
        assert_eq!(fm.description.as_deref(), Some("A."));
        assert_eq!(fm.name, None);
        assert_eq!(fm.inline, None);
    }

    #[test]
    fn empty_frontmatter_block_inlines() {
        let text = "---\n---\nbody";
        let split = split_frontmatter(text).unwrap();
        assert_eq!(split.frontmatter, Some(Frontmatter::default()));
        assert_eq!(split.body, "body");
        assert_eq!(split.mode("d"), Mode::Inline);
    }

    #[test]
    fn fallback_name_is_the_stem() {
        assert_eq!(fallback_name(Path::new("docs/verify.md")), "verify");
        assert_eq!(fallback_name(Path::new("CLAUDE.md")), "CLAUDE");
    }

    // --- render: the name-as-tag pseudo-XML format (byte-pinned) ---

    #[test]
    fn empty_resolved_renders_none() {
        assert_eq!(Resolved::default().render(), None);
    }

    /// The full render pins the tagged bytes: a bare inlined file is its own `<name>` tag; a labeled
    /// group wraps its listed member(s); sections join with a blank line, in document order. Pins the
    /// structure by referencing `LEAD_IN`, so the lead-in wording can change without breaking it.
    #[test]
    fn render_pins_the_tagged_bytes() {
        let resolved = Resolved {
            sections: vec![
                Section {
                    heading: None,
                    items: vec![Item::Inline {
                        name: "CLAUDE.md".into(),
                        path: "./CLAUDE.md".into(),
                        body: "# efference\n\nThe harness.\n".into(),
                    }],
                },
                Section {
                    heading: Some("Skills".into()),
                    items: vec![Item::Listed {
                        name: "verify".into(),
                        description: "Drive the flow end to end.".into(),
                        path: "skills/verify.md".into(),
                    }],
                },
            ],
        };
        let expected = format!(
            "{LEAD_IN}\n\n\
             <CLAUDE.md>\n# efference\n\nThe harness.\n</CLAUDE.md>\n\n\
             <Skills>\n<verify>Drive the flow end to end. Read skills/verify.md for the full \
             file.</verify>\n</Skills>"
        );
        assert_eq!(resolved.render().as_deref(), Some(expected.as_str()));
    }

    /// The operator's heading is the tag verbatim — spaces and all, since it's pseudo-XML nothing
    /// parses.
    #[test]
    fn render_uses_the_heading_verbatim_as_a_tag() {
        let resolved = Resolved {
            sections: vec![Section {
                heading: Some("Project Tips".into()),
                items: vec![Item::Listed {
                    name: "style".into(),
                    description: "Two-space indent.".into(),
                    path: "tips/style.md".into(),
                }],
            }],
        };
        assert!(resolved.render().unwrap().contains(
            "<Project Tips>\n<style>Two-space indent. Read tips/style.md for the full \
             file.</style>\n</Project Tips>"
        ));
    }

    // --- resolve: config entries → files on disk → Resolved (docs/context.md) ---

    use crate::config::ContextGrants;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("efference-ctx-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bare(path: &str) -> ContextEntry {
        ContextEntry {
            heading: None,
            source: ContextSource::Files(vec![path.into()]),
            grants: ContextGrants::default(),
            sort: None,
        }
    }

    fn labeled(heading: &str, paths: &[&str]) -> ContextEntry {
        ContextEntry {
            heading: Some(heading.into()),
            source: ContextSource::Files(paths.iter().map(|s| s.to_string()).collect()),
            grants: ContextGrants::default(),
            sort: None,
        }
    }

    #[test]
    fn resolve_bare_inline_file_uses_the_basename() {
        let dir = tmpdir("bare-inline");
        std::fs::write(dir.join("CLAUDE.md"), "# efference\nbody\n").unwrap();
        let resolved = resolve(&[bare("CLAUDE.md")], &dir).unwrap();
        assert_eq!(
            resolved.sections,
            vec![Section {
                heading: None,
                items: vec![Item::Inline {
                    name: "CLAUDE".into(),
                    path: "CLAUDE.md".into(),
                    body: "# efference\nbody\n".into(),
                }],
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Skills-compat: a file with `name` + `description` becomes a *listed* stub tagged by its
    /// frontmatter name (preferred over the basename).
    #[test]
    fn resolve_prefers_frontmatter_name_and_lists_a_described_file() {
        let dir = tmpdir("fm-name");
        std::fs::write(
            dir.join("v.md"),
            "---\nname: verify\ndescription: Drive it.\n---\nbody\n",
        )
        .unwrap();
        let resolved = resolve(&[bare("v.md")], &dir).unwrap();
        assert_eq!(
            resolved.sections,
            vec![Section {
                heading: None,
                items: vec![Item::Listed {
                    name: "verify".into(),
                    description: "Drive it.".into(),
                    path: "v.md".into(),
                }],
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A labeled entry resolving to a single file collapses: the label is the file's tag, no wrapper.
    #[test]
    fn resolve_labeled_single_file_collapses_label_to_the_tag() {
        let dir = tmpdir("labeled-single");
        std::fs::write(dir.join("arch.md"), "the architecture\n").unwrap();
        let resolved = resolve(&[labeled("Architecture", &["arch.md"])], &dir).unwrap();
        assert_eq!(
            resolved.sections,
            vec![Section {
                heading: None,
                items: vec![Item::Inline {
                    name: "Architecture".into(),
                    path: "arch.md".into(),
                    body: "the architecture\n".into(),
                }],
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A labeled entry resolving to many files (a directory) wraps its `.md` children, name-sorted,
    /// each tagged individually.
    #[test]
    fn resolve_labeled_dir_wraps_sorted_members() {
        let dir = tmpdir("labeled-dir");
        let skills = dir.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("release.md"),
            "---\nname: release\ndescription: Cut a release.\n---\nb\n",
        )
        .unwrap();
        std::fs::write(
            skills.join("verify.md"),
            "---\nname: verify\ndescription: Drive the flow.\n---\nb\n",
        )
        .unwrap();
        // A non-markdown file in the dir is ignored.
        std::fs::write(skills.join("notes.txt"), "ignore me").unwrap();
        let resolved = resolve(&[labeled("Skills", &["skills"])], &dir).unwrap();
        assert_eq!(
            resolved.sections,
            vec![Section {
                heading: Some("Skills".into()),
                items: vec![
                    Item::Listed {
                        name: "release".into(),
                        description: "Cut a release.".into(),
                        path: "skills/release.md".into(),
                    },
                    Item::Listed {
                        name: "verify".into(),
                        description: "Drive the flow.".into(),
                        path: "skills/verify.md".into(),
                    },
                ],
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The Claude-Code skills layout — one directory per skill holding a `SKILL.md` — lands as
    /// one listing per skill via the recursive walk. Hidden children (a `.cache` dir, a dotfile)
    /// are skipped; the walk is sorted by display path; and a symlink cycle (here, a link back
    /// to the root) is walked once, not forever — the cycle guard skips the re-entry.
    #[test]
    fn resolve_dir_walks_recursively_for_the_skills_layout() {
        let dir = tmpdir("skills-tree");
        let skills = dir.join("skills");
        std::fs::create_dir_all(skills.join("alpha")).unwrap();
        std::fs::create_dir_all(skills.join("beta")).unwrap();
        std::fs::create_dir_all(skills.join(".cache")).unwrap();
        std::fs::write(
            skills.join("alpha/SKILL.md"),
            "---\nname: alpha\ndescription: First.\n---\nb\n",
        )
        .unwrap();
        std::fs::write(
            skills.join("beta/SKILL.md"),
            "---\nname: beta\ndescription: Second.\n---\nb\n",
        )
        .unwrap();
        std::fs::write(skills.join(".cache/ignored.md"), "not content\n").unwrap();
        std::fs::write(skills.join(".hidden.md"), "not content\n").unwrap();
        std::os::unix::fs::symlink(&skills, skills.join("loop")).unwrap();

        let resolved = resolve(&[labeled("Skills", &["skills"])], &dir).unwrap();
        assert_eq!(
            resolved.sections,
            vec![Section {
                heading: Some("Skills".into()),
                items: vec![
                    Item::Listed {
                        name: "alpha".into(),
                        description: "First.".into(),
                        path: "skills/alpha/SKILL.md".into(),
                    },
                    Item::Listed {
                        name: "beta".into(),
                        description: "Second.".into(),
                        path: "skills/beta/SKILL.md".into(),
                    },
                ],
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A symlinked directory inside a listed tree is a real layout (a skill collection linked in
    /// from another repo) — the walk follows it, and files land under the *link's* display path,
    /// the path the operator's tree presents.
    #[test]
    fn resolve_follows_a_symlinked_directory() {
        let dir = tmpdir("skills-symlink");
        let skills = dir.join("skills");
        let external = dir.join("external");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(
            external.join("SKILL.md"),
            "---\nname: linked\ndescription: From elsewhere.\n---\nb\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&external, skills.join("linked")).unwrap();

        let resolved = resolve(&[labeled("Skills", &["skills"])], &dir).unwrap();
        assert_eq!(
            resolved.sections,
            vec![Section {
                // Resolving to a single file, the entry collapses: the operator's label is the
                // tag (label > frontmatter name — the settled precedence), no wrapper.
                heading: None,
                items: vec![Item::Listed {
                    name: "Skills".into(),
                    description: "From elsewhere.".into(),
                    path: "skills/linked/SKILL.md".into(),
                }],
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_missing_path_fails_closed() {
        let dir = tmpdir("missing");
        let err = resolve(&[bare("nope.md")], &dir).unwrap_err();
        assert!(matches!(err, ResolveError::Io(..)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_unterminated_frontmatter_fails_closed() {
        let dir = tmpdir("unterm");
        std::fs::write(dir.join("bad.md"), "---\nname: x\nno close\n").unwrap();
        let err = resolve(&[bare("bad.md")], &dir).unwrap_err();
        assert!(matches!(err, ResolveError::Frontmatter(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- exec sections (docs/context.md) ---

    fn exec_entry(label: &str, command: &str) -> ContextEntry {
        ContextEntry {
            heading: Some(label.into()),
            source: ContextSource::Exec(command.into()),
            grants: ContextGrants::default(),
            sort: None,
        }
    }

    /// An exec entry is just another row: its stdout becomes a section at its position in
    /// document order — a dynamic section sitting *between* static files — collapsed like a
    /// labeled single file (the label is the tag, no wrapper).
    #[test]
    fn resolve_exec_section_lands_in_document_order() {
        let dir = tmpdir("exec-order");
        std::fs::write(dir.join("a.md"), "first\n").unwrap();
        std::fs::write(dir.join("b.md"), "last\n").unwrap();
        let resolved = resolve(
            &[
                bare("a.md"),
                exec_entry("Date", "echo from a command"),
                bare("b.md"),
            ],
            &dir,
        )
        .unwrap();
        assert_eq!(resolved.sections.len(), 3);
        assert_eq!(
            resolved.sections[1],
            Section {
                heading: None,
                items: vec![Item::Exec {
                    name: "Date".into(),
                    command: "echo from a command".into(),
                    body: "from a command\n".into(),
                }],
            }
        );
        let rendered = resolved.render().unwrap();
        let date = rendered.find("<Date>\nfrom a command\n</Date>").unwrap();
        assert!(rendered.find("first").unwrap() < date && date < rendered.find("last").unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The command runs with the session cwd, so relative commands mean what they mean in the
    /// bash tool; empty stdout is an empty body, not an error (bytes are bytes).
    #[test]
    fn resolve_exec_runs_in_base_and_allows_empty_stdout() {
        let dir = tmpdir("exec-cwd");
        std::fs::write(dir.join("marker.txt"), "here\n").unwrap();
        let resolved = resolve(&[exec_entry("Files", "ls marker.txt"), exec_entry("Quiet", "true")], &dir).unwrap();
        assert!(matches!(
            &resolved.sections[0].items[0],
            Item::Exec { body, .. } if body == "marker.txt\n"
        ));
        assert!(matches!(
            &resolved.sections[1].items[0],
            Item::Exec { body, .. } if body.is_empty()
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Fail closed, loudly: a non-zero exit is a config error carrying the command's stderr —
    /// never a silently dropped section.
    #[test]
    fn exec_nonzero_exit_fails_closed_with_stderr() {
        let dir = tmpdir("exec-fail");
        let err = resolve(&[exec_entry("Broken", "echo oops >&2; exit 3")], &dir).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exit status: 3") && msg.contains("oops"), "{msg}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A hung command can't hang boot: past the timeout the process group is killed and the
    /// load fails closed.
    #[test]
    fn exec_timeout_fails_closed() {
        let dir = tmpdir("exec-hang");
        let err = run_exec("sleep 30", &dir, std::time::Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Binary stdout in a text preamble is a config mistake — fail closed, don't mangle.
    #[test]
    fn exec_non_utf8_stdout_fails_closed() {
        let dir = tmpdir("exec-binary");
        let err = resolve(&[exec_entry("Binary", r"printf '\xff\xfe'")], &dir).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
