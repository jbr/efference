//! Rendering a tool call and its result — the call signature, diffs, and read markers.

use std::borrow::Cow;

use super::*;

/// How wide a rendered value may be and still sit inside the signature.
const INLINE_BUDGET: usize = 48;

/// One argument as it reads inside the signature.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) struct SigArg {
    pub key: String,
    /// The value (`"serde"`, `4`, `true`) or, when it did not fit, the shape
    /// standing in for it (`505 lines`, `{3 keys}`).
    pub text: String,
    /// True when `text` is a shape rather than the value.
    pub elided: bool,
}

/// A value too big for the signature, rendered below it.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) struct CallBody {
    /// The argument this is the value of; `None` for `bash`'s bare command.
    pub key: Option<String>,
    pub lang: Option<String>,
    pub text: String,
    /// Rendered as prose rather than as a code block: a body that is a message,
    /// like a hand-off's brief, not a value.
    pub prose: bool,
}

/// A call as pseudocode — the shape both clients render (docs/web-client.md,
/// "A call reads as a signature"; docs/tui.md).
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) struct CallView {
    /// The `<server>` half of a namespaced name; `None` for a built-in.
    pub server: Option<String>,
    /// The `<tool>` half, which for a built-in is the whole name.
    pub tool: String,
    /// The signature's arguments. Empty renders `check_inbox()`; `None` means the
    /// call renders bare (the `bash` cases).
    pub args: Option<Vec<SigArg>>,
    /// `bash`'s single-line command, rendered after the name unquoted. Set only
    /// when `args` is `None`.
    pub target: Option<String>,
    pub bodies: Vec<CallBody>,
}

/// Mirrors `mcp::display_name`: the namespace boundary is the *first* `__`, so a
/// server's own `__`-bearing tool name keeps it. A bare built-in name has no server
/// half. Display only — the dotted form is not a name anything is looked up by.
pub(in crate::tui) fn split_tool_name(wire_name: &str) -> (Option<&str>, &str) {
    match wire_name.split_once(crate::mcp::NAMESPACE_SEPARATOR) {
        Some((server, tool)) => (Some(server), tool),
        None => (None, wire_name),
    }
}

/// Every call as a pseudocode signature — total, schema-free, and so correct for a
/// tool this client has never heard of, which matters because MCP servers are
/// user-mountable and their argument shapes are arbitrary. The rules, and the
/// reasoning behind each, are stated once in docs/web-client.md ("A call reads as a
/// signature"); this is the TUI's half of them (docs/tui.md).
///
/// Takes the name and input rather than a [`ToolCall`] so the **live** block can
/// render through it too, from the settled half of a partly-arrived input
/// (`projection/partial.rs`). One function for both is what keeps the streaming
/// header from jumping when the call commits: it fills in, it doesn't change shape.
pub(in crate::tui) fn call_view(name: &str, input: &serde_json::Value) -> CallView {
    let (server, tool) = split_tool_name(name);
    let base = |args, target, bodies| CallView {
        server: server.map(str::to_string),
        tool: tool.to_string(),
        args,
        target,
        bodies,
    };
    let serde_json::Value::Object(object) = input else {
        // Not an object at all: nothing to name, so the value itself is the body.
        return base(
            Some(Vec::new()),
            None,
            vec![CallBody {
                key: None,
                lang: Some("json".to_string()),
                text: pretty(input),
                prose: false,
            }],
        );
    };
    let str_arg = |key: &str| object.get(key).and_then(|v| v.as_str());

    // A hand-off's brief is a message to the session that takes over
    // (docs/tools.md): labelled with whom it is for and read as prose, so it is not
    // mistaken for the agent addressing the operator.
    if name == crate::control_tools::HAND_OFF_WIRE_NAME
        && let Some(brief) = str_arg("brief")
    {
        return base(
            Some(Vec::new()),
            None,
            vec![CallBody {
                key: Some("to the new session".to_string()),
                lang: None,
                text: brief.to_string(),
                prose: true,
            }],
        );
    }

    if name == crate::bash::NAME
        && let Some(command) = str_arg("command")
    {
        return if command.contains('\n') {
            base(
                None,
                None,
                vec![CallBody {
                    key: None,
                    lang: Some("bash".to_string()),
                    text: command.to_string(),
                    prose: false,
                }],
            )
        } else {
            base(None, Some(command.to_string()), Vec::new())
        };
    }

    let lang = blob_lang(name, object);
    let suppress_bodies = is_diff_tool(name);
    let mut args: Vec<SigArg> = Vec::new();
    let mut bodies: Vec<CallBody> = Vec::new();
    for (key, value) in object {
        // `intent` is already the call's leading line.
        if key == crate::tools::INTENT {
            continue;
        }
        if let Some(inline) = inline_value(value) {
            args.push(SigArg {
                key: key.clone(),
                text: inline,
                elided: false,
            });
            continue;
        }
        args.push(SigArg {
            key: key.clone(),
            text: shape_of(value),
            elided: true,
        });
        if suppress_bodies {
            continue;
        }
        bodies.push(match value {
            serde_json::Value::String(text) => CallBody {
                key: Some(key.clone()),
                lang: lang.clone(),
                text: text.clone(),
                prose: false,
            },
            other => CallBody {
                key: Some(key.clone()),
                lang: Some("json".to_string()),
                text: pretty(other),
                prose: false,
            },
        });
    }
    // Inline arguments first — they are the call's identity. Stable, so the input's
    // own order still shows within each group.
    args.sort_by_key(|arg| arg.elided);
    base(Some(args), None, bodies)
}

