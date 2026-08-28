//! The append-only annotated log — the source of truth (docs/README.md, docs/log-and-projection.md).
//!
//! Phase 0 minimal slice: only the `User` / `Assistant` / `ToolResult` payloads
//! and the [`WireState`] types needed to drive the bash-only loop. `Mark`,
//! `Ambient`, and `Commit` payloads (and the model-driven distiller) arrive with
//! the phases that exercise them — see docs/README.md.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod process;
mod upstream;
pub use process::{ExitOutcome, ProcessEvent, ProcessEventKind, ProcessExit, ProcessSpawn};
pub use upstream::{
    CAPTURED_HEADERS, FRAME_CAPTURE_CAP, StreamDiagnostics, UpstreamFailureRecord,
    is_captured_header,
};

/// Monotonic per-entry sequence number; the address space [`project`] reads over.
///
/// [`project`]: crate::project::project
pub type Seq = u64;

/// Which agent turn produced an entry. Metadata only — never projected.
pub type Turn = u32;

/// Wall-clock millis since the Unix epoch. **Log metadata only**: timestamps are
/// never projected onto the wire, so their non-determinism cannot affect cache
/// stability (docs/log-and-projection.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// The wall clock, now. The single place a wall-clock reading enters the process, so
    /// the non-determinism has one door rather than several.
    ///
    /// Safe for anything that is never projected — log metadata, and the absolute
    /// instants control-API state representations carry so clients can compute elapsed
    /// without the daemon re-emitting a ticking field (docs/control-api.md). It must not
    /// reach the wire projection, where any per-process variation breaks the cache prefix
    /// (docs/log-and-projection.md).
    pub fn now() -> Self {
        Self(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_millis() as u64,
        )
    }
}

/// An Anthropic tool-use id (`toolu_…`) and its matching `tool_result` id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub String);

