//! Rendering a tool call and its result — the call's blocks, diffs, and read markers.
//! What a call's arguments are and how its signature lays out is `signature.rs`.

use std::borrow::Cow;

use super::signature::CallView;
use super::*;

/// Mirrors `names::tools::display_name`: the namespace boundary is the *first* `__`, so a
/// server's own `__`-bearing tool name keeps it. A bare built-in name has no server
/// half. Display only — the dotted form is not a name anything is looked up by.
pub(in crate::tui) fn split_tool_name(wire_name: &str) -> (Option<&str>, &str) {
    match wire_name.split_once(crate::record::names::tools::NAMESPACE_SEPARATOR) {
        Some((server, tool)) => (Some(server), tool),
        None => (None, wire_name),
    }
}

/// The language a call's string blobs are in: what it declared, else the extension
/// of the path it operates on.
pub(in crate::tui) fn blob_lang(
    name: &str,
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    // A bash command is shell whatever file it touches.
    if name == crate::record::names::tools::BASH {
        return Some("bash".to_string());
    }
    let str_arg = |key: &str| object.get(key).and_then(|v| v.as_str());
    str_arg("language").map(str::to_string).or_else(|| {
        str_arg("file_path")
            .or_else(|| str_arg("path"))
            .and_then(lang_for_path)
            .map(str::to_string)
    })
}

/// A call as the blocks under its title: the signature (laid out at render time,
/// `DocumentNode::Signature`), then each argument that is content rather than an
/// argument, under its key — what both the committed call entry and the live one
/// render (docs/tui.md). `arriving` marks the input as still streaming.
pub(in crate::tui) fn call_blocks(view: CallView, arriving: bool) -> Vec<DocumentNode<'static>> {
    let bodies = view.bodies.clone();
    let mut blocks = vec![DocumentNode::signature(view, arriving)];
    for body in bodies {
        let content = if body.prose {
            markdown(&body.text)
        } else {
            vec![code_block(body.lang.as_deref(), &body.text)]
        };
        match body.key {
            Some(key) => blocks.push(DocumentNode::Captioned {
                caption: vec![dim_span(&key)],
                nodes: content,
            }),
            None => blocks.extend(content),
        }
    }
    blocks
}

/// The call's intent as its title row: what it was for, ahead of what it was
/// (docs/web-client.md, "Detail by recency").
pub(in crate::tui) fn intent_header(intent: &str) -> Vec<Span<'static>> {
    vec![Span {
        text: intent.to_string().into(),
        style: SpanStyle::Plain,
        action: None,
    }]
}

/// The body shown under a tool-result header. Known tools get a tool-specific
/// representation of their raw output — an edit tool's `===DIFF===` becomes a
/// colored [`DocumentNode::Diff`], and a `read` result drops its sentinel framing
/// and highlights by file type. Triaged / elided / error results, and anything
/// unrecognized, fall back to the content as a plain code block.
pub(in crate::tui) fn result_body(
    tool: Option<&str>,
    raw: &RawResult,
    wire: &WireState,
    content: &str,
) -> Vec<DocumentNode<'static>> {
    if !raw.is_error && matches!(wire, WireState::Raw) {
        match tool {
            Some(t) if is_diff_tool(t) => {
                if let Some(diff) = parse_diff(content) {
                    return vec![diff];
                }
            }
            Some("read") => {
                if let Some(body) = read_result(content) {
                    return vec![body];
                }
            }
            _ => {}
        }
    }
    vec![code_block(None, content)]
}

/// The body shown under a **triaged** tool-result header: the same tool-specific
/// representation [`result_body`] gives the raw output, restricted to the lines the
/// verdict kept (docs/tui.md). The parse runs over the whole result — so a `read`
/// still loses its sentinel framing and is highlighted by extension, and an edit's
/// diff lines are still classified — and the kept-line filter is applied to the
/// *rows that parse produced*, each of which carries the raw content line it came
/// from. That ordering is what lets a triaged view keep the tool's rendering
/// instead of falling back to plain text.
///
/// `kept` is the verdict resolved against the content (`project::selection`), as
/// `(1-based content line, text)`. Falls back to a plain numbered block for an
/// unrecognized tool, an error result, or a parse that found no marker.
pub(in crate::tui) fn triaged_body(
    tool: &str,
    raw: &RawResult,
    kept: &[(u32, &str)],
) -> Vec<DocumentNode<'static>> {
    if kept.is_empty() {
        return Vec::new();
    }
    let keeps: HashSet<u32> = kept.iter().map(|(n, _)| *n).collect();
    if !raw.is_error {
        if is_diff_tool(tool)
            && let Some(rows) = diff_rows(&raw.content)
        {
            // A dropped diff line leaves no gap marker — `Diff` has no gutter to
            // state one in. The result header's `kept N of M lines` is what says the
            // view is partial.
            let lines: Vec<DiffLine> = rows
                .into_iter()
                .filter(|(n, _)| keeps.contains(n))
                .map(|(_, line)| line)
                .collect();
            if !lines.is_empty() {
                return vec![DocumentNode::diff(lines)];
            }
        }
        if tool == "read"
            && let Some(ReadRows { lang, lines: rows }) = read_rows(&raw.content)
        {
            // A kept sentinel line simply doesn't appear: it is framing, not file
            // content, and the raw view drops it too.
            let lines: Vec<(u32, Cow<'static, str>)> = rows
                .into_iter()
                .filter(|(n, _)| keeps.contains(n))
                .map(|(n, text)| (n, text.to_string().into()))
                .collect();
            if !lines.is_empty() {
                return vec![DocumentNode::numbered_code(lang, lines)];
            }
        }
    }
    vec![DocumentNode::numbered_code(
        None::<&str>,
        kept.iter()
            .map(|(n, text)| (*n, text.to_string().into()))
            .collect::<Vec<(u32, Cow<'static, str>)>>(),
    )]
}

/// The known tools whose result carries a `===DIFF===` section.
pub(in crate::tui) fn is_diff_tool(name: &str) -> bool {
    matches!(name, "edit" | "undo_edit" | "retarget_edit")
}

/// Strip a `read` result's sentinel framing (`==<token> BEGIN <path>, BYTES: n
/// <token>==` … matching `END`) and render the file body as a code block, syntax-
/// highlighted by the path's extension. Returns `None` if the marker isn't present
/// (so the caller falls back to a plain code block).
pub(in crate::tui) fn read_result(content: &str) -> Option<DocumentNode<'static>> {
    let ReadRows { lang, lines } = read_rows(content)?;
    let body = lines.into_iter().map(|(_, l)| l).collect::<Vec<_>>();
    Some(code_block(lang, &body.join("\n")))
}

