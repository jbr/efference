//! The control-stream frame envelope (docs/control-api.md).
//!
//! One discriminated shape for every frame a client can receive, so a client writes
//! one parser rather than one per stream. Three **durability classes** — not three
//! topics — because durability is the axis a client actually branches on:
//!
//! | class | durability | resume |
//! | --- | --- | --- |
//! | [`Class::Log`] | durable, monotonic `seq` | replay by `?since`, never drops |
//! | [`Class::Output`] | ephemeral, lossy | none — live tail |
//! | [`Class::State`] | level-triggered | snapshot-on-subscribe, then replacement |
//!
//! Two invariants are enforced by construction rather than documented and hoped for:
//!
//! - **Only `log` frames carry `seq`.** The constructors are the only way to build a
//!   [`Frame`], and only [`Frame::log`] sets it. Over SSE `seq` is also the `id:`
//!   field, so `EventSource`'s Last-Event-ID resume tracks the durable subset and
//!   ignores the ephemeral ones; a `seq` on an ephemeral frame would corrupt that
//!   cursor.
//! - **`render` rides beside `data`, never inside it.** A log frame's `data` is
//!   byte-identical to the corresponding `GET /log` element (see `sse.rs`), so the
//!   human-wire render document is an annotation *of the frame*.
//!   Folding it into `data` would fork the two representations of one entry.
//!
//! The frame ≡ window byte-identity holds **below the result-size threshold**, which
//! is the whole of it in every ordinary session. Above it the two agree with each
//! other and diverge from `GET /log/:seq`: both carry a tagged prefix of an oversized
//! result's `content`, while the entry route serves it whole
//! (`MAX_INLINE_RESULT_BYTES`, entry_view.rs). That divergence is the feature — a
//! multi-megabyte result would otherwise queue every later frame behind it and re-ship
//! on every replay and window read — and it is deliberately narrow: one field of one
//! payload, tagged where it happens, dereferenced at the address images already use.
//!
//! All three classes nest their payload under `data` — uniformly, including `output`,
//! whose fields would otherwise sit at the top level and force a client to special-case
//! one class in the parser it was supposed to write once.

use super::*;
use serde_json::value::{RawValue, to_raw_value};

/// Serialize to **raw** JSON, preserving the type's own field order.
///
/// `data` cannot be a [`Value`]: `serde_json::to_value` produces a `BTreeMap`-backed
/// object, so round-tripping a [`LogEntry`] through it re-emits the fields in
/// *alphabetical* order (`at, payload, seq, turn`) rather than declaration order
/// (`seq, turn, at, payload`) — silently breaking the byte-identity between a log
/// frame's `data` and the corresponding `GET /log` element. The same `Value`
/// key-sorting that makes tool-call inputs cache-stable (docs/log-and-projection.md)
/// destroys the property here, so the envelope holds bytes, not a parsed tree.
fn raw(value: &impl Serialize) -> Box<RawValue> {
    to_raw_value(value).unwrap_or_else(|_| RawValue::from_string("null".into()).expect("valid"))
}

/// The durability class of a frame — the axis a client branches on when deciding
/// whether a gap is recoverable.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Class {
    /// A committed [`LogEntry`]. Durable and monotonic; replayable by cursor.
    Log,
    /// Pre-commit output ([`StreamDelta`]). Lossy; the committed entry on the `log`
    /// class is the authoritative copy.
    Output,
    /// Ephemeral control state. Level-triggered: the whole topic's current value,
    /// `null` when cleared.
    State,
}

