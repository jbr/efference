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
    /// A model turn: leading thinking blocks, prose, then zero or more tool calls.
    ///
    /// `thinking` holds the extended-thinking blocks that lead the turn on the wire
    /// (`thinking` → `text` → `tool_use`); it is a separate typed field rather than folded
    /// into `text` because thinking is always a *leading group* under efference's
    /// single-tool-per-turn constraint (`disable_parallel_tool_use` → no intra-turn
    /// `think→act→think` interleaving), so field order encodes wire order and `calls` stays a
    /// first-class handle (backlog/thinking-blocks.md). `#[serde(default)]` keeps pre-thinking
    /// log entries deserializable.
    Assistant {
        #[serde(default)]
        thinking: Vec<ContentBlock>,
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
    /// A config edit — the durable record of one change to the policy table
    /// (backlog/permissions/config-edit.md). The config-edit loop turns a deny into
    /// negotiation: an orchestrator (human or coordinating session) edits the session's
    /// config, and the change lands here. Effective policy = base config + the fold of these
    /// (see [`effective_config`]); the log is the source of truth for what changed, exactly as
    /// it is for everything else (docs/substrate.md). The fold is **replace-at-key**
    /// (last-write-wins, order-defined) — materializing a table from an ordered log, like a
    /// database replaying its write-ahead log. Audit-only on the wire — it projects to nothing,
    /// like [`Spot`]; its effect reaches the model *by key*: a `commands`/`tools` edit only
    /// through the classifier, while a **session-visible** edit (`scope`, `paths`) *also*
    /// surfaces as an [`Ambient`] notification (an optional tail append — never a re-render of
    /// the cached preamble; see [`crate::daemon`]) so the session learns its perimeter moved.
    ///
    /// [`effective_config`]: crate::policy::effective_config
    /// [`Ambient`]: Payload::Ambient
    ConfigEdit(ConfigEdit),
}

/// A content block authored by the model or user. `Thinking` / `RedactedThinking` carry the
/// extended-thinking payload (docs/transport.md, backlog/thinking-blocks.md): `signature` is
/// the opaque per-block token the API mints and requires echoed **verbatim** on replay (it
/// rejects a modified block); `redacted_thinking` carries opaque `data` instead of text.
/// Richer blocks (images, …) come later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    Thinking { text: String, signature: String },
    RedactedThinking { data: String },
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
    /// The command prefix this spotter call was also asked to judge for **promotion**, if a
    /// prefix-promotion probe rode along (backlog/permissions/prefix-promotion.md). The probe's
    /// answer *is* the stance — `ClearAlways` is the safe-vote for this prefix, any other stance is
    /// not-safe — so the ledger that decides promotion folds from these `Spot` entries: a
    /// `probed_prefix = Some(p)` with `origin = Model` contributes a vote for `p`. `None` when no
    /// probe rode (most calls), so old logs and unprobed calls fold to nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probed_prefix: Option<String>,
}

/// The spotter's graded disposition for one move (docs/permissions.md). It splits
/// the fast-path's binary `Gate` into `Clear` (auto-approve, no human), `Watch` (the
/// middle setting the unbuilt watch-window will map to an intervention latency — for now
/// it resolves to the human gate alongside `Gate`), and `Gate`. [`Stance::ClearAlways`]
/// is a *refinement of `Clear`* the prefix-promotion probe adds: it clears the move **and**
/// votes the probed prefix blanket-safe (backlog/permissions/prefix-promotion.md). Folding the
/// promotion answer into the stance — rather than a separate flag — makes the contradictory
/// combinations (`watch`/`gate` + "always safe") unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    /// Reversible-and-in-scope (or irreversible-but-scope-sanctioned): auto-approve.
    Clear,
    /// Like [`Stance::Clear`] (auto-approve the move) **and** the probe's prefix is safe for *every*
    /// argument under this mission — the safe-vote that drives prefix promotion. Only meaningful when
    /// a probe rode the call (`Spot::probed_prefix` is `Some`); the move-approval path treats it
    /// identically to `Clear`.
    #[serde(rename = "clear_always")]
    ClearAlways,
    /// Probably fine but uncertain, or a one-way move worth a beat of attention.
    /// Resolves to the human gate until the watch-window lands.
    Watch,
    /// Out of scope or unsanctioned-irreversible: stop and ask the human.
    Gate,
}

