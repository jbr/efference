//! Parsing a markdown file's YAML-ish frontmatter (docs/context.md).

use super::*;

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
    /// Every top-level scalar key in the block, verbatim and unparsed — including the three named
    /// above. These are the columns a group's `sort` override addresses (docs/context.md): a file
    /// is a row, its frontmatter keys are that row's fields, and the sort decides typing per column
    /// (numeric when every present value parses as a number, else lexical). Nested structures are
    /// consumed, never flattened into here, so a `metadata:` map contributes no columns.
    pub fields: std::collections::BTreeMap<String, String>,
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
pub(super) struct BlockScalar {
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
pub(super) fn parse_frontmatter_line(line: &str, fm: &mut Frontmatter) -> Option<BlockScalar> {
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
pub(super) fn block_scalar_style(value: &str) -> Option<bool> {
    match value {
        ">" | ">-" | ">+" => Some(true),
        "|" | "|-" | "|+" => Some(false),
        _ => None,
    }
}

/// Assemble a finished block scalar and assign it: folded joins the non-blank lines with spaces
/// (the single-paragraph `description: >-` case real skill files use); literal keeps line breaks.
/// Chomping indicators are ignored — trailing whitespace never matters to these keys.
pub(super) fn close_block_scalar(fm: &mut Frontmatter, block: BlockScalar) {
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

/// Assign one frontmatter value to the key it names. Every key — known or not — is also recorded
/// verbatim in [`Frontmatter::fields`], which is what a group's `sort` override reads as columns
/// (docs/context.md). A repeated key keeps the last occurrence, matching the typed slots below.
/// An unrecognized `inline` value leaves the flag unset (but is still recorded as a field).
pub(super) fn set_frontmatter_key(fm: &mut Frontmatter, key: &str, value: &str) {
    fm.fields.insert(key.to_string(), value.to_string());
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
pub(super) fn unquote(s: &str) -> &str {
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
                name: fm.name.clone().unwrap_or_else(|| fallback_name.to_string()),
                description: fm.description.clone().unwrap_or_default(),
            },
            _ => Mode::Inline,
        }
    }
}

/// The display name a file falls back to when its frontmatter omits `name`: the **full basename**,
/// extension included (`docs/verify.md` → `verify.md`, `CLAUDE.md` → `CLAUDE.md`), or `"context"`
/// for a path with no final component.
///
/// The extension is kept because the name is also the render's tag (`<CLAUDE.md>…`,
/// docs/context.md) and a tag that names a real file the model can `read` is one the stem would
/// have hidden. Files whose frontmatter carries a `name` — skills — are unaffected. Changing this
/// moves the rendered preamble bytes, so it moves the cached prefix for any session whose config
/// lists a nameless file: the render pin is updated deliberately alongside it.
pub fn fallback_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "context".to_string())
}