/// A level-triggered state topic (docs/control-api.md). The set is lifted from the
/// units `src/tui/run.rs` already fingerprints out of the `GET /state` blob and emits
/// separately — that poller is the topic decomposition, arrived at by use — with
/// `role`/`proposals` split (the TUI merges them only for its own convenience) and the
/// loop dials named for the fields of the loop resource.
///
/// A `state` frame carries the topic's **entire** value, never a patch: replacement is
/// idempotent and order-insensitive, which is what makes `GET /state` equal to the fold
/// of every state frame since connect.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Topic {
    /// The pending approval gate, or `null` when none is open.
    Gate,
    /// The question an `ask` has put to the operator, or `null` when none is pending:
    /// `{call_id, question, n, of, recalled}` (docs/supervisor.md). `n` of `of` counts
    /// the asks of one completion, which are put one at a time; `recalled` holds the
    /// turns withdrawn when the ask began, in the shape `DELETE /turns/:id` returns.
    Ask,
    /// Live background processes (docs/supervisor.md).
    Processes,
    /// Token/cost telemetry (docs/transport.md).
    Usage,
    /// The effective policy in force (docs/permissions.md).
    Role,
    /// Config-edit proposals awaiting an orchestrator decision.
    Proposals,
    /// Pending ambient notifications (docs/inbox.md).
    Notifications,
    /// The operator's pace dial.
    Pace,
    /// The review-level dial (docs/permissions.md).
    ReviewLevel,
    /// The review position **in force**, which is the dial's own value except that
    /// `upstream` with no live upstream reviews as `spotter_human` (docs/permissions.md).
    /// Carried beside the dial rather than folded into it because the two answer
    /// different questions — what was set, and who answers a gate open right now —
    /// and because this one moves without anyone turning the dial.
    ResolvedReviewLevel,
    /// Whether the loop is executing a turn.
    Running,
    /// Turns submitted but not yet dequeued, in submission order — `[{submission_id, text}]`
    /// (docs/control-channel.md). The clients' optimistic pending echo keys off it, and it is
    /// what makes the queue addressable for a withdraw.
    Queued,
    /// A trajectory that gave up rather than finished (docs/transport.md).
    /// `null` when the last one finished. Beside [`Running`](Self::Running)
    /// because it is what distinguishes the two idle conditions that flag
    /// cannot: "idle, done" from "idle, gave up".
    Stall,
    /// The last non-success upstream response (docs/transport.md).
    UpstreamFailure,
    /// A retry in flight — which surface, which attempt, of what budget
    /// (docs/transport.md). `null` when nothing is being retried.
    UpstreamRetry,
    /// The session's prompt cache is believed to have expired (backlog:
    /// cold-session.md): `{since, context_tokens}`, `null` while it is believed warm,
    /// before the first request, and on a model with no cache.
    Cold,
    /// The loop is stopped at its cold check mid-turn (docs/control-api.md):
    /// `{reason: "expired", since, context_tokens}`, `{reason: "folded"}`, or `null`.
    /// Separate from [`Cold`](Self::Cold), which is `null` after a fold though the
    /// loop stays parked until the next turn or continue.
    Park,
    /// A fold in progress, or the last one's failure (docs/control-api.md): `{state:
    /// "writing"}`, `{state: "failed", error}`, or `null`. Separate from
    /// [`Cold`](Self::Cold) because a fold can be asked of a warm session.
    Fold,
    /// The config file on disk does not parse and could not be restored, so the
    /// loop is parked until a human repairs it (docs/permissions.md). `null`
    /// whenever the file parses — which is nearly always, since a bad edit is
    /// normally reverted rather than left on disk.
    ConfigRepair,
    /// What this session *is* — the configured model and its working directory
    /// (docs/control-api.md). Static for the session today; carried as a topic so a
    /// client reads it from the connect snapshot like everything else, and so a
    /// value that ever moves reaches clients without a new mechanism.
    SessionIdentity,
    /// What the loop is waiting on right now (docs/control-api.md): the upstream
    /// requests open, each with its origin, stage, and the instant that stage began,
    /// and the tool calls executing. `null` when nothing is open. Parked waits keep
    /// their own topics.
    CurrentOperations,
}

