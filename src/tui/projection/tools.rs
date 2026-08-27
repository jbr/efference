//! Rendering a tool call and its result — argument summaries, diffs, and read markers.

use std::borrow::Cow;

use super::*;

/// The call's argument, split into what goes inline on the `❯ tool · …` header
/// (a short summary) and what overflows into the call entry's body (a multi-line
/// `bash` command, or a JSON envelope for an unrecognized tool). Known tools get a
/// tool-specific summary — the file tools surface their path/pattern; `bash` shows
/// a single-line command inline, a multi-line one as a shell block.
///
/// Takes the name and input rather than a [`ToolCall`] so the **live** block can
/// render through it too, from the settled half of a partly-arrived input
/// (`projection/partial.rs`). One function for both is what keeps the streaming
/// header from jumping when the call commits: it fills in, it doesn't change shape.
pub(in crate::tui) fn call_args(
    name: &str,
    input: &serde_json::Value,
) -> (Option<Vec<Span<'static>>>, Vec<DocumentNode<'static>>) {
    let call_input = input;
    let input = |key| call_input.get(key).and_then(|v| v.as_str());
    match name {
        n if n == crate::bash::NAME => {
            if let Some(command) = input("command") {
                return if command.contains('\n') {
                    (None, vec![code_block(Some("bash"), command)])
                } else {
                    (Some(vec![inline_code(command)]), vec![])
                };
            }
        }
        "read" => {
            if let Some(paths) = call_input.get("paths").and_then(|v| v.as_array()) {
                let joined = paths
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return (Some(vec![inline_code(joined)]), vec![]);
            }
        }
        "search" => {
            if let Some(pattern) = input("pattern") {
                let mut spans = vec![inline_code(pattern.to_string())];
                if let Some(path) = input("path") {
                    spans.push(Span::plain("  in  "));
                    spans.push(inline_code(path.to_string()));
                }
                return (Some(spans), vec![]);
            }
        }
        "list" => {
            if let Some(path) = input("path") {
                return (Some(vec![inline_code(path.to_string())]), vec![]);
            }
        }
        "read_image" => {
            if let Some(path) = input("path") {
                let mut spans = vec![inline_code(path.to_string())];
                // `long_edge` changes what the call *costs* — image tokens scale
                // with area (docs/tools.md) — so the requested resolution belongs
                // beside the path rather than in a dropped envelope.
                if let Some(long_edge) = call_input.get("long_edge").and_then(|v| v.as_u64()) {
                    spans.push(Span {
                        text: format!(" · long_edge {long_edge}").into(),
                        style: SpanStyle::Comment,
                        action: None,
                    });
                }
                return (Some(spans), vec![]);
            }
        }
        "write" => {
            // The file being written *is* the call, so show it as the file: the path
            // inline, the contents as a code block highlighted by its extension. The
            // JSON envelope rendered `contents` as one escaped string — the `\n`s
            // literal, no highlighting — which is the least readable form of the one
            // thing worth reading here.
            if let Some(path) = input("path") {
                let mut spans = vec![inline_code(path.to_string())];
                // `append` and `overwrite` change what the call *does* to an existing
                // file, so they belong beside the path rather than in a dropped envelope.
                for (key, label) in [("append", " · append"), ("overwrite", " · overwrite")] {
                    if call_input.get(key).and_then(|v| v.as_bool()) == Some(true) {
                        spans.push(Span {
                            text: label.into(),
                            style: SpanStyle::Comment,
                            action: None,
                        });
                    }
                }
                let body = input("contents")
                    .map(|contents| vec![code_block(lang_for_path(path), contents)])
                    .unwrap_or_default();
                return (Some(spans), body);
            }
        }
        name if is_diff_tool(name) => {
            // The diff result is the representation — never dump the (verbose) edit
            // input. What the header owes the reader is *which file* changed and how:
            // the anchor snippet that used to sit here answered neither (it is a
            // fragment of the old text, which the diff below shows in full anyway),
            // and at 48 characters it read as a truncated sentence.
            let mut spans = Vec::new();
            if let Some(path) = input("file_path") {
                spans.push(inline_code(path.to_string()));
            }
            if let Some(operation) = input("operation") {
                if !spans.is_empty() {
                    spans.push(Span {
                        text: " · ".into(),
                        style: SpanStyle::Comment,
                        action: None,
                    });
                }
                spans.push(Span {
                    text: operation.to_string().into(),
                    style: SpanStyle::Comment,
                    action: None,
                });
            }
            // `undo_edit` takes no arguments at all, so it is only ever its name.
            return ((!spans.is_empty()).then_some(spans), vec![]);
        }
        _ => {}
    }
    let pretty =
        serde_json::to_string_pretty(call_input).unwrap_or_else(|_| call_input.to_string());
    (None, vec![code_block(Some("json"), &pretty)])
}

/// The body shown under a tool-result header. Known tools get a tool-specific
/// representation of their raw output — an edit tool's `===DIFF===` becomes a
/// colored [`DocumentNode::Diff`], and a `read` result drops its sentinel framing
/// and highlights by file type. Distilled / elided / error results, and anything
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

/// The body shown under a **distilled** tool-result header: the same tool-specific
/// representation [`result_body`] gives the raw output, restricted to the lines the
/// verdict kept (docs/tui.md). The parse runs over the whole result — so a `read`
/// still loses its sentinel framing and is highlighted by extension, and an edit's
/// diff lines are still classified — and the kept-line filter is applied to the
/// *rows that parse produced*, each of which carries the raw content line it came
/// from. That ordering is what lets a triaged view keep the tool's rendering
/// instead of falling back to plain text.
///
/// `kept` is the verdict resolved against the content (`project::distillate`), as
/// `(1-based content line, text)`. Falls back to a plain numbered block for an
/// unrecognized tool, an error result, or a parse that found no marker.
pub(in crate::tui) fn distilled_body(
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

/// Strip a `read` result's sentinel framing (`==<token> BEGIN <path>, LENGTH: n
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
            Some(found_path) => {
                saw_marker = true;
                if found_path.is_some() {
                    path = found_path;
                }
            }
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

/// Classify a `read` sentinel line: `Some(Some(path))` for a `BEGIN` line,
/// `Some(None)` for an `END` line, `None` for ordinary file content.
pub(in crate::tui) fn read_marker(line: &str) -> Option<Option<String>> {
    let inner = line.strip_prefix("==")?.strip_suffix("==")?;
    if let Some(rest) = inner.split_once(" BEGIN ").map(|(_, r)| r) {
        let path = rest.split(", LENGTH:").next().unwrap_or(rest).trim();
        Some(Some(path.to_string()))
    } else if inner.contains(" END ") {
        Some(None)
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
/// so [`distilled_body`] can restrict them without re-parsing. The prose above the
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
