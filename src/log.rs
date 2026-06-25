//! The append-only annotated log — the source of truth (DESIGN §1, §2).
//!
//! Phase 0 minimal slice: only the `User` / `Assistant` / `ToolResult` payloads
//! and the [`WireState`] seam needed to drive the bash-only loop. `Mark`,
//! `Ambient`, and `Commit` payloads (and the model-driven distiller) arrive with
//! the phases that exercise them — see DESIGN §7.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Monotonic per-entry sequence number; the address space [`project`] reads over.
///
/// [`project`]: crate::project::project
pub type Seq = u64;

/// Which agent turn produced an entry. Metadata only — never projected.
pub type Turn = u32;

/// Wall-clock millis since the Unix epoch. **Log metadata only**: timestamps are
/// never projected onto the wire, so their non-determinism cannot affect cache
/// stability (DESIGN §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp(pub u64);

/// An Anthropic tool-use id (`toolu_…`) and its matching `tool_result` id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub String);

/// One immutable, ordered record in the log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: Seq,
    pub turn: Turn,
    pub at: Timestamp,
    pub payload: Payload,
}

/// The content of a log entry. Immutable once appended; redaction never mutates
/// a payload's `raw`, only flips a [`WireState`] (DESIGN §1).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Payload {
    /// Prose (and, later, richer blocks) authored by the user / operator.
    User(Vec<ContentBlock>),
    /// A model turn: prose plus zero or more tool calls.
    Assistant {
        text: Vec<ContentBlock>,
        calls: Vec<ToolCall>,
    },
    /// The result of running one tool call. `raw` is immutable; redaction only
    /// flips `wire`.
    ToolResult {
        call_id: CallId,
        raw: RawResult,
        wire: WireState,
    },
    /// A salience annotation written at the moment it's known (DESIGN §1, §2): a
    /// decision, a dead end, a relevance note, or a consolidation boundary. The
    /// Phase-3 substrate for intent-aware compaction — *kept* through a fold while
    /// the deliberation around it is elided. Wired into [`project`] ahead of the
    /// feature: today it projects to nothing; Slice 1 teaches projection to fold
    /// the span behind a boundary mark (DESIGN §10 condensed map → Phase 3).
    ///
    /// [`project`]: crate::project::project
    Mark(Mark),
}

/// Phase 0 content is text-only; richer blocks (images, …) come later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
}

/// A tool invocation emitted by the model. `input` is a [`serde_json::Value`],
/// whose object maps serialize with sorted keys — deterministic, sidestepping
/// the `HashMap`-key-order cache hazard (DESIGN §3).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: CallId,
    pub name: String,
    pub input: Value,
}

/// The full, immutable result of a tool run. Survives in the log forever; never
/// overwritten by redaction — that is what makes every redaction reversible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawResult {
    pub content: String,
    pub is_error: bool,
}

/// The content of a [`Payload::Mark`] — a breadcrumb left in the record. `note` is
/// the one-line decision / dead-end / relevance / consolidation-summary text.
/// Richer structure (a `kind` discriminator, the span a consolidation boundary
/// covers) lands with Slice 1, when projection starts folding behind it. Additive
/// by design: because a mark projects to nothing today, growing this struct cannot
/// perturb the byte-stability pin (DESIGN §3).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mark {
    pub kind: MarkKind,
    pub note: String,
}

/// What a [`Mark`] does to the projection. Slice 0 introduces only the
/// consolidation `Boundary`; the §1/§2 salience kinds (decision / dead-end /
/// relevance — the "keep marks, drop deliberation" breadcrumbs) land when they get
/// a construction site. `upto` is the seq the boundary folds up to — recorded now,
/// read by the Slice-1 projection fold (DESIGN §10 condensed map → Phase 3).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MarkKind {
    Boundary { upto: Seq },
}

/// How a [`Payload::ToolResult`] should be projected. Flipping this (never
/// touching `raw`) is the one redaction primitive — every projection experiment
/// is a change to how [`project`] reads these (DESIGN §1, §2).
///
/// Phase 0 only ever constructs `Raw`; `Distilled`/`Elided` are wired into
/// [`project`] now so Phase 1 adds *construction sites*, not a type change that
/// would invalidate the byte-stability test.
///
/// [`project`]: crate::project::project
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WireState {
    /// Project the full `raw` content.
    Raw,
    /// Project this replacement instead; `raw` stays in the log.
    Distilled(String),
    /// Project nothing of substance; still recoverable from the log.
    Elided,
}