/// One immutable, ordered record in the log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: Seq,
    pub turn: Turn,
    pub at: Timestamp,
    /// The submission id of the `POST /v1/turns` this entry realized
    /// (docs/control-api.md): client-minted when the body carried one,
    /// daemon-minted otherwise, `None` on entries the loop originates itself
    /// (redirects, handoff notes). **Log metadata only** — never projected,
    /// like `at` and `turn` (docs/log-and-projection.md). Clients key delivery
    /// reconciliation on it; it also records which socket submission produced
    /// the entry, for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
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
    /// first-class handle (docs/transport.md). `#[serde(default)]` keeps pre-thinking
    /// log entries deserializable.
    Assistant {
        #[serde(default)]
        thinking: Vec<ContentBlock>,
        text: Vec<ContentBlock>,
        calls: Vec<ToolCall>,
        /// `Some` = the upstream stream died before finalizing (no `message_stop` /
        /// `[DONE]`, an in-stream `error` event, or a mid-stream transport failure) —
        /// or, for an entry with no content at all, the request itself failed and this
        /// records the error body durably (docs/transport.md). Its `why` says which:
        /// the provider's error verbatim when one arrived, a harness-written note for
        /// a silent close. One path attaches more — a stream that finalizes empty
        /// carries [`StreamDiagnostics`], because nothing else about it survives
        /// anywhere. Such an entry is **record-only**: the partial (often the
        /// only copy anywhere) stays in the log for audit, but [`project`] skips the
        /// entry entirely — the provider has no record of the generation and doesn't
        /// stand behind it, so it must not enter the trajectory. The assistant-side
        /// analogue of a [`WireState`] verdict: the mark lives in the entry because
        /// projection is deterministic over the log alone (docs/log-and-projection.md).
        /// `None` (the default; skipped when serializing, so finalized entries and old
        /// logs are byte-identical) = a finalized turn, projected as always. A record
        /// with no diagnostics serializes as a bare string, the shape older logs
        /// wrote, and either shape deserializes.
        ///
        /// [`project`]: crate::project::project
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upstream_failure: Option<UpstreamFailureRecord>,
    },
    /// The result of running one tool call. `raw` is immutable; redaction only
    /// flips `wire`.
    ToolResult {
        call_id: CallId,
        raw: RawResult,
        wire: WireState,
    },
    /// A salience annotation written at the moment it's known (docs/README.md,
    /// docs/log-and-projection.md): a decision, a dead end, a relevance note, or a
    /// consolidation boundary. The Phase-3 foundation for intent-aware compaction —
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
    /// Per-call tool telemetry (docs/tools.md) — how long one call took, how it
    /// came out, and how big its result was, appended right after the
    /// [`ToolResult`] it describes. Projects to nothing: it is log metadata in
    /// the same sense timestamps are, so it can be added, grown, or read back
    /// without touching the wire or the cached prefix
    /// (docs/log-and-projection.md).
    ///
    /// A separate entry rather than a field on [`ToolResult`], which is the
    /// shape [`Spot`] already uses for per-call audit: the pairing is by
    /// `call_id`, the payload is written by a different concern than the result,
    /// and [`ToolResult`] is constructed at dozens of sites (many of them a
    /// harness stand-in with no run to measure) that would each have to name a
    /// telemetry value they do not have.
    ///
    /// [`ToolResult`]: Payload::ToolResult
    /// [`Spot`]: Payload::Spot
    ToolTelemetry(ToolTelemetry),
    /// One lifecycle fact about a background process — its mint or its exit
    /// (docs/supervisor.md). The supervisor's registry is ephemeral, but a
    /// `p#<n>` handle appears in prose (the backgrounding result, the exit
    /// notification) that outlives it, so these entries are what a reader of a
    /// recorded session — or a restarted daemon — resolves the handle against.
    /// Metadata only: the command, the spawning call, the spawn time, the exit
    /// status and duration. A process's *output* is the registry's bounded
    /// best-effort capture and is deliberately not recorded here.
    ///
    /// Projects to nothing, like [`ToolTelemetry`] — the exit still reaches the
    /// model as an [`Ambient`] notification, and this is the audit record beside
    /// it, not a second copy on the wire (docs/log-and-projection.md).
    ///
    /// [`ToolTelemetry`]: Payload::ToolTelemetry
    /// [`Ambient`]: Payload::Ambient
    ProcessEvent(ProcessEvent),
    /// A config edit — the durable record of one change to the policy table
    /// (docs/permissions.md). The config-edit loop turns a deny into
    /// negotiation: an orchestrator (human or coordinating session) edits the session's
    /// config, and the change lands here. Effective policy = base config + the fold of these
    /// (see [`effective_config`]); the log is the source of truth for what changed, exactly as
    /// it is for everything else (docs/log-and-projection.md). The fold is **replace-at-key**
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
    /// The boot preamble snapshot (docs/context.md): the full rendered `system`
    /// string, exactly as this session's requests carry it, appended **once at boot**. The
    /// preamble folds in `[context]` file contents that drift on disk between boots, so it is
    /// no longer reconstructable from `config + current files` — this entry makes the log the
    /// complete record of the wire (docs/log-and-projection.md). Audit/replay only: it projects to
    /// nothing (the system prompt rides the request's `system` field, never the `messages`
    /// array); the `wire` dev view reads it back instead of re-resolving files from disk.
    ///
    /// `context` carries the **structured** `[context]` listing (name / `path` / description
    /// per item) the flat `system` string was rendered from, snapshotted **additively** so the
    /// listing survives as a fold over the log — the source `project` re-renders a surfaced
    /// context item from (docs/redaction-fork.md), the same way it looks up pinned
    /// notification bodies. `system` stays the byte-exact wire truth; `context` never touches
    /// the wire. `#[serde(default)]`: logs predating the field read back an empty listing.
    Preamble {
        system: String,
        #[serde(default)]
        context: crate::context::Resolved,
    },
}

/// A content block authored by the model or user. `Thinking` / `RedactedThinking` carry the
/// extended-thinking payload (docs/transport.md, docs/transport.md): `signature` is
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
/// the `HashMap`-key-order cache hazard (docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: CallId,
    pub name: String,
    pub input: Value,
}