/// One config edit: a single, durable change to the policy table (backlog/permissions/config-edit.md).
/// The change vocabulary lives here, in the substrate, deliberately — like [`Stance`] and
/// [`Origin`], the log owns its own serialized types so the wire format isn't hostage to a
/// config struct's layout. [`crate::policy::apply_config_change`] applies each [`ConfigChange`] to
/// the config it folds into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEdit {
    /// Where this edit sits in the propose→commit/cancel lifecycle
    /// (backlog/permissions/proposal-lifecycle.md). The fold gates on it: only [`EditState::Committed`]
    /// takes effect (and, for session-visible keys, notifies); a [`EditState::Proposed`] entry is
    /// inert in the effective config — it sits in the daemon's pending-proposals queue awaiting an
    /// orchestrator's commit. The synchronous human-gate path appends `Committed` directly.
    pub state: EditState,
    /// Which pending proposal this entry resolves, by the `Proposed` entry's `seq` — the foreign
    /// key from a `Committed`/`Cancelled` decision back to its proposal (the audit link, and how
    /// the daemon drops the right entry from its pending-proposals queue). `None` for a *direct*
    /// commit that had no prior proposal (the synchronous human gate, fs-watch) and for a
    /// `Proposed` entry itself (a proposal resolves nothing).
    pub resolves: Option<Seq>,
    /// What changed: which slot, and its new value. Replace-at-key — the fold sets the addressed
    /// key, last-write-wins, so replaying a log in order materializes the table deterministically.
    pub change: ConfigChange,
    /// Whether the edit is session-scoped or was also persisted to the config file. The fold
    /// ignores this (both take effect this session); it is audit metadata — the answer to "did
    /// this survive into future boots?".
    pub lifetime: Lifetime,
    /// The orchestrator's one-line rationale, addressed to the session (and the corpus byproduct:
    /// the labeled `(change, why)` pair for any future distilled approval model).
    pub note: String,
}

/// Where a [`ConfigEdit`] sits in the propose→commit/cancel lifecycle
/// (backlog/permissions/proposal-lifecycle.md). Append-only means the three states are **distinct
/// entries sharing the proposal's `seq`**, not a mutated record: an async initiator (the session's
/// `propose_scope`, the spotter's prefix-promotion) appends a `Proposed`, and the orchestrator later
/// appends a `Committed` or `Cancelled`. The synchronous human-gate path and fs-watch append
/// `Committed` directly (the human's gesture / file edit *is* the commit). The fold applies only
/// `Committed`; `Proposed` and `Cancelled` are audit-only and never reach the effective config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditState {
    /// Filed by an async initiator, awaiting an orchestrator decision. Inert in the effective
    /// config; lives in the daemon's pending-proposals queue, addressed by its `seq`.
    Proposed,
    /// In effect: the fold applies its `change` (and, for `scope`/`paths`, notifies the session).
    Committed,
    /// The orchestrator declined a `Proposed` edit. Audit-only — drops the proposal from the queue,
    /// never touches the effective config.
    Cancelled,
}

/// What a [`ConfigEdit`] changes — the `{ which slot, new value }` reshape
/// (backlog/permissions/config-edit.md). Each variant addresses one section of the config table;
/// [`crate::policy::apply_config_change`] sets the named key (replace-at-key, last-write-wins).
/// Most variants set membership or a perm; [`ConfigChange::Scope`] replaces a scalar — the
/// narrowing edit append-only KDL couldn't do. The `watch-for` slot is unbuilt (its replace
/// semantics over a list are an unsettled question; add it when an initiator needs it).
///
/// **Session-visibility** (which edits notify the session) is decided by variant in
/// [`crate::daemon`]: `Scope` and `Path` are the perimeter the session should learn moved;
/// `Command` and `Tool` reach the model only through the classifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigChange {
    /// Clear a bash command-prefix: add `command` to the `[commands]` set. The string is the
    /// key itself (a re-lexable command prefix); empty-args = blanket-trust the program.
    Command { command: String },
    /// Set a filesystem subtree's perms: `path` and everything beneath it (`write` implies
    /// `read`, as in the config). **Session-visible** — moving the spatial perimeter notifies.
    Path {
        path: String,
        read: bool,
        write: bool,
    },
    /// Grant a structured-tool name: add `name` to the `tools` set.
    Tool { name: String },
    /// Replace the session **scope** (the perimeter prose). **Session-visible** — the boot
    /// preamble stays frozen (re-rendering it would mutate behind the cache breakpoint), so a
    /// live scope edit reaches the session as a notification, not a preamble rewrite
    /// (backlog/permissions/config-edit.md).
    Scope { text: String },
}

/// How long a [`ConfigEdit`] lives (backlog/permissions/config-edit.md). The MVP carries the two
/// endpoints; turn/time-TTL is deferred (and time-TTL is a determinism hazard — timestamps are
/// never projected, so it could only gate the recomputed classifier, never the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifetime {
    /// In effect for this session only: a log entry that dies with the daemon.
    Session,
    /// Also written back to the role config file, so it survives into future boots. Sound as an
    /// append because grants are additive (backlog/permissions/config-edit.md).
    Persist,
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
