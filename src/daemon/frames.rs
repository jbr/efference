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
use crate::protocol::channel_value;
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
    /// (docs/triage.md); both call sites resolve it from the log. `session` signs the
    /// entry's artifact links (docs/artifacts.md).
    pub(crate) fn log(
        entry: &LogEntry,
        result_raw: Option<&RawResult>,
        session: Option<&Session>,
    ) -> Self {
        Self {
            class: Class::Log,
            kind: payload_kind(&entry.payload),
            seq: Some(entry.seq),
            data: raw(&EntryView::new(entry)
                .with_result_raw(result_raw)
                .with_artifacts(session)),
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
        // The delta serializes as `{"kind", "data"}`; the envelope names the kind itself
        // and carries the data, whose keys sort as every channel object's do.
        let data = channel_value(delta)["data"].take();
        Self {
            class: Class::Output,
            kind: delta.kind(),
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

/// The wire name of a log payload's variant — the `kind` of a [`Class::Log`] frame: the
/// variant's name in snake case, and a telemetry entry's
/// [`Telemetry::kind`](crate::log::Telemetry::kind). Derived on the log types from the
/// names (docs/log-and-projection.md), so a new variant brings its own.
pub(crate) fn payload_kind(payload: &Payload) -> &'static str {
    payload.into()
}

#[cfg(test)]
mod test;