impl ToolCall {
    /// The harness-injected `intent` (docs/tools.md): why this call was made, in the
    /// model's words, written before it ran. `None` for a call from before the field
    /// existed, one the model omitted, or a tool that declares its own `intent` — no
    /// reader may assume it is present.
    ///
    /// It stays in `input` rather than becoming a field of its own: the wire carries
    /// it as part of the `tool_use` block, so it is already in the projection and in
    /// every recorded log, and lifting it out would move the pinned wire bytes
    /// (docs/log-and-projection.md). Only the dispatch path removes it.
    pub fn intent(&self) -> Option<&str> {
        self.input
            .get(crate::tools::INTENT)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|intent| !intent.is_empty())
    }
}

/// The full, immutable result of a tool run. Survives in the log forever; never
/// overwritten by redaction — that is what makes every redaction reversible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawResult {
    pub content: String,
    pub is_error: bool,
    /// Set **only by the control-channel entry view**, and only on the window and
    /// frame copies of an oversized result: it says the `content` beside it is a
    /// prefix, and how much of the whole string that prefix is
    /// (`daemon/entry_view.rs`, docs/control-api.md). The mirror of
    /// [`ResultImage::data`], which the same views omit — a Rust client parsing a
    /// view with this shared type reads the elision as a value rather than
    /// inferring it from a length. The log itself always writes `content` whole
    /// and never writes this field, so it is `None` in every in-process result and
    /// absent from every logged line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_elided: Option<ContentElision>,
    /// Images the tool returned alongside `content` (docs/log-and-projection.md).
    /// The realized bytes live here, in the log entry itself, so projection is a
    /// copy of stored data — never a re-read or re-encode of anything on disk.
    /// Encoding happens once, at tool-run time. Defaulted and skipped when empty,
    /// so text-only results (and every existing log) serialize byte-identically
    /// to before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ResultImage>,
}

/// The tag on a `content` a control-channel view truncated: how many bytes the view
/// carries, and how many the whole result holds. `GET /log/:seq` serves the rest
/// (docs/control-api.md).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentElision {
    pub kept_bytes: usize,
    pub total_bytes: usize,
}

/// One image in a [`RawResult`]: the exact bytes projected onto the wire, plus
/// the dimensions of the image as encoded. `width`/`height` are log metadata
/// (audit, TUI, token math) — the wire image block carries only `media_type` +
/// `data`; the dimensions the *model* needs ride in the result's text content,
/// written by the tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultImage {
    /// An API-accepted media type: `image/jpeg`, `image/png`, `image/gif`, or
    /// `image/webp`.
    pub media_type: String,
    /// Standard base64 of the encoded image, stored exactly as it will appear
    /// in the wire block's `source.data`. `#[serde(default)]` because the
    /// control-channel views omit this field (the bytes are fetched from
    /// `GET /log/:seq/images/:idx` instead — `daemon/entry_view.rs`,
    /// docs/control-api.md), and a client parsing a view with this shared type
    /// reads it back empty. The log itself always writes it.
    #[serde(default)]
    pub data: String,
    pub width: u32,
    pub height: u32,
}

/// The content of a [`Payload::Mark`] — a breadcrumb left in the record. `note` is
/// the one-line decision / dead-end / relevance / consolidation-summary text.
/// Richer structure (a `kind` discriminator, the span a consolidation boundary
/// covers) lands with Slice 1, when projection starts folding behind it. Additive
/// by design: because a mark projects to nothing today, growing this struct cannot
/// perturb the byte-stability pin (docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mark {
    pub kind: MarkKind,
    pub note: String,
}

