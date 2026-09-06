//! A pseudo-YAML pretty-printer for [`serde_json::Value`] — a deliberately lossy,
//! **read-only** rendering for eyeballing the dev `wire` view (`efference dev wire
//! --format yaml`). It trades round-trip fidelity for readability of deeply
//! nested, escape-heavy JSON: multi-line strings (tool-schema descriptions, the
//! system prompt, tool-result bodies) unfold into indented blocks instead of
//! `\n`-littered one-liners, and arrays of scalars fold onto a single line
//! (`required: source, destination`). The output is intentionally ambiguous and
//! not parseable back — that's the trade we want for a human inspection format.
//!
//! Object keys render in [`serde_json::Map`] order, which is sorted (the crate
//! runs serde_json without `preserve_order`, docs/log-and-projection.md) — deterministic, like the
//! JSON view. Callers that want a bespoke top-level order (the request envelope
//! prefers `model`/`system` before the big `tools`/`messages` blocks) drive
//! [`keyed`] per field instead of handing the whole object to [`to_pseudo_yaml`].

use serde_json::{Map, Value};

/// One level of nesting. Two spaces, YAML-style.
const INDENT: &str = "  ";

/// Render `value` as pseudo-YAML (see module docs). A scalar renders as a single
/// line; a container renders as an indented block.
pub fn to_pseudo_yaml(value: &Value) -> String {
    let lines = match render(value) {
        Rendered::Inline(s) => vec![s],
        Rendered::Block(lines) => lines,
    };
    join(lines)
}

/// Render a single `key: value` entry to its lines — a scalar value stays on the
/// key's line, a container drops to an indented block beneath it. Exposed so a
/// caller can emit a chosen key order (the renderer otherwise follows the map's
/// own sorted order).
pub fn keyed(key: &str, value: &Value) -> Vec<String> {
    match render(value) {
        Rendered::Inline(s) => vec![format!("{key}: {s}")],
        Rendered::Block(block) => {
            let mut lines = vec![format!("{key}:")];
            lines.extend(block.into_iter().map(|l| format!("{INDENT}{l}")));
            lines
        }
    }
}

/// A value rendered either as a single inline token or as a multi-line block
/// whose lines are indented relative to column 0 (the caller re-indents).
enum Rendered {
    Inline(String),
    Block(Vec<String>),
}

fn render(value: &Value) -> Rendered {
    match value {
        Value::Null => Rendered::Inline("null".into()),
        Value::Bool(b) => Rendered::Inline(b.to_string()),
        Value::Number(n) => Rendered::Inline(n.to_string()),
        Value::String(s) => render_string(s),
        Value::Array(items) => render_array(items),
        Value::Object(map) => render_object(map),
    }
}

/// A single-line string stays inline; a multi-line one becomes a block, one line
/// per physical line (escapes already decoded by serde, so `\n` is a real break,
/// `\"` a bare quote, etc. — the whole point of this view).
fn render_string(s: &str) -> Rendered {
    if s.contains('\n') {
        Rendered::Block(s.split('\n').map(str::to_string).collect())
    } else {
        Rendered::Inline(s.to_string())
    }
}

/// An array of pure scalars folds onto one line (`a, b, c`); anything richer
/// becomes a `- `-prefixed block list.
fn render_array(items: &[Value]) -> Rendered {
    if items.is_empty() {
        return Rendered::Inline("[]".into());
    }
    let rendered: Vec<Rendered> = items.iter().map(render).collect();
    if let Some(scalars) = all_inline(&rendered) {
        return Rendered::Inline(scalars.join(", "));
    }
    let mut lines = Vec::new();
    for r in rendered {
        match r {
            Rendered::Inline(s) => lines.push(format!("- {s}")),
            // The element's first line rides the `- `; the rest indent to align
            // their content past it.
            Rendered::Block(block) => {
                for (i, line) in block.into_iter().enumerate() {
                    if i == 0 {
                        lines.push(format!("- {line}"));
                    } else {
                        lines.push(format!("{INDENT}{line}"));
                    }
                }
            }
        }
    }
    Rendered::Block(lines)
}

fn render_object(map: &Map<String, Value>) -> Rendered {
    if map.is_empty() {
        return Rendered::Inline("{}".into());
    }
    let mut lines = Vec::new();
    for (key, value) in map {
        lines.extend(keyed(key, value));
    }
    Rendered::Block(lines)
}

/// `Some(tokens)` if every element rendered inline (so the array can fold onto
/// one line); `None` the moment one needs a block.
fn all_inline(rendered: &[Rendered]) -> Option<Vec<String>> {
    rendered
        .iter()
        .map(|r| match r {
            Rendered::Inline(s) => Some(s.clone()),
            Rendered::Block(_) => None,
        })
        .collect()
}

/// Join lines, trimming each line's trailing whitespace so blank lines inside a
/// multi-line string don't surface as runs of indentation spaces.
pub(crate) fn join(lines: Vec<String>) -> String {
    lines
        .iter()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod test;
