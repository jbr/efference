//! The triage's logged decision (docs/triage.md).

use super::*;

/// A triage verdict (docs/triage.md) — what the model decided in a throwaway
/// triage, recorded durably so every triage decision is reconstructable from the log
/// (including ones only ever visible inside the fork). The rendered wire form lives
/// on the paired [`ToolResult`]'s [`WireState`]; this carries the *provenance* the
/// rendered artifact can't, with no overlap.
///
/// [`ToolResult`]: Payload::ToolResult
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Triage {
    /// The `seq` of the [`Payload::ToolResult`] this verdict triaged — `None` for a
    /// **standalone** triage (the idle-triggered inbox-only triage, docs/inbox.md: a triage
    /// that rides no tool result). Backward-compatible: serde reads an old bare `seq` as `Some`.
    pub for_result: Option<Seq>,
    /// Whether the verdict came from the model or a harness fallback. For a
    /// model-free stub triager this is the one signal the rendered artifact
    /// erases (its head/tail verdict renders like a deliberate one); a failed triage
    /// additionally projects a withheld-output error (docs/triage.md).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse (empty for a model-free triager).
    pub raw_keep: String,
    /// Per-notification dispositions chosen in this triage, by the notification's
    /// arrival `seq` (docs/inbox.md). Empty on a triage that was shown nothing.
    pub inbox: Vec<(Seq, Disposition)>,
    /// Context items this triage chose to **surface** — names from the boot `Preamble`'s
    /// structured listing (stub files with a `path`) the model judged relevant to the
    /// work in flight (docs/triage.md). `project` re-renders each as a
    /// read-me reminder at this entry's tail-ward position, off the listing (like a
    /// pinned notification's body). Also the surfaced-once dedup source: a later triage's
    /// roster excludes any name already here. `#[serde(default)]`: old logs read empty.
    #[serde(default)]
    pub surfaced: Vec<String>,
    /// One breadcrumb per **non-compliant triage attempt** this triage saw, in order,
    /// attempt-stamped — recorded even when an in-fork retry recovered
    /// (docs/triage.md). A runtime triage failure is a harness defect
    /// signal, so it must survive to the log rather than only the daemon's stderr;
    /// the compliance record is a fold over this field. Triage-scoped: rides the last
    /// `Triage` of a batch, like `inbox`. `#[serde(default)]`: old logs read empty.
    #[serde(default)]
    pub retries: Vec<String>,
    /// The triage's own reasoning — the text of every thinking block the fork emitted,
    /// one entry per attempt that produced any, in attempt order (docs/triage.md).
    /// The fork runs under the same thinking policy as an agent turn (the shared
    /// envelope), so most triages think; without this the log records *what* was decided
    /// and never *why*. Signatures are not kept: this is a record, not a replay input —
    /// the fork's transcript stays throwaway. Operator/developer observability only;
    /// [`project`] never reads it, so recording it cannot move the wire.
    /// `#[serde(default)]`: old logs read empty.
    ///
    /// [`project`]: crate::project::project
    #[serde(default)]
    pub thinking: Vec<String>,
}

/// Where a [`Triage`] verdict came from (docs/triage.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// A real `retain` call from the triage model.
    Model,
    /// The harness fallback — the triage failed (the result projects a withheld-output
    /// error, docs/triage.md), or a model-free stub triager ran.
    Fallback,
}
