//! The spotter's logged verdict on one gated call, and the decision model's answers
//! recorded beside it (docs/permissions.md).

use std::collections::BTreeMap;

use super::*;

/// The spotter's verdict on one gated tool call (docs/permissions.md), logged as a
/// [`Payload::Spot`] audit entry. The `stance` is the graded disposition the
/// independent rung returned; the `reason` is its one-line rationale, addressed to
/// the acting session (what it reads to self-correct or petition). `origin` records
/// whether the verdict was the spotter model's or the fail-closed fallback (the one
/// signal the stance alone erases — a deliberate `gate` and a failed spotter call
/// both surface as `Gate`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    /// (no request happened, so nothing answered) and on older entries — in both
    /// cases the absence is the honest reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The command prefix this spotter call was also asked to judge for **promotion**, if a
    /// prefix-promotion probe rode along (docs/permissions.md). The probe's
    /// answer *is* the stance — `ClearAlways` is the safe-vote for this prefix, any other stance is
    /// not-safe — so the ledger that decides promotion folds from these `Spot` entries: a
    /// `probed_prefix = Some(p)` with `origin = Model` contributes a vote for `p`. `None` when no
    /// probe rode (most calls), so unprobed calls fold to nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probed_prefix: Option<String>,
    /// Every probability the decision rung answered, when that rung produced the
    /// verdict (docs/permissions.md). The stance and reason are a fold over these
    /// under the thresholds of the day; recording the vector is what lets a live
    /// session's verdicts join the calibration corpus and replay under a different
    /// band without re-asking. `None` from the prose rung and from a fail-closed
    /// verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axes: Option<Axes>,
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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

/// The decision rung's answers behind one verdict (docs/permissions.md): each battery
/// question's [`QuestionReading`] by the key it is asked under, whether the harness placed
/// the move outside the grants (the one input to the fold that is not an answer), and
/// the promotion question's answer when a probe was asked.
///
/// Recorded alongside the stance on `Spot::axes`, so the verdict can be re-folded and
/// replayed under other thresholds without asking again. A conditional question (the
/// two about spotter notes) that was not asked has no entry, so an absent key means "not
/// asked", never "answered zero".
///
/// An entry recorded under an earlier fold carries that fold's figures (`in_scope`,
/// `sketch`, `precedent`, …) instead of `readings`. They are ignored on read, and the
/// log line keeps them as written; such an entry reads as having no readings.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Axes {
    /// Each asked battery question's reading, by its key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub readings: BTreeMap<String, QuestionReading>,
    /// Whether the harness's resolved path facts place the move outside the grants: a
    /// path no grant covers, or one under a denied grant. Read by the fold, and stated
    /// by the harness rather than answered by the model.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outside_grants: bool,
    /// P(yes) to the promotion question, asked once and only when a probe was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_safe: Option<f64>,
}

impl Axes {
    /// The answers of a verdict that read `readings`, inside the grants, with no probe.
    pub fn new(readings: impl IntoIterator<Item = (String, QuestionReading)>) -> Self {
        Self {
            readings: readings.into_iter().collect(),
            outside_grants: false,
            prefix_safe: None,
        }
    }

    /// Whether the harness placed the move outside the grants.
    pub fn with_outside_grants(mut self, outside_grants: bool) -> Self {
        self.outside_grants = outside_grants;
        self
    }

    /// The promotion question's answer, when a probe was asked.
    pub fn with_prefix_safe(mut self, prefix_safe: Option<f64>) -> Self {
        self.prefix_safe = prefix_safe;
        self
    }

    /// One `name value` string per recorded figure, for the dev verbs that print a
    /// spot's whole vector: each question's reading with its two answers, positive
    /// wording first.
    pub fn summary(&self) -> Vec<String> {
        let mut parts: Vec<String> = self
            .readings
            .iter()
            .map(|(key, r)| {
                format!(
                    "{key} {:.2} ({:.2}/{:.2})",
                    r.reading, r.positive, r.negative
                )
            })
            .collect();
        if self.outside_grants {
            parts.push("outside_grants".to_string());
        }
        parts.extend(self.prefix_safe.map(|p| format!("prefix_safe {p:.2}")));
        parts
    }
}

/// One battery question's answers (docs/permissions.md). Each question is asked in two
/// wordings, as written and negated, because a decision model leans toward yes; the
/// reading is their average with the negation flipped.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct QuestionReading {
    /// P(yes) to the question as written.
    pub positive: f64,
    /// P(yes) to its negation.
    pub negative: f64,
    /// `(positive + 1 - negative) / 2`.
    pub reading: f64,
}

impl QuestionReading {
    /// The reading of a question whose wording was answered `positive` and whose
    /// negation was answered `negative`. The sum is taken in the order the harness that
    /// fitted the thresholds took it, so a reading near a threshold rounds the same way
    /// here.
    pub fn new(positive: f64, negative: f64) -> Self {
        Self {
            positive,
            negative,
            reading: (positive + 1.0 - negative) / 2.0,
        }
    }
}