/// The value as it reads inside the signature, or `None` when it does not fit:
/// strings quoted, every other scalar bare, a collection as compact JSON while that
/// stays inside [`INLINE_BUDGET`].
fn inline_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => (!text.contains('\n')
            && text.chars().count() <= INLINE_BUDGET)
            .then(|| serde_json::Value::String(text.clone()).to_string()),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Some(value.to_string())
        }
        collection => {
            let compact = collection.to_string();
            (compact.chars().count() <= INLINE_BUDGET).then_some(compact)
        }
    }
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// What stands in for an elided value: how much of it there is, and of what.
fn shape_of(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => {
            let lines = text.lines().count();
            if lines > 1 {
                plural(lines, "line")
            } else {
                plural(text.chars().count(), "char")
            }
        }
        serde_json::Value::Array(items) => format!("[{}]", plural(items.len(), "item")),
        serde_json::Value::Object(entries) => format!("{{{}}}", plural(entries.len(), "key")),
        _ => "…".to_string(),
    }
}

/// The language a call's string blobs are in: what it declared, else the extension
/// of the path it operates on.
fn blob_lang(name: &str, object: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    // A bash command is shell whatever file it touches.
    if name == crate::bash::NAME {
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

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The signature as header spans, plus the blocks that go under it — what both the
/// committed call entry and the live one render. `arriving` marks the input as still
/// streaming, which puts the `…` *inside* the parentheses so the signature never
/// reads as closed before it is.
pub(in crate::tui) fn call_spans(
    name: &str,
    input: &serde_json::Value,
    arriving: bool,
) -> (Vec<Span<'static>>, Vec<DocumentNode<'static>>) {
    let view = call_view(name, input);
    let punct = |text: &str| Span {
        text: text.to_string().into(),
        style: SpanStyle::Punctuation,
        action: None,
    };
    let mut spans = Vec::new();
    // The server segment is dimmed, and stays a segment of its own: built-in versus
    // mounted-server is the `open_world_hint` distinction (docs/web-client.md).
    if let Some(server) = &view.server {
        spans.push(Span {
            text: format!("{server}.").into(),
            style: SpanStyle::Comment,
            action: None,
        });
    }
    spans.push(inline_code_styled(
        view.tool.clone(),
        SpanStyle::FunctionName,
    ));
    match &view.args {
        Some(args) => {
            spans.push(punct("("));
            for (idx, arg) in args.iter().enumerate() {
                if idx > 0 {
                    spans.push(punct(", "));
                }
                spans.push(inline_code_styled(arg.key.clone(), SpanStyle::FieldName));
                spans.push(punct(": "));
                spans.push(if arg.elided {
                    Span {
                        text: arg.text.clone().into(),
                        style: SpanStyle::Comment,
                        action: None,
                    }
                } else {
                    inline_code(arg.text.clone())
                });
            }
            if arriving {
                if !args.is_empty() {
                    spans.push(punct(", "));
                }
                spans.push(dim_span("…"));
            }
            spans.push(punct(")"));
        }
        // `bash`: the command reads better as a shell line than as a quoted argument.
        None => {
            if let Some(target) = &view.target {
                spans.push(Span {
                    text: " · ".into(),
                    style: SpanStyle::Comment,
                    action: None,
                });
                spans.push(inline_code(target.clone()));
            }
            if arriving {
                spans.push(dim_span("…"));
            }
        }
    }

    let mut blocks = Vec::new();
    for body in view.bodies {
        if let Some(key) = body.key {
            blocks.push(DocumentNode::styled_lines([dim_span(&format!("{key}:"))]));
        }
        if body.prose {
            blocks.extend(markdown(&body.text));
        } else {
            blocks.push(code_block(body.lang.as_deref(), &body.text));
        }
    }
    (spans, blocks)
}

#[cfg(test)]
mod test;

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
