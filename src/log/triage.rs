//! The triage's logged decision (docs/triage.md).

use super::*;

/// A triage verdict (docs/triage.md) — what the model decided in a throwaway
/// triage, recorded durably so every triage decision is reconstructable from the log
/// (including ones only ever visible inside the fork). The rendered wire form lives
/// on the paired [`ToolResult`]'s [`WireState`]; this carries the *provenance* the
/// rendered output can't, with no overlap.
///
/// [`ToolResult`]: Payload::ToolResult
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Triage {
    /// The `seq` of the [`Payload::ToolResult`] this verdict triaged — `None` for a
    /// **standalone** triage (the idle-triggered inbox-only triage, docs/inbox.md: a triage
    /// that rides no tool result). Backward-compatible: serde reads an old bare `seq` as `Some`.
    pub for_result: Option<Seq>,
    /// Whether the verdict came from the model or a harness fallback. For a
    /// model-free stub triager this is the one signal the rendered output
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
    /// The last arrival this triage's rider showed (docs/inbox.md): every notification
    /// whose arrival `seq` is at or below it, and with no verdict yet, was in front of
    /// the model. Absent means this entry's own `seq`, which is what a log written before
    /// the field recorded (a notification was logged when a triage first showed it, so it
    /// sat just ahead of that triage). A triage that showed no rider records `Some(0)` (seq 0 is the boot `Preamble`),
    /// and one that showed it records the highest `seq` the rider held, so a notification
    /// that arrived while the request was in flight reads as new at the next triage.
    /// [`project`] never reads it.
    ///
    /// [`project`]: crate::project::project
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown_through: Option<Seq>,
}

/// Where a [`Triage`] verdict came from (docs/triage.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "TriageOrigin"))]
pub enum Origin {
    /// A real `retain` call from the triage model.
    Model,
    /// The harness fallback — the triage failed (the result projects a withheld-output
    /// error, docs/triage.md), or a model-free stub triager ran.
    Fallback,
}
