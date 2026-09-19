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
pub(super) const OUTSIDE_GRANTS: &str = "outside_grants";
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
    pub outside_grants: f64,
    pub exposes_secrets: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_for_hit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_safe: Option<f64>,
}

/// Where the band between "no" and "yes" sits. Two knobs rather than one per axis:
/// the axes are all P(true) on comparable questions, and a per-axis table is a
/// tuning surface with no evidence behind it yet. A sweep that shows one axis
/// wants its own band is what earns the extra field.
///
/// The band is deliberately wide. A probability inside it reads as *uncertain*, and
/// uncertainty resolves toward the human — the error weighting for this rung is
/// asymmetric (backlog/spotter-eval.md: an allowed-when-it-shouldn't-have is worse
/// than a gated-when-it-was-fine), so the middle belongs to the gate, not to the
/// clear.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    /// At or above this, an axis reads as true.
    pub high: f64,
    /// At or below this, an axis reads as false.
    pub low: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            high: 0.80,
            low: 0.20,
        }
    }
}

impl Thresholds {
    fn is_true(&self, p: f64) -> bool {
        p >= self.high
    }

    fn is_false(&self, p: f64) -> bool {
        p <= self.low
    }
}

/// Which axis decided the stance — what the synthesized reason is written from.
/// Carried separately from the stance because the same stance arrives by several
/// routes and the session reading the reason needs to know which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Deciding {
    Secrets,
    Grants,
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
    if !t.is_false(axes.exposes_secrets) {
        return (Stance::Gate, Deciding::Secrets);
    }
    // The grants are the operator's own boundary, stated as data. Straying past
    // one is out of bounds regardless of how the move reads otherwise.
    if !t.is_false(axes.outside_grants) {
        return (Stance::Gate, Deciding::Grants);
    }
    // A watch-for is the operator naming, in advance, the thing they want to see.
    // A clear hit stops; an uncertain one is worth the beat.
    if let Some(hit) = axes.watch_for_hit {
        if t.is_true(hit) {
            return (Stance::Gate, Deciding::WatchFor);
        }
        if !t.is_false(hit) {
            return (Stance::Watch, Deciding::WatchFor);
        }
    }
    if t.is_false(axes.in_scope) {
        return (Stance::Gate, Deciding::Scope);
    }
    if !t.is_true(axes.in_scope) {
        return (Stance::Watch, Deciding::Scope);
    }
    // In scope. What is left is permanence: an irreversible move needs the scope
    // to have sanctioned this kind of permanence, and "the scope is silent" is not
    // a sanction (docs/permissions.md: the reversibility default).
    let sanctioned = t.is_true(axes.sanctioned);
    if t.is_false(axes.reversible) && !sanctioned {
        return (Stance::Gate, Deciding::Irreversible);
    }
    if !t.is_true(axes.reversible) && !sanctioned {
        return (Stance::Watch, Deciding::Irreversible);
    }
    // Clear. The promotion safe-vote rides the stance (docs/permissions.md) and
    // takes the strict reading: only an explicit, confident yes promotes.
    match (probe, axes.prefix_safe) {
        (true, Some(safe)) if t.is_true(safe) => (Stance::ClearAlways, Deciding::Clean),
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
        Deciding::Grants => format!(
            "this reads as reaching a path outside the configured grants ({} confidence); \
             the grants are the perimeter, so it stops here.",
            pct(axes.outside_grants)
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