/// A `read` result's file lines, each paired with its **1-based line number in the
/// raw result** — the coordinate system the triage verdict is expressed in, so a
/// verdict can be applied to these rows. Sentinel lines are dropped (they are
/// framing, not content) and take their numbers with them, so the gutter's jumps
/// state both what triage dropped and where the framing was. `None` if no marker is
/// present.
fn read_rows(content: &str) -> Option<ReadRows<'_>> {
    let mut path: Option<String> = None;
    let mut saw_marker = false;
    let mut body: Vec<(u32, &str)> = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        match read_marker(line) {
            Some(ReadMarker::Begin(found_path)) => {
                saw_marker = true;
                path = Some(found_path);
            }
            Some(ReadMarker::End) => saw_marker = true,
            None => body.push((idx as u32 + 1, line)),
        }
    }
    if !saw_marker {
        return None;
    }
    Some(ReadRows {
        lang: path.as_deref().and_then(lang_for_path),
        lines: body,
    })
}

/// A parsed `read` result: the syntect language its path implies, and its file lines
/// numbered by their position in the raw result (see [`read_rows`]).
struct ReadRows<'c> {
    lang: Option<&'static str>,
    lines: Vec<(u32, &'c str)>,
}

/// A `read` result's sentinel line.
#[derive(Debug, PartialEq)]
pub(in crate::tui) enum ReadMarker {
    /// The `BEGIN` line, with the path it names.
    Begin(String),
    /// The `END` line.
    End,
}

/// Classify a `read` sentinel line: a `BEGIN` or an `END` line, or `None` for ordinary
/// file content.
pub(in crate::tui) fn read_marker(line: &str) -> Option<ReadMarker> {
    let inner = line.strip_prefix("==")?.strip_suffix("==")?;
    if let Some(rest) = inner.split_once(" BEGIN ").map(|(_, r)| r) {
        // `BYTES:` is the current spelling; `LENGTH:` is what older session logs hold.
        let path = rest
            .split(", BYTES:")
            .next()
            .and_then(|r| r.split(", LENGTH:").next())
            .unwrap_or(rest)
            .trim();
        Some(ReadMarker::Begin(path.to_string()))
    } else if inner.contains(" END ") {
        Some(ReadMarker::End)
    } else {
        None
    }
}

/// A syntect language token for a file path's extension, or `None` for plain text.
pub(in crate::tui) fn lang_for_path(path: &str) -> Option<&'static str> {
    Some(match path.rsplit('.').next()? {
        "rs" => "rust",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "sh" | "bash" => "bash",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "css" => "css",
        _ => return None,
    })
}

/// Parse an edit tool's result into a colored diff: take the text after the
/// `===DIFF===` marker and classify each line by its leading ` `/`+`/`-`/`@`.
/// Returns `None` when there's no diff section (so the caller falls back to a
/// plain code block).
pub(in crate::tui) fn parse_diff(content: &str) -> Option<DocumentNode<'static>> {
    let lines = diff_rows(content)?
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    Some(DocumentNode::diff(lines))
}

/// The diff section's lines, classified, each paired with its **1-based line number
/// in the raw result** — the coordinate system the triage verdict is expressed in,
/// so [`triaged_body`] can restrict them without re-parsing. The prose above the
/// `===DIFF===` marker, and the marker itself, are not diff lines and carry no row.
fn diff_rows(content: &str) -> Option<Vec<(u32, DiffLine)>> {
    let mut lines = content.lines().enumerate();
    lines.find(|(_, line)| line.contains("===DIFF==="))?;
    let rows: Vec<(u32, DiffLine)> = lines
        .skip_while(|(_, line)| line.is_empty())
        .map(|(idx, line)| {
            let kind = match line.chars().next() {
                Some('+') => DiffLineKind::Added,
                Some('-') => DiffLineKind::Removed,
                Some('@') => DiffLineKind::Hunk,
                _ => DiffLineKind::Context,
            };
            (
                idx as u32 + 1,
                DiffLine {
                    kind,
                    text: line.to_string(),
                },
            )
        })
        .collect();
    if rows.iter().all(|(_, line)| line.text.trim().is_empty()) {
        return None;
    }
    Some(rows)
}
