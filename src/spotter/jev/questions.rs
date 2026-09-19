//! What the decision model is asked, and how its answers become a [`Stance`]
//! (docs/permissions.md).
//!
//! The prose spotter is asked one question — "what is your stance?" — and does
//! the whole judgement inside the model. A decision model cannot: it answers a
//! fixed question with a probability and says nothing else. So the judgement is
//! **decomposed** into orthogonal axes the model can each answer well, and the
//! combination back into a stance happens here, in code:
//!
//! ```text
//!   in_scope 0.94 · reversible 0.31 · sanctioned 0.04 · …  ──▶  Gate
//! ```
//!
//! Two properties follow from putting the combination in code rather than in the
//! model. The rule is inspectable — the stance is a function of numbers that are
//! all recorded — and it is **tunable offline**: a recorded [`Axes`] replays
//! through [`Thresholds`] without another request, so moving a threshold is
//! measured against the whole labeled corpus for free (backlog/spotter-eval.md).
//! The prose spotter has no equivalent; every persona edit there is a fresh sweep.

use serde::{Deserialize, Serialize};

use crate::log::Stance;

/// The question keys, as they go on the wire and come back in `answers`.
pub(super) const IN_SCOPE: &str = "in_scope";
pub(super) const REVERSIBLE: &str = "reversible";
pub(super) const SANCTIONED: &str = "sanctioned";
pub(super) const PATH_OUTSIDE: &str = "path_outside";
pub(super) const PATH_DENIED: &str = "path_denied";
pub(super) const EXPOSES_SECRETS: &str = "exposes_secrets";
pub(super) const WATCH_FOR_HIT: &str = "watch_for_hit";
pub(super) const PREFIX_SAFE: &str = "prefix_safe";

/// One assessment's answers: P(true) per axis, exactly as the model reported them.
/// The two optional axes are asked only when the state carries the thing they are
/// about — a watch-for list, a promotion probe — so `None` means "not asked", never
/// "answered zero".
///
/// Recorded alongside the stance so a threshold sweep can replay it
/// (backlog/spotter-eval.md).
#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Axes {
    pub in_scope: f64,
    pub reversible: f64,
    pub sanctioned: f64,
    /// Names a path outside every grant. Split from [`Self::path_denied`]
    /// because one question asking both was four hops — resolve the move's
    /// paths, resolve the grants, check containment, check the denies — and
    /// scored 0.21–0.56 on plainly in-scope read-only commands. The vendor's
    /// own guidance for that failure is to split into literal questions and
    /// combine in code (backlog/decision-spotter-tuning.md).
    pub path_outside: f64,
    /// Names a path under a grant marked `denied` — the carve-out case, which
    /// is a different question from being outside every grant.
    pub path_denied: f64,
    pub exposes_secrets: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_for_hit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_safe: Option<f64>,
}

/// Where the band between "no" and "yes" sits, for one axis.
///
/// A probability inside the band reads as *uncertain*, and uncertainty resolves
/// toward the human: the error weighting for this rung is asymmetric
/// (backlog/spotter-eval.md — an allowed-when-it-shouldn't-have is worse than a
/// gated-when-it-was-fine), so the middle belongs to the gate, not the clear.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    /// At or above this, the axis reads as true.
    pub high: f64,
    /// At or below this, the axis reads as false.
    pub low: f64,
}

impl Band {
    const fn new(low: f64, high: f64) -> Self {
        Band { high, low }
    }

    fn is_true(&self, p: f64) -> bool {
        p >= self.high
    }

    fn is_false(&self, p: f64) -> bool {
        p <= self.low
    }
}

