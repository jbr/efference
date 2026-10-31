//! Tool-result construction: the ok/err shapes, the size cap, and the payloads a withheld or aborted call logs (docs/tools.md).

use super::*;
use crate::log::Interruption;
use crate::names::tools::{
    CHECK_INBOX, KILL_PROCESS, LIST_PROCESSES, READ_PROCESS, WAIT, WATCH_PROCESS,
};

/// Enforce [`MAX_TOOL_RESULT_BYTES`]: replace an over-cap result with a hard error
/// (rather than a silent truncation) that names the byte counts and shows a short,
/// char-boundary-safe head, so both the model and the logs see what overflowed and the
/// model knows to narrow the call. Under the cap, the result passes through untouched.
pub(super) fn cap_tool_result(shared: &Session, call: &ToolCall, result: RawResult) -> RawResult {
    let len = result.content.len();
    if len <= MAX_TOOL_RESULT_BYTES {
        return result;
    }
    shared.warn(format_args!(
        "tool `{}` returned {len} bytes (> {MAX_TOOL_RESULT_BYTES} cap); \
         replacing with a hard error",
        call.name
    ));
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

/// Run a harness process-monitor/inbox tool (docs/process-monitor.md) against daemon state, or `None`
/// if `call` isn't one (so the caller falls through to the world toolbox). Like
/// `consolidate` these act on `Session`, not the world — but unlike it their result
/// triages through the universal fork like any tool (no bypass).
pub(super) async fn process_monitor_tool_result(
    shared: &Arc<Session>,
    call: &ToolCall,
) -> Option<RawResult> {
    let handle = || call.input.get("handle").and_then(Value::as_str);
    match call.name.as_str() {
        LIST_PROCESSES => Some(RawResult::text(shared.bg_list_text())),
        CHECK_INBOX => Some(RawResult::text(shared.inbox_listing())),
        WAIT => Some(
            // A turn queued while this step was generating is already a reason to stop:
            // checked ahead of the refusal, which counts only sources that could arrive.
            if shared.turn_to_run() {
                RawResult::text(TURN_ARRIVED)
            } else if !shared.has_pending_notifications()
                && !shared.has_running_background()
                && !shared.has_subscriptions()
            {
                RawResult::error(
                    "wait refused: nothing can wake this session — a pending notification, a running \
                 background process, or a session you are subscribed to would count.",
                )
            } else {
                // `inbox_wake` is the idle-triage signal: it arms before checking the queue,
                // then awaits without holding either the inbox or process monitor lock. Arm the
                // interrupt waiter before its latch check for the same lost-wakeup protection.
                let interrupt = shared.interrupt_signal();
                let interrupted = interrupt.notified();
                tokio::pin!(interrupted);
                if interrupted.as_mut().enable() || shared.interrupt_pending() {
                    RawResult::error("wait interrupted by user")
                } else {
                    tokio::select! {
                        _ = shared.inbox_wake() => RawResult::text(shared.inbox_listing()),
                        _ = shared.turn_wake() => RawResult::text(TURN_ARRIVED),
                        _ = &mut interrupted => RawResult::error("wait interrupted by user"),
                    }
                }
            },
        ),
        READ_PROCESS => Some(match handle() {
            Some(h) => {
                match shared.bg_read_since(h, call.input.get("cursor").and_then(Value::as_u64)) {
                    Some(text) => RawResult::text(text),
                    None => RawResult::error(format!("no background process {h}")),
                }
            }
            None => RawResult::error("read_process requires a `handle`"),
        }),
        // `watch_process` registers and returns (docs/process-monitor.md) — it never waits.
        // The hit arrives as an inbox notification, which is what wakes an idle agent;
        // the result here only confirms the registration.
        WATCH_PROCESS => Some(
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
        KILL_PROCESS => Some(match handle() {
            Some(h) if shared.kill_bg(h) => RawResult::text(format!(
                "killed {h} (SIGKILL to its process group); its exit will be confirmed in your inbox."
            )),
            Some(h) => RawResult::error(format!("no running background process {h}")),
            None => RawResult::error("kill_process requires a `handle`"),
        }),
        _ => None,
    }
}

/// `wait`'s result when a queued turn ends it (docs/process-monitor.md). Fixed text: it
/// projects to the wire, and the turn itself follows it at the next juncture.
pub(super) const TURN_ARRIVED: &str = "wait ended: a message arrived, and follows this result.";

/// The `command` of a background-bash call, if present.
pub(super) fn background_command(call: &ToolCall) -> Option<String> {
    call.input
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A `tool_result` for an un-asked triage-tool call (docs/triage.md): `retain`
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

/// A `tool_result` for a call interrupted while it ran (docs/control-channel.md). A
/// bash command's process group is killed; a synchronous tool cannot be suspended, so
/// it completed and its effects stand. Either way its *output* never reaches the
/// model: `raw` is kept in the log (the TUI can still show it), and the wire carries a
/// line saying who interrupted, after how long, and what became of the call
/// ([`WireState::Interrupted`]). The pair has to exist at all because an assistant
/// `tool_use` with no `tool_result` is a request the API rejects
/// (docs/log-and-projection.md).
///
/// The line states facts rather than guessing at why. Whoever interrupted may say why
/// next, and often does not: a supervising agent or a driver stops a runaway command
/// and moves on, and the model must still be able to tell its call was stopped.
pub(super) fn withheld(call: &ToolCall, raw: RawResult, interruption: Interruption) -> Payload {
    Payload::tool_result(call.id.clone(), raw, WireState::Interrupted(interruption))
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

/// A `tool_result` for a call its decider rejected in favor of a redirect message.
/// Like [`aborted`] it pairs the orphaned `tool_use`; the decider's words follow as a
/// separate `Payload::User` entry, which `project` merges into this result's user
/// message as `[tool_result…, text]` (the same merge a queued turn delivered mid-task
/// relies on, docs/control-channel.md) — so the model reads this as context for the
/// steer that follows rather than as tool output (docs/README.md).
///
/// It names who decided (docs/permissions.md): the operator as "the user", as it always
/// has; another agent or client by the label its own messages open with
/// (`names::sources::wire_label`); the loop's own permission review as itself.
pub(super) fn rejected(call: &ToolCall, decider: Option<&str>) -> Payload {
    let text = match decider {
        None | Some("web") => {
            "Not run — the user replied instead of approving; see their next message.".to_string()
        }
        Some(review) if crate::names::sources::is_review_source(review) => {
            "Not run — the permission review declined it; see the next message.".to_string()
        }
        Some(source) => format!(
            "Not run — {} replied instead of approving; see the next message.",
            crate::names::sources::wire_label(source)
        ),
    };
    Payload::tool_result(call.id.clone(), RawResult::error(text), WireState::Raw)
}
