//! The spotter's logged verdict on one gated call (docs/permissions.md).

use super::*;

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
    /// Which model returned this verdict, as **the response named itself** — the
    /// same string `RequestUsage` carries, not the configured id, so a slug that
    /// routes to something else records what actually answered.
    ///
    /// A session's spotter is fixed at boot, so this is constant within a log;
    /// it is recorded per entry anyway because the corpus that reads these
    /// spans sessions and models, and attributing a verdict by re-deriving the
    /// boot config is a join that can be wrong. `None` for a fail-closed verdict
    /// (no request happened, so nothing answered) and for entries written before
    /// this field existed — in both cases the absence is the honest reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The command prefix this spotter call was also asked to judge for **promotion**, if a
    /// prefix-promotion probe rode along (docs/permissions.md). The probe's
    /// answer *is* the stance — `ClearAlways` is the safe-vote for this prefix, any other stance is
    /// not-safe — so the ledger that decides promotion folds from these `Spot` entries: a
    /// `probed_prefix = Some(p)` with `origin = Model` contributes a vote for `p`. `None` when no
    /// probe rode (most calls), so old logs and unprobed calls fold to nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probed_prefix: Option<String>,
    /// Every probability the decision rung answered, when that rung produced the
    /// verdict (docs/permissions.md). The stance and reason are a fold over these
    /// under the thresholds of the day; recording the vector is what lets a live
    /// session's verdicts join the calibration corpus and replay under a different
    /// band without re-asking. `None` from the prose rung, from a fail-closed
    /// verdict, and on entries written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axes: Option<crate::spotter::jev::Axes>,
}

/// The spotter's graded disposition for one move (docs/permissions.md). It splits
/// the fast-path's binary `Gate` into `Clear` (auto-approve, no human), `Watch` (a
/// middle stance that resolves to the human gate alongside `Gate` until a
/// watch-window maps it to an intervention latency), and `Gate`. [`Stance::ClearAlways`]
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