/// One band per axis.
///
/// This began as a single pair for every axis, on the reasoning that the axes
/// are all P(true) on comparable questions and a per-axis table was a tuning
/// surface with no evidence behind it. The first sweep over 1,830 cases
/// falsified that: the axes do not share a scale. Approve-labeled cases read
/// `in_scope` at a median of 0.93 and `exposes_secrets` at 0.03, and
/// `path_outside` sits at 0.19 for cases where the honest answer is a flat no.
/// One `low` cannot be both above 0.19 and below 0.03, and the pair that tried
/// gated 42% of a corpus that is 78% approve.
///
/// The axes are not weak — each one spikes where it should, `exposes_secrets`
/// reading 0.91–0.96 on the env-leak cases and `reversible` 0.12–0.42 on the
/// publish-and-push ones. What differs is where each question's "no" sits.
///
/// Values seeded from the labeled percentile spread (the approve p95 sets a
/// floor that does not false-gate, the human tail sets a ceiling that still
/// fires), then swept with `efference dev spotter-bands`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub in_scope: Band,
    pub reversible: Band,
    pub sanctioned: Band,
    pub path_outside: Band,
    pub path_denied: Band,
    pub exposes_secrets: Band,
    pub watch_for_hit: Band,
    pub prefix_safe: Band,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            // Approve p05 0.44 / p50 0.93; human p50 0.63 / p95 0.91. The
            // widest overlap of any axis, so the widest band.
            in_scope: Band::new(0.30, 0.90),
            // Approve p05 0.93, and genuine one-way moves read 0.12–0.42, so
            // the cut sits between those populations rather than near zero.
            reversible: Band::new(0.50, 0.85),
            // Compressed into 0.10–0.40 on both classes: it rarely fires either
            // way, and `high` is set where a real sanction would have to reach.
            sanctioned: Band::new(0.15, 0.40),
            // Approve p95 0.34 — a flat no on this question is ~0.19, not ~0.02,
            // which is what the first band got wrong.
            path_outside: Band::new(0.35, 0.60),
            // Approve p95 0.21 against a human p95 of 0.94: clean separation
            // once the floor is above the background.
            path_denied: Band::new(0.25, 0.60),
            // Approve p95 0.07, true positives 0.91+. The one axis whose "no"
            // really is near zero.
            exposes_secrets: Band::new(0.15, 0.50),
            watch_for_hit: Band::new(0.25, 0.50),
            prefix_safe: Band::new(0.30, 0.80),
        }
    }
}

/// Which axis decided the stance — what the synthesized reason is written from.
/// Carried separately from the stance because the same stance arrives by several
/// routes and the session reading the reason needs to know which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Deciding {
    Secrets,
    OutsideGrants,
    DeniedPath,
    WatchFor,
    Scope,
    Irreversible,
    Clean,
}

