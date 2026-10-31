//! What a call's arguments are, and how its signature lays out at a width — the
//! TUI's port of the web client's `ui/signature.ts` and `ui/signatureLayout.ts`
//! (docs/web-client.md, "A call reads as a signature"; docs/tui.md). The two
//! clients state the same rules so a call reads the same in either.
//!
//! [`call_view`] is width-free: which arguments sit in the signature, which are
//! content rendered below it, and which of a built-in's arguments go unlabelled.
//! [`layout_signature`] breaks that view across rows at a column count, the way a
//! pretty-printer does. The layout runs at render time, where the width is known
//! (`DocumentNode::Signature`).

use serde_json::Value;

/// One argument as the signature holds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SigArg {
    /// The input key the value came from, whether or not the signature shows it.
    pub key: String,
    /// Shown ahead of the value (`from_line: 3`), or `None` for a built-in's primary
    /// argument, which reads positionally (`read("a.rs")`).
    pub label: Option<String>,
    /// The value itself. Never a multi-line string, nor a collection holding one:
    /// those are content, and render below as a [`CallBody`].
    pub value: Value,
}

/// An argument rendered below the signature as content rather than in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallBody {
    /// The argument this is the value of; `None` for `bash`'s bare command.
    pub key: Option<String>,
    pub lang: Option<String>,
    pub text: String,
    /// Rendered as prose rather than as code: a body that is a message, like a
    /// hand-off's brief, not a value.
    pub prose: bool,
}

/// How a `bash` call runs, when it says: backgrounded from the start, or after the
/// seconds it named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shell {
    pub background: bool,
    pub background_after: Option<u64>,
}

/// A call as pseudocode, before any width is known.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CallView {
    /// The call's `intent` — the call's title, ahead of the signature.
    pub intent: Option<String>,
    /// The `<server>` half of a namespaced name, shown dimmed ahead of the dot;
    /// `None` for a built-in. Built-in versus mounted-server is the
    /// `open_world_hint` distinction — whether the call could reach outside the
    /// session — so it carries a standing visual.
    pub server: Option<String>,
    /// The `<tool>` half, which for a built-in is the whole name.
    pub tool: String,
    /// The signature's arguments, positional first. Empty renders `check_inbox()`;
    /// `None` means the call renders bare (`bash`, or a call whose input has not
    /// started arriving).
    pub args: Option<Vec<SigArg>>,
    /// `bash`'s single-line command, rendered after the prompt unquoted, in place
    /// of the name and arguments. Set only when `args` is `None`.
    pub target: Option<String>,
    /// `bash`'s way of running; `None` for every other tool.
    pub shell: Option<Shell>,
    pub bodies: Vec<CallBody>,
}

/// The built-ins whose primary argument reads without its key: the one thing the
/// call acts on, which its name already implies — `read("a.rs", "b.rs")`, not
/// `read(paths: ["a.rs", "b.rs"])`. An array here spreads into one positional
/// argument per item. Keyed by wire name, so a mounted server's same-named tool keeps
/// its labels: only a built-in's argument shapes are known.
fn positional_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "read" => "paths",
        "search" => "pattern",
        "list" | "write" | "read_image" => "path",
        "edit" | "undo_edit" | "retarget_edit" | "find_anchor" => "file_path",
        "read_process" | "kill_process" | "watch_process" => "handle",
        _ => return None,
    })
}

