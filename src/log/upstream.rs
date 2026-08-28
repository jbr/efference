//! The record-only mark an assistant entry carries when the upstream stream did
//! not produce a usable turn, and the transport evidence captured with it
//! (docs/transport.md).
//!
//! The mark is one string in the ordinary case. One case carries
//! [`StreamDiagnostics`] as well — a stream that *finalizes* with no content and
//! no usage, whose generation id 404s afterward, so the log is the only place a
//! later investigation can read from.
//!
//! None of this reaches the wire: an entry with an `upstream_failure` projects
//! to nothing (docs/log-and-projection.md), so the shape here cannot move the
//! cached prefix.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(test)]
mod test;

/// Why an assistant entry is record-only, plus the transport evidence when the
/// path that produced it captured any.
///
/// Serializes as a **bare string** when there are no diagnostics, which is the
/// shape every log before this type used, and deserializes from either shape —
/// so an old log line parses and an ordinary failure writes the same bytes it
/// always did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamFailureRecord {
    /// The provider's error verbatim when one arrived, or a harness-written note.
    pub why: String,
    /// Present only on the empty-finalized-stream path (docs/transport.md).
    pub stream: Option<StreamDiagnostics>,
}

impl UpstreamFailureRecord {
    /// The prose half — what every consumer that renders or forwards the failure
    /// reads.
    pub fn why(&self) -> &str {
        &self.why
    }
}

impl From<String> for UpstreamFailureRecord {
    fn from(why: String) -> Self {
        Self { why, stream: None }
    }
}

impl From<&str> for UpstreamFailureRecord {
    fn from(why: &str) -> Self {
        Self::from(why.to_string())
    }
}

impl std::fmt::Display for UpstreamFailureRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

/// The structured spelling, written only when diagnostics are attached.
#[derive(Serialize)]
struct StructuredRef<'a> {
    why: &'a str,
    stream: &'a StreamDiagnostics,
}

impl Serialize for UpstreamFailureRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.stream {
            None => serializer.serialize_str(&self.why),
            Some(stream) => StructuredRef {
                why: &self.why,
                stream,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for UpstreamFailureRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Structured {
            why: String,
            #[serde(default)]
            stream: Option<StreamDiagnostics>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            /// Every log written before diagnostics existed, and every failure
            /// that carries none.
            Text(String),
            Structured(Structured),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Text(why) => Self { why, stream: None },
            Repr::Structured(Structured { why, stream }) => Self { why, stream },
        })
    }
}

/// What the transport saw on the response that produced an empty finalized
/// stream (docs/transport.md).
///
/// Every field is optional-ish on purpose: this is evidence, not a contract, and
/// a missing piece must not cost the pieces that are present. Values pass the
/// log's `[env]` masker like any other stored text (docs/env.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamDiagnostics {
    /// The HTTP status the stream arrived on — 200 on this path, recorded rather
    /// than assumed.
    pub status: u16,
    /// Response headers, restricted to [`is_captured_header`]. A response header
    /// can carry account-scoped values, so the log stores a named set rather than
    /// whatever the edge attached (docs/env.md).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// The `message_start` event's `data` payload as received, when one arrived.
    /// Kept whole rather than reduced to parsed fields: OpenRouter's Messages
    /// skin carries a top-level `provider` in the `message` object naming the
    /// serving leg (docs/transport.md), and keeping the body whole means any
    /// other unmodeled field survives without a schema change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_start: Option<String>,
    /// The stream re-rendered from its decoded events, one `event:` / `data:`
    /// pair per frame in arrival order. Not the socket's bytes: the SSE decoder
    /// hands over decoded events, so comments and line folding are already gone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub frames: String,
    /// Whether [`FRAME_CAPTURE_CAP`] stopped the capture — `frames` then holds
    /// the head of the stream and nothing after it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub frames_truncated: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// How many bytes of decoded frames one stream may accumulate.
///
/// The path this capture serves is the small-stream path by definition — an
/// empty stream is a `message_start` and a terminator — so the cap is a guard on
/// the invariant rather than a budget anyone expects to spend. Every stream
/// buffers up to it (the emptiness verdict is only available at `finish`), so it
/// also bounds what a live turn holds: the head is kept and the rest dropped.
pub const FRAME_CAPTURE_CAP: usize = 8 * 1024;

/// Response headers retained on the empty-stream path.
///
/// An allowlist, not a filter list: the set is the join keys and the
/// content/routing facts an after-the-fact query needs — OpenRouter's
/// `x-generation-id` above all, which is the id every `GET /generation` lookup
/// is keyed by. Anything not named here never enters the log, so an edge that
/// starts attaching an account-scoped header does not silently start recording
/// it (docs/env.md).
pub const CAPTURED_HEADERS: &[&str] = &[
    "cf-ray",
    "content-type",
    "date",
    "request-id",
    "server",
    "x-generation-id",
    "x-openrouter-cache-status",
    "x-request-id",
];

/// Whether a response header is one of the [`CAPTURED_HEADERS`]. Case-insensitive:
/// HTTP/1 preserves whatever case the edge sent.
pub fn is_captured_header(name: &str) -> bool {
    CAPTURED_HEADERS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(name))
}
