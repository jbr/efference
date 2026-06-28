//! The append-only annotated log — the source of truth (docs/README.md, docs/substrate.md).
//!
//! Phase 0 minimal slice: only the `User` / `Assistant` / `ToolResult` payloads
//! and the [`WireState`] seam needed to drive the bash-only loop. `Mark`,
//! `Ambient`, and `Commit` payloads (and the model-driven distiller) arrive with
//! the phases that exercise them — see docs/README.md.

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
/// stability (docs/substrate.md).
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
/// a payload's `raw`, only flips a [`WireState`] (docs/README.md).
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
    /// A salience annotation written at the moment it's known (docs/README.md,
    /// docs/substrate.md): a decision, a dead end, a relevance note, or a
    /// consolidation boundary. The Phase-3 substrate for intent-aware compaction —
    /// *kept* through a fold while the deliberation around it is elided.
    /// [`MarkKind::Boundary`] folds its named span (`seq <= upto`) to a summary
    /// user message on the wire (Slice 1, docs/README.md).
    ///
    /// [`project`]: crate::project::project
    Mark(Mark),
    /// A fork verdict (docs/redaction-fork.md) — the durable record of a throwaway
    /// peek's decision. The fork's *transcript* stays throwaway; its *decision* is
    /// logged here for audit / introspection / distiller-quality work. Projects as
    /// audit only, except it emits a block per **pinned** notification (its `inbox`)
    /// at this entry's tail-ward position (docs/inbox.md).
    Triage(Triage),
    /// An out-of-band notification (docs/inbox.md), logged at **first-show** — the
    /// moment a fork peek first shows it to the model. Pure audit on the canonical
    /// wire (it projects to nothing); a *pinned* one is emitted by its resolving
    /// [`Triage`], at that entry's position. Its `seq` is its durable id (the handle
    /// the `retain` inbox verdict references).
    Ambient(Notification),
    /// A spotter verdict (docs/permissions.md) — the durable record of the
    /// independent permission rung's decision on one gated tool call. Like
    /// [`Triage`], the deciding round-trip is throwaway but its *verdict* is logged:
    /// the audit answer to "why did this call get through (or stop)?" and the corpus
    /// for any future distilled approval model. Projects to nothing — it is audit
    /// only, never on the canonical wire. Fast-path clears are deliberately *not*
    /// logged; the spotter is the first permission rung that earns an entry.
    Spot(Spot),
}

/// Phase 0 content is text-only; richer blocks (images, …) come later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
}

/// A tool invocation emitted by the model. `input` is a [`serde_json::Value`],
/// whose object maps serialize with sorted keys — deterministic, sidestepping
/// the `HashMap`-key-order cache hazard (docs/substrate.md).
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
/// perturb the byte-stability pin (docs/substrate.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mark {
    pub kind: MarkKind,
    pub note: String,
}

/// What a [`Mark`] does to the projection. `Boundary` is the consolidation
/// variant (Slice 1 — active in `project`): folds every entry with `seq <=
/// upto` to a summary user message. The salience-annotation kinds defined in
/// docs/README.md and docs/substrate.md (decision / dead-end / relevance) land
/// when they get a construction site.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MarkKind {
    Boundary { upto: Seq },
}

/// A fork verdict (docs/redaction-fork.md) — what the model decided in a throwaway
/// peek, recorded durably so every fork decision is reconstructable from the log
/// (including ones only ever visible inside the fork). The rendered wire form lives
/// on the paired [`ToolResult`]'s [`WireState`]; this carries the *provenance* the
/// rendered artifact can't, with no overlap.
///
/// [`ToolResult`]: Payload::ToolResult
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Triage {
    /// The `seq` of the [`Payload::ToolResult`] this verdict distilled — `None` for a
    /// **standalone** triage (the idle-triggered inbox-only peek, docs/inbox.md: a gate
    /// that rides no tool result). Backward-compatible: serde reads an old bare `seq` as `Some`.
    pub for_result: Option<Seq>,
    /// Whether the verdict came from the model or the keep-all fallback — the one
    /// signal the rendered artifact erases (a deliberate keep-all and a failed peek
    /// render identically).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse (empty for a model-free distiller).
    pub raw_keep: String,
    /// Per-notification dispositions chosen in this peek, by the notification's
    /// arrival `seq` (docs/inbox.md). Always empty until Slice C wires the inbox.
    pub inbox: Vec<(Seq, Disposition)>,
}

/// Where a [`Triage`] verdict came from (docs/redaction-fork.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// A real `retain` call from the peek model.
    Model,
    /// The keep-all fallback — the peek failed, or a model-free stub distiller ran.
    Fallback,
}

/// The spotter's verdict on one gated tool call (docs/permissions.md), logged as a
/// [`Payload::Spot`] audit entry. The `stance` is the graded disposition the
/// independent rung returned; the `reason` is its one-line rationale, addressed to
/// the acting session (what it reads to self-correct or petition). `origin` records
/// whether the verdict was the spotter model's or the fail-closed fallback (the one
/// signal the stance alone erases — a deliberate `gate` and a failed spotter call
/// both surface as `Gate`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Spot {
    /// The `tool_use` id of the call this verdict judged.
    pub call_id: CallId,
    pub stance: Stance,
    pub reason: String,
    pub origin: Origin,
}

/// The spotter's graded disposition for one move (docs/permissions.md). It splits
/// the fast-path's binary `Gate` into three: `Clear` auto-approves with no human,
/// `Gate` falls through to the human, and `Watch` is the middle setting the
/// (unbuilt) watch-window will map to an intervention latency — for now it resolves
/// to the human gate alongside `Gate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    /// Reversible-and-in-scope (or irreversible-but-brief-sanctioned): auto-approve.
    Clear,
    /// Probably fine but uncertain, or a one-way move worth a beat of attention.
    /// Resolves to the human gate until the watch-window lands.
    Watch,
    /// Out of scope or unsanctioned-irreversible: stop and ask the human.
    Gate,
}

/// What the agent chose to do with one notification at a triage gate (docs/inbox.md).
/// "Leave" (bump to the next gate) is the *absence* of an entry, so it isn't a variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    /// Promote into canonical working context (projects at the [`Triage`] position).
    Pin,
    /// Clear from the inbox (logged for audit, never reaches the wire).
    Dismiss,
}

/// An out-of-band notification (docs/inbox.md): freeform `body` + `source`, optionally
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
/// is a change to how [`project`] reads these (docs/README.md, docs/substrate.md).
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
    /// (docs/redaction-fork.md). The kept bytes are never stored twice, and the
    /// structured decision doubles as the record. `raw` stays in the log.
    ///
    /// [`project`]: crate::project::project
    Distilled { keep: Keep, note: Option<String> },
    /// Project nothing of substance; still recoverable from the log.
    Elided,
}

/// What of a tool result survives on the canonical wire (docs/redaction-fork.md).
/// Ranges are 1-based inclusive line spans, interpreted against the first
/// `PEEK_LINE_CAP` lines — the set the fork peek showed the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keep {
    /// Keep every (shown) line.
    All,
    /// Keep nothing — only the `note` breadcrumb survives.
    None,
    /// Keep these line ranges (the model's `retain(lines=…)` choice).
    Ranges(Vec<(usize, usize)>),
}
