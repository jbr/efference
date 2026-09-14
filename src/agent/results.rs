//! Tool-result construction: the ok/err shapes, the size cap, and the payloads a withheld or aborted call logs (docs/tools.md).

use super::*;

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
    RawResult::error(format!(
        "tool `{}` returned {len} bytes, over the {MAX_TOOL_RESULT_BYTES}-byte limit \
         for a single tool result. This almost always means the call was too broad \
         (e.g. searching generated or log files). Narrow it — a tighter path, a more \
         specific pattern, or a line range — and try again. First 1KB of the output:\n\n{head}",
        call.name
    ))
}

/// Run a harness supervisor/inbox tool (docs/supervisor.md) against daemon state, or `None`
/// if `call` isn't one (so the caller falls through to the world toolbox). Like
/// `consolidate` these act on `Session`, not the world — but unlike it their result
/// distills through the universal fork like any tool (no bypass).
pub(super) async fn supervisor_tool_result(
    shared: &Arc<Session>,
    call: &ToolCall,
) -> Option<RawResult> {
    let handle = || call.input.get("handle").and_then(Value::as_str);
    match call.name.as_str() {
        supervisor::LIST => Some(RawResult::text(shared.bg_list_text())),
        supervisor::CHECK_INBOX => Some(RawResult::text(shared.inbox_summary())),
        supervisor::WAIT => Some(
            if !shared.has_pending_notifications()
                && !shared.has_running_background()
                && !shared.has_subscriptions()
            {
                RawResult::error(
                    "wait refused: nothing can wake this session — a pending notification, a running \
                 background process, or a session you are subscribed to would count.",
                )
            } else {
                // `inbox_wake` is the idle-triage signal: it arms before checking the queue,
                // then awaits without holding either the inbox or supervisor lock. Arm the
                // interrupt waiter before its latch check for the same lost-wakeup protection.
                let interrupt = shared.interrupt_signal();
                let interrupted = interrupt.notified();
                tokio::pin!(interrupted);
                if interrupted.as_mut().enable() || shared.interrupt_pending() {
                    RawResult::error("wait interrupted by user")
                } else {
                    tokio::select! {
                        _ = shared.inbox_wake() => RawResult::text(shared.inbox_summary()),
                        _ = &mut interrupted => RawResult::error("wait interrupted by user"),
                    }
                }
            },
        ),
        supervisor::READ => Some(match handle() {
            Some(h) => {
                match shared.bg_read_since(h, call.input.get("cursor").and_then(Value::as_u64)) {
                    Some(text) => RawResult::text(text),
                    None => RawResult::error(format!("no background process {h}")),
                }
            }
            None => RawResult::error("read_process requires a `handle`"),
        }),
        // `watch_process` registers and returns (docs/supervisor.md) — it never waits.
        // The hit arrives as an inbox notification, which is what wakes an idle agent;
        // the result here only confirms the registration.
        supervisor::WATCH => Some(
            match (handle(), call.input.get("pattern").and_then(Value::as_str)) {
                (Some(h), Some(pattern)) if !pattern.is_empty() => {
                    match shared.watch_bg(h, pattern.to_string()) {
                        Some(count) => RawResult::text(format!(
                            "watching {h} for `{pattern}` ({count} watch(es) pending on it). \
                             Only output printed from now on is matched. You'll be notified \
                             once, the first time a line contains it — end your turn rather \
                             than polling."
                        )),
                        None => RawResult::error(format!("no running background process {h}")),
                    }
                }
                (None, _) => RawResult::error("watch_process requires a `handle`"),
                _ => RawResult::error("watch_process requires a non-empty `pattern`"),
            },
        ),
        supervisor::KILL => Some(match handle() {
            Some(h) if shared.kill_bg(h) => RawResult::text(format!(
                "killed {h} (SIGKILL to its process group); its exit will be confirmed in your inbox."
            )),
            Some(h) => RawResult::error(format!("no running background process {h}")),
            None => RawResult::error("kill_process requires a `handle`"),
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
    Payload::tool_result(
        call.id.clone(),
        RawResult::error(format!(
            "not run — nothing is being asked of `{}` right now; continue with your task.",
            call.name
        )),
        WireState::Raw,
    )
}

/// A `tool_result` for a call naming no tool the turn advertised (docs/tools.md).
/// Built ahead of the permission cascade — a call that cannot run needs no
/// permission decision — and carried as a `Direct` payload, so the corrective text
/// reaches the wire unsummarized. `is_error`, because the call was not cogent.
pub(super) fn unknown_tool(call: &ToolCall, callable: &[String]) -> Payload {
    Payload::tool_result(
        call.id.clone(),
        RawResult::error(crate::tools::unknown_tool_message(&call.name, callable)),
        WireState::Raw,
    )
}

/// A `tool_result` standing in for a call that didn't run (aborted). Every
/// assistant `tool_use` needs exactly one paired result or projection orphans it
/// and the API rejects the request (docs/README.md).
pub(super) fn aborted(call: &ToolCall) -> Payload {
    Payload::tool_result(
        call.id.clone(),
        RawResult::error("aborted by user"),
        WireState::Raw,
    )
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
    Payload::tool_result(call.id.clone(), raw, WireState::Elided)
}

/// A `tool_result` for a call whose path expansion failed (docs/tools.md): the
/// variable was unlisted, undefined, or malformed, so the call never reached the
/// permission cascade or the tool. Pairs the orphaned `tool_use` like
/// [`aborted`]; the message names the variable and the allowed set so the model
/// can correct the path rather than theorize about the tool.
pub(super) fn expansion_error(call: &ToolCall, message: &str) -> Payload {
    Payload::tool_result(
        call.id.clone(),
        RawResult::error(format!("not run — {message}")),
        WireState::Raw,
    )
}

/// A `tool_result` for a call the user rejected in favor of a redirect message.
/// Like [`aborted`] it pairs the orphaned `tool_use`; the human's actual words
/// follow as a separate `Payload::User` entry, which `project` merges into this
/// result's user message as `[tool_result…, text]` (the same seam-drain merge the
/// mid-trajectory injection relies on, docs/control-channel.md) — so the
/// model reads this as context for the steer that follows rather than as tool output
/// (docs/README.md).
pub(super) fn rejected(call: &ToolCall) -> Payload {
    Payload::tool_result(
        call.id.clone(),
        RawResult::error(
            "Not run — the user replied instead of approving; see their next message.",
        ),
        WireState::Raw,
    )
}