/// What a [`Mark`] does to the projection. `Boundary` is the consolidation
/// variant (Slice 1 — active in `project`): folds every entry with `seq <=
/// upto` to a summary user message. The salience-annotation kinds defined in
/// docs/README.md and docs/log-and-projection.md (decision / dead-end / relevance) land
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
    /// Whether the verdict came from the model or a harness fallback. For a
    /// model-free stub distiller this is the one signal the rendered artifact
    /// erases (its head/tail verdict renders like a deliberate one); a failed peek
    /// additionally projects a withheld-output error (docs/redaction-fork.md).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse (empty for a model-free distiller).
    pub raw_keep: String,
    /// Per-notification dispositions chosen in this peek, by the notification's
    /// arrival `seq` (docs/inbox.md). Always empty until Slice C wires the inbox.
    pub inbox: Vec<(Seq, Disposition)>,
    /// Context items this peek chose to **surface** — names from the boot `Preamble`'s
    /// structured listing (stub files with a `path`) the model judged relevant to the
    /// work in flight (docs/redaction-fork.md). `project` re-renders each as a
    /// read-me reminder at this entry's tail-ward position, off the listing (like a
    /// pinned notification's body). Also the surfaced-once dedup source: a later gate's
    /// roster excludes any name already here. `#[serde(default)]`: old logs read empty.
    #[serde(default)]
    pub surfaced: Vec<String>,
    /// One breadcrumb per **non-compliant peek attempt** this gate saw, in order,
    /// attempt-stamped — recorded even when an in-fork retry recovered
    /// (docs/redaction-fork.md). A runtime triage failure is a harness defect
    /// signal, so it must survive to the log rather than only the daemon's stderr;
    /// the compliance record is a fold over this field. Gate-scoped: rides the last
    /// `Triage` of a batch, like `inbox`. `#[serde(default)]`: old logs read empty.
    #[serde(default)]
    pub retries: Vec<String>,
    /// The peek's own reasoning — the text of every thinking block the fork emitted,
    /// one entry per attempt that produced any, in attempt order (docs/redaction-fork.md).
    /// The fork runs under the same thinking policy as an agent turn (the shared
    /// envelope), so most gates think; without this the log records *what* was decided
    /// and never *why*. Signatures are not kept: this is a record, not a replay input —
    /// the fork's transcript stays throwaway. Operator/developer observability only;
    /// [`project`] never reads it, so recording it cannot move the wire.
    /// `#[serde(default)]`: old logs read empty.
    ///
    /// [`project`]: crate::project::project
    #[serde(default)]
    pub thinking: Vec<String>,
}

/// Where a [`Triage`] verdict came from (docs/redaction-fork.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// A real `retain` call from the peek model.
    Model,
    /// The harness fallback — the peek failed (the result projects a withheld-output
    /// error, docs/redaction-fork.md), or a model-free stub distiller ran.
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
    /// prefix-promotion probe rode along (docs/permissions.md). The probe's
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
/// votes the probed prefix blanket-safe (docs/permissions.md). Folding the
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

/// One config edit: a single, durable change to the policy table (docs/permissions.md).
/// The change vocabulary lives here, in the log types, deliberately — like [`Stance`] and
/// [`Origin`], the log owns its own serialized types so the wire format isn't hostage to a
/// config struct's layout. [`crate::policy::apply_config_change`] applies each [`ConfigChange`] to
/// the config it folds into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEdit {
    /// Where this edit sits in the propose→commit/cancel lifecycle
    /// (docs/permissions.md). The fold gates on it: only [`EditState::Committed`]
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
    /// The orchestrator's one-line rationale, addressed to the session (and the corpus byproduct:
    /// the labeled `(change, why)` pair for any future distilled approval model).
    pub note: String,
}

/// Where a [`ConfigEdit`] sits in the propose→commit/cancel lifecycle
/// (docs/permissions.md). Append-only means the three states are **distinct
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
/// (docs/permissions.md). Each variant addresses one section of the config table;
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
    /// (docs/permissions.md).
    Scope { text: String },
}

/// What one tool call cost and how it came out (docs/tools.md), recorded per
/// call as a [`Payload::ToolTelemetry`] entry.
///
/// It exists because the questions it answers were previously answered by
/// grepping result *prose*: "did that edit apply or get refused" was recovered
/// from `startswith("Applied")`, and anchor discipline was inferred from
/// argument lengths plus a count of `===TIP===` markers that turned out to be a
/// throttle cap rather than a measure. Prose written for a model is not a
/// measurement surface; this is.
///
/// Additive by construction: because it projects to nothing, growing this struct
/// cannot perturb the byte-stability pin (docs/log-and-projection.md) — the same
/// property [`Mark`] has.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolTelemetry {
    /// The `tool_use` id — the join back to the call and its [`Payload::ToolResult`].
    pub call_id: CallId,
    /// The wire tool name, namespaced as the call carried it.
    pub tool: String,
    /// Wall-clock milliseconds the dispatch took: the permission cascade is
    /// *excluded* (a call that waited an hour at a human gate did not run for an
    /// hour), and a backgrounded spawn is the spawn alone.
    pub duration_ms: u64,
    pub outcome: ToolOutcome,
    /// Bytes of result text as the tool produced it — **before** the fork peek
    /// distills it (docs/redaction-fork.md). The pair (this, the distillate's
    /// size) is what says how much a gate actually saved.
    pub result_bytes: usize,
    /// Realized image blocks the result carried. Zero for every text tool;
    /// defaulted and skipped so those entries stay small.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub images: usize,
    /// Bash-specific fields, when the call was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bash: Option<BashTelemetry>,
    /// Edit-shaped fields, when the call carried the arguments they read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<EditTelemetry>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// How one tool call came out (docs/tools.md). Derived generically at the