/// Whether a value is content rather than an argument: a string with a line break
/// in it, anywhere inside the value. A line break is what a file's contents, a
/// heredoc, or a prose message have and an identifier never does.
fn is_content(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains('\n'),
        Value::Array(items) => items.iter().any(is_content),
        Value::Object(entries) => entries.values().any(is_content),
        _ => false,
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// A content value as the block below renders it: a string verbatim, with its line
/// breaks as line breaks; anything else pretty-printed.
pub(crate) fn body_of(key: &str, value: &Value, lang: Option<String>) -> CallBody {
    match value {
        Value::String(text) => CallBody {
            key: Some(key.to_string()),
            lang,
            text: text.clone(),
            prose: false,
        },
        other => CallBody {
            key: Some(key.to_string()),
            lang: Some("json".to_string()),
            text: pretty(other),
            prose: false,
        },
    }
}

/// Every call as a pseudocode signature — total, and correct for a tool this client
/// has never heard of, which matters because MCP servers are user-mountable and
/// their argument shapes are arbitrary.
///
/// A multi-line value is content: it leaves the signature and renders below it
/// under its key. Every other argument sits in the signature — whether one is too
/// long to sit there sensibly depends on the width, which is [`layout_signature`]'s
/// decision. Two cases stay special: `bash` renders its command bare, after a prompt
/// rather than its name, and the edit family emits no bodies, because the diff
/// rendered with its result is the representation there.
pub(crate) fn call_view(name: &str, input: &Value) -> CallView {
    let (server, tool) = super::split_tool_name(name);
    let mut view = CallView {
        intent: None,
        server: server.map(str::to_string),
        tool: tool.to_string(),
        args: Some(Vec::new()),
        target: None,
        shell: None,
        bodies: Vec::new(),
    };
    let Value::Object(object) = input else {
        // Not an object at all: nothing to name, so the value itself is the body.
        if !input.is_null() {
            view.bodies.push(CallBody {
                key: None,
                lang: Some("json".to_string()),
                text: pretty(input),
                prose: false,
            });
        }
        return view;
    };
    let str_arg = |key: &str| object.get(key).and_then(Value::as_str);
    view.intent = str_arg(crate::log::INTENT)
        .map(str::trim)
        .filter(|intent| !intent.is_empty())
        .map(str::to_string);

    // A hand-off's brief is a message to the session that takes over (docs/tools.md):
    // labelled with whom it is for and read as prose, so it is not mistaken for the
    // agent addressing the operator.
    if name == crate::protocol::agents::HAND_OFF_WIRE_NAME
        && let Some(brief) = str_arg("brief")
    {
        view.bodies.push(CallBody {
            key: Some("to the new session".to_string()),
            lang: None,
            text: brief.to_string(),
            prose: true,
        });
        return view;
    }

    if name == crate::bash::NAME
        && let Some(command) = str_arg("command")
    {
        view.args = None;
        view.shell = Some(Shell {
            background: object.get("background") == Some(&Value::Bool(true)),
            background_after: object.get("background_after").and_then(Value::as_u64),
        });
        if command.contains('\n') {
            view.bodies.push(CallBody {
                key: None,
                lang: Some("bash".to_string()),
                text: command.to_string(),
                prose: false,
            });
        } else {
            view.target = Some(command.to_string());
        }
        return view;
    }

    let lang = super::blob_lang(name, object);
    let primary = positional_key(name);
    let mut positional = Vec::new();
    let mut labelled = Vec::new();
    for (key, value) in object {
        // `intent` is already the call's title.
        if key == crate::log::INTENT {
            continue;
        }
        if is_content(value) {
            if !super::is_diff_tool(name) {
                view.bodies.push(body_of(key, value, lang.clone()));
            }
            continue;
        }
        if Some(key.as_str()) != primary {
            labelled.push(SigArg {
                key: key.clone(),
                label: Some(key.clone()),
                value: value.clone(),
            });
        } else if let Value::Array(items) = value
            && !items.is_empty()
            && items.iter().all(is_scalar)
        {
            positional.extend(items.iter().map(|item| SigArg {
                key: key.clone(),
                label: None,
                value: item.clone(),
            }));
        } else {
            positional.push(SigArg {
                key: key.clone(),
                label: None,
                value: value.clone(),
            });
        }
    }
    positional.extend(labelled);
    view.args = Some(positional);
    view
}

/// What colors a run of signature text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Server,
    Tool,
    Sep,
    Key,
    Value,
    /// `bash`'s command.
    Target,
    /// How a `bash` command runs (`background after 600s`).
    Tag,
}

/// One run of text in the signature, with the role that colors it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SigToken {
    pub kind: TokenKind,
    pub text: String,
}

/// One line of a broken signature, indented by `indent` columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SigRow {
    pub indent: usize,
    pub tokens: Vec<SigToken>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SigLayout {
    pub rows: Vec<SigRow>,
    /// A shell command: the head reads as a prompt, not a call.
    pub shell: bool,
    /// True when the signature broke across rows.
    pub broken: bool,
    /// The arguments too long to sit in the signature at this width. They are absent
    /// from `rows`, and render below it as content.
    pub pulled: Vec<SigArg>,
}

/// Per-level indent of a broken signature, in columns.
const INDENT: usize = 2;
/// How many whole lines one value may take in the signature before it is content
/// rather than an argument.
const PULL_LINES: usize = 3;
/// Below this many columns the pull threshold stops shrinking: on a narrow column a
/// path that wraps twice is still a path.
const MIN_PULL_COLUMNS: usize = 32;

