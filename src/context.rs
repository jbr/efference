//! `[context]` preamble builder — reading, frontmatter, and mode (backlog/preamble-context.md).
//!
//! The [`crate::config`] schema materializes the operator's `[context]` entries (what files, what
//! grants, what order). This module turns a file's *content* into a preamble section: it splits an
//! optional leading frontmatter block off the body and decides the file's **mode** — **inlined**
//! (the whole body rides in the cached preamble, the CLAUDE.md role) or **listed** (only a one-line
//! stub rides, the body fetched on demand via the `read` tool). Mode is the *file's* call, carried
//! in its frontmatter; the config never decides it (backlog/preamble-context.md).
//!
//! Frontmatter is recognized **only at the very start of the file** (first line `---`), so a `---`
//! horizontal rule mid-document is never mistaken for it. The parser is deliberately minimal —
//! `key: value` lines for the few keys we read (`name`, `description`, `inline`) — not a full YAML
//! engine: multiline scalars and nested structures are out of scope (and unnecessary for those
//! keys). Unknown keys are ignored so the frontmatter vocabulary can grow without a parser change.

use std::path::Path;

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

/// How a context file renders into the preamble (backlog/preamble-context.md).
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

/// Split an optional leading frontmatter block off `text`. Frontmatter exists **only** when the very
/// first line is the `---` delimiter; the block runs to the next `---` line, and the body is
/// everything after it. A `---` anywhere but the first line is ordinary markdown (a horizontal
/// rule), never frontmatter. An opened-but-unclosed block is [`UnterminatedFrontmatter`].
pub fn split_frontmatter(text: &str) -> Result<Split<'_>, UnterminatedFrontmatter> {
    let first_line_end = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
    if text[..first_line_end].trim() != "---" {
        return Ok(Split {
            frontmatter: None,
            body: text,
        });
    }
    let mut fm = Frontmatter::default();
    let mut offset = first_line_end;
    for line in text[first_line_end..].split_inclusive('\n') {
        if line.trim() == "---" {
            // Closing delimiter: the body is everything after this line.
            return Ok(Split {
                frontmatter: Some(fm),
                body: &text[offset + line.len()..],
            });
        }
        parse_frontmatter_line(line, &mut fm);
        offset += line.len();
    }
    Err(UnterminatedFrontmatter)
}

/// Parse one `key: value` frontmatter line into `fm`. Splits on the *first* colon (so a value may
/// itself contain colons), trims, and strips one layer of matching surrounding quotes. Unknown keys
/// and blank lines are ignored; an unrecognized `inline` value leaves the flag unset.
fn parse_frontmatter_line(line: &str, fm: &mut Frontmatter) {
    let Some((key, value)) = line.split_once(':') else {
        return;
    };
    let value = unquote(value.trim());
    match key.trim() {
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
/// (backlog/preamble-context.md). Each [`Section`] is a bare entry (no heading) or a labeled group.
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
/// Both render as a name tag `<name>…</name>` (backlog/preamble-context.md); the name is the file's
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
        Item::Inline { name, body, .. } => format!("<{name}>\n{}\n</{name}>", body.trim()),
        Item::Listed {
            name,
            description,
            path,
        } => format!("<{name}>{description} Read {path} for the full file.</{name}>"),
    }
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
}
