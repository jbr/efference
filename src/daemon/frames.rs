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
//!   human-wire render document (docs/tool-contract) is an annotation *of the frame*.
//!   Folding it into `data` would fork the two representations of one entry.
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
    /// Whether the loop is executing a turn.
    Running,
    /// The last non-success upstream response (docs/transport.md).
    UpstreamFailure,
}

impl Topic {
    /// Every topic, in `GET /state` key order. The one enumeration: the composite
    /// snapshot is built by folding this, and the level-triggered emitter diffs by
    /// walking it, so a topic added here reaches both surfaces or neither — which is
    /// what keeps `GET /state` equal to the fold of every state frame
    /// (docs/control-api.md).
    pub(crate) const ALL: [Topic; 10] = [
        Self::Gate,
        Self::Processes,
        Self::Usage,
        Self::Role,
        Self::Proposals,
        Self::Notifications,
        Self::Pace,
        Self::ReviewLevel,
        Self::Running,
        Self::UpstreamFailure,
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
            Self::Processes => "processes",
            Self::Usage => "usage",
            Self::Role => "role",
            Self::Proposals => "proposals",
            Self::Notifications => "notifications",
            Self::Pace => "pace",
            Self::ReviewLevel => "review_level",
            Self::Running => "running",
            Self::UpstreamFailure => "upstream_failure",
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
    /// The human-wire render document for a `tool_result` (docs/tool-contract).
    /// Beside `data`, never inside it — see the module docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) render: Option<Box<RawValue>>,
}

impl Frame {
    /// A durable log frame. The only constructor that sets `seq`, and the only one
    /// whose `data` is byte-identical to a `GET /log` element.
    pub(crate) fn log(entry: &LogEntry) -> Self {
        Self {
            class: Class::Log,
            kind: payload_kind(&entry.payload),
            seq: Some(entry.seq),
            data: raw(entry),
            render: None,
        }
    }

    /// Attach a render document to a log frame (docs/tool-contract). Separate from
    /// [`Frame::log`] because emission is a later unit; the field exists now so adding
    /// it doesn't reshape the envelope.
    pub(crate) fn with_render(mut self, render: &impl Serialize) -> Self {
        self.render = Some(raw(render));
        self
    }

    /// An ephemeral output frame. No `seq`: these are pre-commit and carry no cursor,
    /// which is the whole reason they are a distinct durability class.
    pub(crate) fn output(delta: &StreamDelta) -> Self {
        let (kind, data) = match delta {
            StreamDelta::AssistantText(text) => {
                ("assistant_delta", serde_json::json!({ "text": text }))
            }
            StreamDelta::ThinkingText(text) => {
                ("thinking_delta", serde_json::json!({ "text": text }))
            }
            StreamDelta::ToolCallStart { name } => {
                ("tool_call_start", serde_json::json!({ "name": name }))
            }
            StreamDelta::ToolCallInput { fragment } => (
                "tool_call_input",
                serde_json::json!({ "fragment": fragment }),
            ),
            StreamDelta::ToolOutput { call_id, chunk } => (
                "tool_output",
                serde_json::json!({ "call_id": call_id, "chunk": chunk }),
            ),
            StreamDelta::ProcessOutput { id, chunk } => (
                "process_output",
                serde_json::json!({ "id": id, "chunk": chunk }),
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
        Payload::Mark(_) => "mark",
        Payload::Triage(_) => "triage",
        Payload::Ambient(_) => "ambient",
        Payload::Spot(_) => "spot",
        Payload::ConfigEdit(_) => "config_edit",
        Payload::Preamble { .. } => "preamble",
    }
}

#[cfg(test)]
mod test;