impl Topic {
    /// Every topic, in `GET /state` key order. The one enumeration: the composite
    /// snapshot is built by folding this, and the level-triggered emitter diffs by
    /// walking it, so a topic added here reaches both surfaces or neither — which is
    /// what keeps `GET /state` equal to the fold of every state frame
    /// (docs/control-api.md).
    pub(crate) const ALL: [Topic; 21] = [
        Self::Gate,
        Self::Ask,
        Self::Processes,
        Self::Usage,
        Self::Role,
        Self::Proposals,
        Self::Notifications,
        Self::Pace,
        Self::ReviewLevel,
        Self::ResolvedReviewLevel,
        Self::Running,
        Self::Queued,
        Self::Stall,
        Self::UpstreamFailure,
        Self::UpstreamRetry,
        Self::Cold,
        Self::Park,
        Self::Fold,
        Self::ConfigRepair,
        Self::SessionIdentity,
        Self::CurrentOperations,
    ];

    /// The topic named by a `?include=state:<topic>` token, or `None` if there is no such
    /// topic. The inverse of [`Topic::as_str`], and the reason that method exists: the
    /// grammar is parsed against the same names the frames carry.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == name)
    }

    /// The wire name, for `?include=state:<topic>` parsing and the SSE `event:` field.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Gate => "gate",
            Self::Ask => "ask",
            Self::Processes => "processes",
            Self::Usage => "usage",
            Self::Role => "role",
            Self::Proposals => "proposals",
            Self::Notifications => "notifications",
            Self::Pace => "pace",
            Self::ReviewLevel => "review_level",
            Self::ResolvedReviewLevel => "resolved_review_level",
            Self::Running => "running",
            Self::Queued => "queued",
            Self::Stall => "stall",
            Self::UpstreamFailure => "upstream_failure",
            Self::UpstreamRetry => "upstream_retry",
            Self::Cold => "cold",
            Self::Park => "park",
            Self::Fold => "fold",
            Self::ConfigRepair => "config_repair",
            Self::SessionIdentity => "session_identity",
            Self::CurrentOperations => "current_operations",
        }
    }
}

/// One frame of the unified control stream. Built only through the per-class
/// constructors, which is what keeps `seq` exclusive to [`Class::Log`].
#[derive(Serialize, Debug)]
pub(crate) struct Frame {
    pub(crate) class: Class,
    /// The payload kind within the class — the log payload's variant, the delta's
    /// variant, or the state topic. Doubles as the SSE `event:` field so a client can
    /// switch without parsing the body first.
    pub(crate) kind: &'static str,
    /// The durable cursor. `Some` on [`Class::Log`] frames and nowhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seq: Option<u64>,
    /// The payload, carried as raw JSON so a log frame reproduces its `GET /log`
    /// element byte for byte (see [`raw`]). `null` on a `state` frame means the topic
    /// is cleared.
    pub(crate) data: Box<RawValue>,
    /// The human-wire render document for a `tool_result`. Beside `data`, never
    /// inside it — see the module docs. Nothing emits one yet; the field is here so
    /// that adding emission does not reshape the envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) render: Option<Box<RawValue>>,
}

impl Frame {
    /// A durable log frame. The only constructor that sets `seq`, and the only one
    /// whose `data` is byte-identical to a `GET /log` element — both serialize the
    /// entry's [`EntryView::new`] (image `data` omitted, oversized result text
    /// truncated and tagged; entry_view.rs). `result_raw` is the bytes of the result
    /// a [`Payload::ResultVerdict`] names, which that entry does not carry itself
    /// (docs/redaction-fork.md); both call sites resolve it from the log.
    pub(crate) fn log(entry: &LogEntry, result_raw: Option<&RawResult>) -> Self {
        Self {
            class: Class::Log,
            kind: payload_kind(&entry.payload),
            seq: Some(entry.seq),
            data: raw(&EntryView::new(entry).with_result_raw(result_raw)),
            render: None,
        }
    }

