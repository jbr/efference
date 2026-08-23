//! Tool-result construction: the ok/err shapes, the size cap, and the payloads a withheld or aborted call logs (docs/tools.md).

use super::*;

pub(super) fn ok(content: String) -> RawResult {
    RawResult {
        images: Vec::new(),
        content,
        is_error: false,
    }
}

pub(super) fn err(content: String) -> RawResult {
    RawResult {
        images: Vec::new(),
        content,
        is_error: true,
    }
}

/// Enforce [`MAX_TOOL_RESULT_BYTES`]: replace an over-cap result with a hard error
/// (rather than a silent truncation) that names the byte counts and shows a short,
/// char-boundary-safe head, so both the model and the logs see what overflowed and the
/// model knows to narrow the call. Under the cap, the result passes through untouched.
pub(super) fn cap_tool_result(call: &ToolCall, result: RawResult) -> RawResult {
    let len = result.content.len();
    if len <= MAX_TOOL_RESULT_BYTES {
        return result;
    }
    log::warn!(
        "tool `{}` returned {len} bytes (> {MAX_TOOL_RESULT_BYTES} cap); \
         replacing with a hard error",
        call.name
    );
    let mut head = String::new();
    for ch in result.content.chars() {
        if head.len() + ch.len_utf8() > 1024 {
            break;
        }
        head.push(ch);
    }
    err(format!(
        "tool `{}` returned {len} bytes, over the {MAX_TOOL_RESULT_BYTES}-byte limit \
         for a single tool result. This almost always means the call was too broad \
         (e.g. searching generated or log files). Narrow it — a tighter path, a more \
         specific pattern, or a line range — and try again. First 1KB of the output:\n\n{head}",
        call.name
    ))
}

/// Run a harness supervisor/inbox tool (docs/supervisor.md) against daemon state, or `None`
/// if `call` isn't one (so the caller falls through to the world toolbox). Like
/// `consolidate` these act on `Shared`, not the world — but unlike it their result
/// distills through the universal fork like any tool (no bypass).
pub(super) fn supervisor_tool_result(shared: &Arc<Shared>, call: &ToolCall) -> Option<RawResult> {
    let handle = || call.input.get("handle").and_then(Value::as_str);
    match call.name.as_str() {
        supervisor::LIST => Some(ok(shared.bg_list_text())),
        supervisor::CHECK_INBOX => Some(ok(shared.inbox_summary())),
        supervisor::READ => Some(match handle() {
            Some(h) => {
                match shared.bg_read_since(h, call.input.get("cursor").and_then(Value::as_u64)) {
                    Some(text) => ok(text),
                    None => err(format!("no background process {h}")),
                }
            }
            None => err("read_process requires a `handle`".into()),
        }),
        supervisor::KILL => Some(match handle() {
            Some(h) if shared.kill_bg(h) => ok(format!(
                "killed {h} (SIGKILL to its process group); its exit will be confirmed in your inbox."
            )),
            Some(h) => err(format!("no running background process {h}")),
            None => err("kill_process requires a `handle`".into()),
        }),
        _ => None,
    }
}

/// The `command` of a background-bash call, if present.
pub(super) fn background_command(call: &ToolCall) -> Option<String> {
    call.input
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A `tool_result` for an un-asked peek-tool call (docs/redaction-fork.md): `retain`
/// and `triage` stand in every turn's `tools[]` for cache-lineage sharing, but on an
/// agent turn nothing is asked of them, so the call is paired with this nudge instead
/// of entering the cascade. One call per turn means the cost is a wasted round-trip,
/// not corruption. The stable "nothing is being asked" phrase is the compliance
/// breadcrumb — the rate of un-asked calls is a grep over the logged raw results.
pub(super) fn unasked(call: &ToolCall) -> Payload {
    Payload::ToolResult {
        call_id: call.id.clone(),
        raw: RawResult {
            images: Vec::new(),
            content: format!(
                "not run — nothing is being asked of `{}` right now; continue with your task.",
                call.name
            ),
            is_error: true,
        },
        wire: WireState::Raw,
    }
}

/// A `tool_result` standing in for a call that didn't run (aborted). Every
/// assistant `tool_use` needs exactly one paired result or projection orphans it
/// and the API rejects the request (docs/README.md).
pub(super) fn aborted(call: &ToolCall) -> Payload {
    Payload::ToolResult {
        call_id: call.id.clone(),
        raw: RawResult {
            images: Vec::new(),
            content: "aborted by user".into(),
            is_error: true,
        },
        wire: WireState::Raw,
    }
}

/// A `tool_result` for a call the operator interrupted while it ran
/// (docs/control-channel.md). Nothing here can suspend a synchronous tool, so the
/// call completed and its effects stand — what the interrupt buys is that its
/// *output* never reaches the model: `raw` is kept in the log (the TUI can still
/// show it, and the verdict is reversible like any other), while the wire carries
/// only the `Elided` placeholder. The pair has to exist at all because an
/// assistant `tool_use` with no `tool_result` is a request the API rejects
/// (docs/log-and-projection.md).
///
/// It deliberately explains nothing. The operator interrupted in order to say
/// something, and their message is the next thing on the wire; a harness-authored
/// gloss here would guess at a meaning that differs per tool and land *before*
/// the person whose intent it was guessing at.
pub(super) fn withheld(call: &ToolCall, raw: RawResult) -> Payload {
    Payload::ToolResult {
        call_id: call.id.clone(),
        raw,
        wire: WireState::Elided,
    }
}

/// A `tool_result` for a call whose path expansion failed (docs/tools.md): the
/// variable was unlisted, undefined, or malformed, so the call never reached the
/// permission cascade or the tool. Pairs the orphaned `tool_use` like
/// [`aborted`]; the message names the variable and the allowed set so the model
/// can correct the path rather than theorize about the tool.
pub(super) fn expansion_error(call: &ToolCall, message: &str) -> Payload {
    Payload::ToolResult {
        call_id: call.id.clone(),
        raw: RawResult {
            images: Vec::new(),
            content: format!("not run — {message}"),
            is_error: true,
        },
        wire: WireState::Raw,
    }
}

/// A `tool_result` for a call the user rejected in favor of a redirect message.
/// Like [`aborted`] it pairs the orphaned `tool_use`; the human's actual words
/// follow as a separate `Payload::User` entry, which `project` merges into this
/// result's user message as `[tool_result…, text]` (the same seam-drain merge the
/// mid-trajectory injection relies on, docs/control-channel.md) — so the
/// model reads this as context for the steer that follows rather than as tool output
/// (docs/README.md).
pub(super) fn rejected(call: &ToolCall) -> Payload {
    Payload::ToolResult {
        call_id: call.id.clone(),
        raw: RawResult {
            images: Vec::new(),
            content: "Not run — the user replied instead of approving; see their next message."
                .into(),
            is_error: true,
        },
        wire: WireState::Raw,
    }
}