/// dispatch seam from `is_error`, which since 2026-08-27 means *refusal* for
/// built-in negative outcomes — so the three classes are readable off every
/// tool without per-tool knowledge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolOutcome {
    /// The call did what it was asked to do.
    Applied,
    /// The tool ran and declined: an anchor that matched nothing, a command that
    /// exited non-zero, a path outside a grant. The model's next move is the
    /// interesting one — this is the class whose recovery-on-next-attempt rate
    /// says whether a cheap model spirals on the errors the `is_error` flip made
    /// visible.
    Rejected,
    /// The harness could not run the call at all: no mounted tool owns the name,
    /// or a background spawn failed. Distinct from [`Rejected`] because it is a
    /// defect signal about the harness, not a datum about the model's judgment.
    ///
    /// Set from an explicit flag at the sites that *construct* a harness-authored
    /// failure, never inferred from result text. So an argument that would not
    /// deserialize is **not** here: the tool returned that error itself, and a
    /// model sending a string where the schema declares an array is a datum about
    /// the model. It classifies as [`Rejected`].
    ///
    /// [`Rejected`]: ToolOutcome::Rejected
    HarnessError,
}

/// Bash-call fields (docs/tools.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BashTelemetry {
    /// The status the result reported — `"1"`, or `"signal"` for a command a
    /// signal killed. `None` means the result reported none, which for a
    /// completed foreground command is exit zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<String>,
    /// Whether the call asked to be backgrounded, in which case `duration_ms`
    /// measures the spawn and `result_bytes` the handle message — not the
    /// command.
    pub background: bool,
    /// The `p#<n>` handle of the process this call spawned (docs/supervisor.md),
    /// as the supervisor minted it. Present for every bash command that reached
    /// a spawn — foreground included, since foreground and background share one
    /// namespace — so the handle↔call join is readable off the telemetry row
    /// without parsing result prose. `None` for a call that never spawned (a
    /// refused command, a failed background spawn) and for logs written before
    /// the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

/// Fields read off an edit-shaped call's own arguments (docs/tools.md). Every
/// value here comes from the `input` the model wrote, so it is available for any
/// tool that spells its arguments this way and costs nothing to collect; nothing
/// is plumbed out of the editing crates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditTelemetry {
    /// The call's `operation` argument, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Byte length of the anchor the call matched on — the number the
    /// anchor-discipline question is asked about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_bytes: Option<usize>,
    /// Byte length of the replacement content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_bytes: Option<usize>,
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
/// is a change to how [`project`] reads these (docs/README.md, docs/log-and-projection.md).
///
/// The variants were wired into [`project`] ahead of their construction sites, so
/// each site landed as a behavior change rather than a type change that would
/// invalidate the byte-stability test. `Distilled` is built by the redaction fork
/// (docs/redaction-fork.md); `Elided` by the interrupt (docs/control-channel.md).
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
    /// The gate could not elicit a usable triage verdict — a transport failure or
    /// in-fork retry exhaustion (docs/redaction-fork.md). Projects a **hard error**
    /// in place of the output (`is_error` forced true), never the untriaged raw,
    /// which would weld the bloat into the cached prefix for the rest of the
    /// session. `why` is the same breadcrumb the `Triage` entry records; `raw`
    /// stays in the log.
    TriageFailed { why: String },
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