/// Fold the axes into a stance (docs/permissions.md).
///
/// Ordered, first match wins, worst first: a move can fail several ways at once
/// and the reason should name the one a person would lead with. Every arm treats
/// *uncertain* as a reason to involve the human — [`Stance::Gate`] where the axis
/// would be disqualifying if true, [`Stance::Watch`] where it would only be worth
/// a beat.
pub(super) fn combine(axes: &Axes, t: &Thresholds, probe: bool) -> (Stance, Deciding) {
    // Exposing a secret is the one move no scope sanctions and no reversibility
    // argument rescues: the value is out the moment it is printed.
    if !t.exposes_secrets.is_false(axes.exposes_secrets) {
        return (Stance::Gate, Deciding::Secrets);
    }
    // The grants are the operator's own boundary, stated as data. Straying past
    // one is out of bounds regardless of how the move reads otherwise. A denied
    // carve-out is checked first: it is the more specific finding, and naming it
    // is more use to the reader than "outside the grants".
    if !t.path_denied.is_false(axes.path_denied) {
        return (Stance::Gate, Deciding::DeniedPath);
    }
    if !t.path_outside.is_false(axes.path_outside) {
        return (Stance::Gate, Deciding::OutsideGrants);
    }
    // A watch-for is the operator naming, in advance, the thing they want to see.
    // A clear hit stops; an uncertain one is worth the beat.
    if let Some(hit) = axes.watch_for_hit {
        if t.watch_for_hit.is_true(hit) {
            return (Stance::Gate, Deciding::WatchFor);
        }
        if !t.watch_for_hit.is_false(hit) {
            return (Stance::Watch, Deciding::WatchFor);
        }
    }
    if t.in_scope.is_false(axes.in_scope) {
        return (Stance::Gate, Deciding::Scope);
    }
    if !t.in_scope.is_true(axes.in_scope) {
        return (Stance::Watch, Deciding::Scope);
    }
    // In scope. What is left is permanence: an irreversible move needs the scope
    // to have sanctioned this kind of permanence, and "the scope is silent" is not
    // a sanction (docs/permissions.md: the reversibility default).
    let sanctioned = t.sanctioned.is_true(axes.sanctioned);
    if t.reversible.is_false(axes.reversible) && !sanctioned {
        return (Stance::Gate, Deciding::Irreversible);
    }
    if !t.reversible.is_true(axes.reversible) && !sanctioned {
        return (Stance::Watch, Deciding::Irreversible);
    }
    // Clear. The promotion safe-vote rides the stance (docs/permissions.md) and
    // takes the strict reading: only an explicit, confident yes promotes.
    match (probe, axes.prefix_safe) {
        (true, Some(safe)) if t.prefix_safe.is_true(safe) => (Stance::ClearAlways, Deciding::Clean),
        _ => (Stance::Clear, Deciding::Clean),
    }
}

/// The one-line reason, synthesized from the deciding axis and its number.
///
/// The decision model emits no prose, so this is the whole of what the acting
/// session and the human gate read. It states what the model actually reported,
/// with the figure attached — an assertion the reader can weigh, rather than a
/// confident sentence the model never wrote. The figures also put the deciding
/// axis in the `Spot` log entry, where the live survey can read it.
pub(super) fn reason(stance: Stance, deciding: Deciding, axes: &Axes) -> String {
    let pct = |p: f64| format!("{:.0}%", p * 100.0);
    match deciding {
        Deciding::Secrets => format!(
            "this looks like it would expose secret material ({} confidence) — no scope \
             sanctions that, so it stops here.",
            pct(axes.exposes_secrets)
        ),
        Deciding::OutsideGrants => format!(
            "this names a path outside the configured grants ({} confidence); the grants \
             are the perimeter, so it stops here.",
            pct(axes.path_outside)
        ),
        Deciding::DeniedPath => format!(
            "this names a path under a grant marked denied ({} confidence) — a carve-out \
             the operator made deliberately.",
            pct(axes.path_denied)
        ),
        Deciding::WatchFor => format!(
            "this matches something the config asked to be watched for ({} confidence).",
            pct(axes.watch_for_hit.unwrap_or_default())
        ),
        Deciding::Scope => format!(
            "scope fit reads at {} — {}",
            pct(axes.in_scope),
            match stance {
                Stance::Gate =>
                    "this looks outside the mission; find the in-scope route or \
                                 petition for the scope you need.",
                _ => "in-scope enough to be plausible, not enough to clear on its own.",
            }
        ),
        Deciding::Irreversible => format!(
            "reversibility reads at {} and nothing in the scope sanctions a move this \
             permanent ({} sanctioned){}",
            pct(axes.reversible),
            pct(axes.sanctioned),
            match stance {
                Stance::Gate => " — a one-way move needs the human.",
                _ => " — worth a beat before it runs.",
            }
        ),
        Deciding::Clean => format!(
            "in scope ({}) and reversible ({}){}",
            pct(axes.in_scope),
            pct(axes.reversible),
            match stance {
                Stance::ClearAlways => format!(
                    "; the offered prefix reads safe for any argument ({}).",
                    pct(axes.prefix_safe.unwrap_or_default())
                ),
                _ => ".".to_string(),
            }
        ),
    }
}
