//! Per-call tool telemetry (docs/tools.md).

use super::*;

/// What one tool call cost and how it came out (docs/tools.md), recorded per
/// call as a [`Telemetry::Tool`] entry.
///
/// It exists so that "did that edit apply or get refused" is a field rather than
/// a grep over result prose. Prose written for a model is not a measurement
/// surface — its wording is free to change for the model's sake, and a count
/// taken off it can measure a throttle rather than the thing it names.
///
/// Additive by construction: because it projects to nothing, growing this struct
/// cannot perturb the byte-stability pin (docs/log-and-projection.md) — the same
/// property every [`Telemetry`] entry has.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    /// Bytes of result text as the tool produced it — **before** the triage
    /// triages it (docs/triage.md). The pair (this, the selection's
    /// size) is what says how much a triage actually saved.
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
/// dispatch point from `is_error`, which means *refusal* for built-in negative
/// outcomes — so the three classes are readable off every tool without per-tool
/// knowledge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    /// The `p#<n>` handle of the process this call spawned (docs/process-monitor.md),
    /// as the process monitor minted it. Present for every bash command that reached
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