fn token(kind: TokenKind, text: impl Into<String>) -> SigToken {
    SigToken {
        kind,
        text: text.into(),
    }
}

fn is_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn key_text(key: &str) -> String {
    if is_identifier(key) {
        key.to_string()
    } else {
        Value::String(key.to_string()).to_string()
    }
}

/// A value as it reads flat: strings quoted (their escapes are what keep an
/// embedded quote or comma unambiguous), other scalars bare, collections in the
/// signature's own `key: value` register.
pub(crate) fn flat_value(value: &Value) -> String {
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(flat_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(entries) if entries.is_empty() => "{}".to_string(),
        Value::Object(entries) => format!(
            "{{ {} }}",
            entries
                .iter()
                .map(|(k, v)| format!("{}: {}", key_text(k), flat_value(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        scalar => scalar.to_string(),
    }
}

/// Columns a string occupies, counted in chars. Wide characters (CJK, emoji) count
/// as one each, so a line holding them can overrun its budget; the renderer wraps
/// it as text rather than clipping.
fn width(text: &str) -> usize {
    text.chars().count()
}

fn flat_arg(arg: &SigArg) -> String {
    match &arg.label {
        None => flat_value(&arg.value),
        Some(label) => format!("{label}: {}", flat_value(&arg.value)),
    }
}

fn name_tokens(view: &CallView) -> Vec<SigToken> {
    let mut tokens = Vec::new();
    if let Some(server) = &view.server {
        tokens.push(token(TokenKind::Server, format!("{server}.")));
    }
    tokens.push(token(TokenKind::Tool, view.tool.clone()));
    tokens
}

fn arg_tokens(arg: &SigArg) -> Vec<SigToken> {
    let value = token(TokenKind::Value, flat_value(&arg.value));
    match &arg.label {
        None => vec![value],
        Some(label) => vec![
            token(TokenKind::Key, label.clone()),
            token(TokenKind::Sep, ": "),
            value,
        ],
    }
}

/// How a shell command runs, when it is not the default of blocking until it exits.
fn shell_tags(shell: Shell) -> Vec<String> {
    if shell.background {
        vec!["background".to_string()]
    } else if let Some(after) = shell.background_after {
        vec![format!("background after {after}s")]
    } else {
        Vec::new()
    }
}

/// The signature on one line of text, for surfaces with no room for rows and for
/// tests.
pub(crate) fn flat_signature(view: &CallView) -> String {
    let name = match &view.server {
        Some(server) => format!("{server}.{}", view.tool),
        None => view.tool.clone(),
    };
    if view.shell.is_some() {
        let command = view.target.clone().unwrap_or_else(|| {
            let first = view
                .bodies
                .first()
                .and_then(|b| b.text.lines().next())
                .unwrap_or("");
            format!("{first} …")
        });
        return format!("› {command}");
    }
    match &view.args {
        None => name,
        Some(args) => format!(
            "{name}({})",
            args.iter().map(flat_arg).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Lay the signature out in `columns`, or flat when the width is unknown (`None`).
/// A call that fits is one line; one that does not breaks after `(` with an argument
/// per line, and an argument whose value is a collection that still does not fit
/// breaks the same way inside. A single value longer than a few whole lines is not
/// an argument a reader scans, so it leaves the signature as content.
pub(crate) fn layout_signature(view: &CallView, columns: Option<usize>) -> SigLayout {
    let name = name_tokens(view);
    if let Some(shell) = view.shell {
        // bash: the command is verbatim and wraps as text; there is nothing to break
        // it at that would read better than its own spaces. A multi-line command is
        // a body below, so the head holds only its tags, if any.
        let mut tokens: Vec<SigToken> = view
            .target
            .iter()
            .map(|target| token(TokenKind::Target, target.clone()))
            .collect();
        tokens.extend(
            shell_tags(shell)
                .into_iter()
                .map(|t| token(TokenKind::Tag, t)),
        );
        return SigLayout {
            rows: if tokens.is_empty() {
                Vec::new()
            } else {
                vec![SigRow { indent: 0, tokens }]
            },
            shell: true,
            broken: false,
            pulled: Vec::new(),
        };
    }
    let Some(args) = &view.args else {
        // The bare name: a call whose input has not started arriving.
        return SigLayout {
            rows: vec![SigRow {
                indent: 0,
                tokens: name,
            }],
            shell: false,
            broken: false,
            pulled: Vec::new(),
        };
    };

    let limit = columns.map_or(usize::MAX, |c| {
        PULL_LINES * c.saturating_sub(INDENT).max(MIN_PULL_COLUMNS)
    });
    let (kept, pulled): (Vec<&SigArg>, Vec<&SigArg>) =
        args.iter().partition(|arg| width(&flat_arg(arg)) <= limit);

    let name_width: usize = name.iter().map(|t| width(&t.text)).sum();
    let flat_width = name_width
        + 2
        + kept.iter().map(|arg| width(&flat_arg(arg))).sum::<usize>()
        + 2 * kept.len().saturating_sub(1);
    // A lone scalar argument stays hugged by its parentheses: breaking around it
    // adds two rows and the value wraps as text either way.
    let hugged = kept.len() == 1 && is_scalar(&kept[0].value);
    let pulled: Vec<SigArg> = pulled.into_iter().cloned().collect();
    if columns.is_none_or(|c| flat_width <= c) || kept.is_empty() || hugged {
        let mut tokens = name;
        tokens.push(token(TokenKind::Sep, "("));
        for (i, arg) in kept.iter().enumerate() {
            if i > 0 {
                tokens.push(token(TokenKind::Sep, ", "));
            }
            tokens.extend(arg_tokens(arg));
        }
        tokens.push(token(TokenKind::Sep, ")"));
        return SigLayout {
            rows: vec![SigRow { indent: 0, tokens }],
            shell: false,
            broken: false,
            pulled,
        };
    }

    let columns = columns.unwrap_or(usize::MAX);
    let mut head = name;
    head.push(token(TokenKind::Sep, "("));
    let mut rows = vec![SigRow {
        indent: 0,
        tokens: head,
    }];
    for (i, arg) in kept.iter().enumerate() {
        let trailer = if i + 1 < kept.len() { "," } else { "" };
        rows.extend(arg_rows(arg, INDENT, columns, trailer));
    }
    rows.push(SigRow {
        indent: 0,
        tokens: vec![token(TokenKind::Sep, ")")],
    });
    SigLayout {
        rows,
        shell: false,
        broken: true,
        pulled,
    }
}

/// One argument of a broken signature: a row when it fits at its indent (or is a
/// scalar, which has nowhere to break but its own text), else its collection opened
/// across rows, each member laid out the same way.
fn arg_rows(arg: &SigArg, indent: usize, columns: usize, trailer: &str) -> Vec<SigRow> {
    let tail: Vec<SigToken> = if trailer.is_empty() {
        Vec::new()
    } else {
        vec![token(TokenKind::Sep, trailer)]
    };
    let fits = indent + width(&flat_arg(arg)) + trailer.len() <= columns;
    let members: Option<Vec<SigArg>> = match &arg.value {
        Value::Array(items) if !items.is_empty() => Some(
            items
                .iter()
                .map(|item| SigArg {
                    key: arg.key.clone(),
                    label: None,
                    value: item.clone(),
                })
                .collect(),
        ),
        Value::Object(entries) if !entries.is_empty() => Some(
            entries
                .iter()
                .map(|(k, v)| SigArg {
                    key: arg.key.clone(),
                    label: Some(key_text(k)),
                    value: v.clone(),
                })
                .collect(),
        ),
        _ => None,
    };
    let Some(members) = members.filter(|_| !fits) else {
        let mut tokens = arg_tokens(arg);
        tokens.extend(tail);
        return vec![SigRow { indent, tokens }];
    };
    let (open, close) = if arg.value.is_array() {
        ("[", "]")
    } else {
        ("{", "}")
    };
    let head = match &arg.label {
        None => vec![token(TokenKind::Sep, open)],
        Some(label) => vec![
            token(TokenKind::Key, label.clone()),
            token(TokenKind::Sep, ": "),
            token(TokenKind::Sep, open),
        ],
    };
    let mut rows = vec![SigRow {
        indent,
        tokens: head,
    }];
    let count = members.len();
    for (i, member) in members.iter().enumerate() {
        let trailer = if i + 1 < count { "," } else { "" };
        rows.extend(arg_rows(member, indent + INDENT, columns, trailer));
    }
    let mut close_tokens = vec![token(TokenKind::Sep, close)];
    close_tokens.extend(tail);
    rows.push(SigRow {
        indent,
        tokens: close_tokens,
    });
    rows
}

#[cfg(test)]
mod test;