    /// Attach a render document to a log frame — separate from [`Frame::log`], which
    /// never sets one. Unreached (see [`Frame::render`]).
    pub(crate) fn with_render(mut self, render: &impl Serialize) -> Self {
        self.render = Some(raw(render));
        self
    }

    /// An ephemeral output frame. No `seq`: these are pre-commit and carry no cursor,
    /// which is the whole reason they are a distinct durability class.
    pub(crate) fn output(delta: &StreamDelta) -> Self {
        let (kind, data) = match delta {
            StreamDelta::AssistantText { index, text } => (
                "assistant_delta",
                serde_json::json!({ "index": index, "text": text }),
            ),
            StreamDelta::ThinkingText { index, text } => (
                "thinking_delta",
                serde_json::json!({ "index": index, "text": text }),
            ),
            StreamDelta::ToolCallStart {
                index,
                call_id,
                name,
            } => (
                "tool_call_start",
                serde_json::json!({ "index": index, "call_id": call_id, "name": name }),
            ),
            StreamDelta::ToolCallInput { index, fragment } => (
                "tool_call_input",
                serde_json::json!({ "index": index, "fragment": fragment }),
            ),
            StreamDelta::ToolOutput { call_id, chunk } => (
                "tool_output",
                serde_json::json!({ "call_id": call_id, "chunk": chunk }),
            ),
            StreamDelta::ProcessOutput { id, chunk } => (
                "process_output",
                serde_json::json!({ "id": id, "chunk": chunk }),
            ),
            StreamDelta::NarratorText { text } => {
                ("narrator_delta", serde_json::json!({ "text": text }))
            }
            StreamDelta::NarratorThinking { text } => (
                "narrator_thinking_delta",
                serde_json::json!({ "text": text }),
            ),
        };
        Self {
            class: Class::Output,
            kind,
            seq: None,
            data: raw(&data),
            render: None,
        }
    }

    /// A level-triggered state frame carrying a topic's entire current value.
    /// `Value::Null` is meaningful — it means the topic is cleared (no gate pending,
    /// say), not that the frame is empty.
    pub(crate) fn state(topic: Topic, data: &impl Serialize) -> Self {
        Self {
            class: Class::State,
            kind: topic.as_str(),
            seq: None,
            data: raw(data),
            render: None,
        }
    }

    /// A state frame whose value is **already serialized** — the form the emitter uses,
    /// since the one differ serializes each topic once and every connection sends those
    /// same bytes. Re-parsing to a [`Value`] and back would sort the keys, which is the
    /// hazard [`raw`] exists to avoid.
    pub(crate) fn state_raw(topic: Topic, json: &str) -> Self {
        Self {
            class: Class::State,
            kind: topic.as_str(),
            seq: None,
            data: RawValue::from_string(json.to_string())
                .unwrap_or_else(|_| RawValue::from_string("null".into()).expect("valid")),
            render: None,
        }
    }

    /// The serialized frame body, as sent in an SSE `data:` field.
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The wire name of a log payload's variant — the `kind` of a [`Class::Log`] frame.
pub(crate) fn payload_kind(payload: &Payload) -> &'static str {
    match payload {
        Payload::User(_) => "user",
        Payload::Assistant { .. } => "assistant",
        Payload::ToolResult { .. } => "tool_result",
        Payload::ResultVerdict { .. } => "result_verdict",
        Payload::Mark(_) => "mark",
        Payload::Triage(_) => "triage",
        Payload::Ambient(_) => "ambient",
        Payload::Spot(_) => "spot",
        Payload::Telemetry(telemetry) => telemetry.kind(),
        Payload::Resumed(_) => "resumed",
        Payload::ProcessEvent(_) => "process_event",
        Payload::Worktree(_) => "worktree",
        Payload::ImagesRefused { .. } => "images_refused",
        Payload::ConfigEdit(_) => "config_edit",
        Payload::Preamble { .. } => "preamble",
        Payload::Title(_) => "title",
    }
}

#[cfg(test)]
mod test;
