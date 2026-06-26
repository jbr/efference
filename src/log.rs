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
    /// the deliberation around it is elided. [`MarkKind::Boundary`] folds its named
    /// span (`seq <= upto`) to a summary user message on the wire (Slice 1,
    /// DESIGN §10 Phase 3).
    ///
    /// [`project`]: crate::project::project
    Mark(Mark),
    /// A fork verdict (DESIGN §4) — the durable record of a throwaway peek's
    /// decision. The fork's *transcript* stays throwaway; its *decision* is logged
    /// here for audit / introspection / distiller-quality work. Projects as audit
    /// only, except it emits a block per **pinned** notification (its `inbox`) at this
    /// entry's tail-ward position (DESIGN §6).
    Triage(Triage),
    /// An out-of-band notification (DESIGN §6), logged at **first-show** — the moment
    /// a fork peek first shows it to the model. Pure audit on the canonical wire (it
    /// projects to nothing); a *pinned* one is emitted by its resolving [`Triage`],
    /// at that entry's position. Its `seq` is its durable id (the handle the `retain`
    /// inbox verdict references).
    Ambient(Notification),
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

/// What a [`Mark`] does to the projection. `Boundary` is the consolidation
/// variant (Slice 1 — active in `project`): folds every entry with `seq <=
/// upto` to a summary user message. The §1/§2 salience kinds (decision /
/// dead-end / relevance) land when they get a construction site.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MarkKind {
    Boundary { upto: Seq },
}

/// A fork verdict (DESIGN §4) — what the model decided in a throwaway peek, recorded
/// durably so every fork decision is reconstructable from the log (including ones only
/// ever visible inside the fork). The rendered wire form lives on the paired
/// [`ToolResult`]'s [`WireState`]; this carries the *provenance* the rendered artifact
/// can't, with no overlap.
///
/// [`ToolResult`]: Payload::ToolResult
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Triage {
    /// The `seq` of the [`Payload::ToolResult`] this verdict distilled — `None` for a
    /// **standalone** triage (the idle-triggered inbox-only peek, DESIGN §6: a gate that
    /// rides no tool result). Backward-compatible: serde reads an old bare `seq` as `Some`.
    pub for_result: Option<Seq>,
    /// Whether the verdict came from the model or the keep-all fallback — the one
    /// signal the rendered artifact erases (a deliberate keep-all and a failed peek
    /// render identically).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse (empty for a model-free distiller).
    pub raw_keep: String,
    /// Per-notification dispositions chosen in this peek, by the notification's
    /// arrival `seq` (DESIGN §6). Always empty until Slice C wires the inbox.
    pub inbox: Vec<(Seq, Disposition)>,
}

/// Where a [`Triage`] verdict came from (DESIGN §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// A real `retain` call from the peek model.
    Model,
    /// The keep-all fallback — the peek failed, or a model-free stub distiller ran.
    Fallback,
}

/// What the agent chose to do with one notification at a triage gate (DESIGN §6).
/// "Leave" (bump to the next gate) is the *absence* of an entry, so it isn't a variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    /// Promote into canonical working context (projects at the [`Triage`] position).
    Pin,
    /// Clear from the inbox (logged for audit, never reaches the wire).
    Dismiss,
}

/// An out-of-band notification (DESIGN §6): freeform `body` + `source`, optionally
/// bound to a logical `slot` (for supersession in the daemon's pending queue).
/// Immutable; once logged as a [`Payload::Ambient`] its arrival `seq` is its id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    pub source: String,
    pub body: String,
    pub slot: Option<String>,
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
    /// Project the **verdict** — which lines survive (`keep`) plus an optional `note`
    /// — re-rendered from `raw` by [`project`] each turn, *not* the rendered bytes
    /// (DESIGN §4). The kept bytes are never stored twice, and the structured decision
    /// doubles as the record. `raw` stays in the log.
    ///
    /// [`project`]: crate::project::project
    Distilled { keep: Keep, note: Option<String> },
    /// Project nothing of substance; still recoverable from the log.
    Elided,
}

/// What of a tool result survives on the canonical wire (DESIGN §4). Ranges are
/// 1-based inclusive line spans, interpreted against the first `PEEK_LINE_CAP` lines —
/// the set the fork peek showed the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keep {
    /// Keep every (shown) line.
    All,
    /// Keep nothing — only the `note` breadcrumb survives.
    None,
    /// Keep these line ranges (the model's `retain(lines=…)` choice).
    Ranges(Vec<(usize, usize)>),
}
