//! What the decision model's answers record, and how they become a [`Stance`]
//! (docs/permissions.md).
//!
//! The prose spotter is asked one question — "what is your stance?" — and does
//! the whole judgement inside the model. A decision model cannot: it answers a
//! fixed question with a probability and says nothing else. So the judgement is
//! split into a sketchiness reading and the operator's own history, and the
//! combination back into a stance happens here, in code:
//!
//! ```text
//!   sketch level (raised by a tripped axis)  ×  approved calls like this one  ──▶  Clear | Gate
//! ```
//!
//! The rule is inspectable — the stance is a function of numbers that are all
//! recorded in [`Axes`] — and it never returns `Watch`: how many approved calls a
//! level needs ([`REQUIRED`]) is the dial between asking and not asking.

use crate::record::log::{Axes, Stance};

/// The question keys, as they go on the wire and come back in `answers`.
pub(super) const SKETCH: &str = "sketch";
pub(super) const IN_SCOPE: &str = "in_scope";
pub(super) const PATH_OUTSIDE: &str = "path_outside";
pub(super) const PATH_DENIED: &str = "path_denied";
pub(super) const EXPOSES_SECRETS: &str = "exposes_secrets";
pub(super) const WATCH_FOR_HIT: &str = "watch_for_hit";
pub(super) const PREFIX_SAFE: &str = "prefix_safe";

/// How many levels the `sketch` Score has.
pub(super) const LEVELS: usize = 5;

/// How many approved same-action calls each sketch level needs before the move
/// clears; `None` is a level no history clears.
pub(super) const REQUIRED: [Option<usize>; LEVELS] = [Some(0), Some(0), Some(1), Some(2), None];

/// `exposes_secrets` at or above this gates, whatever the history says.
const SECRETS_GATE: f64 = 0.5;
/// A relation reading (`same`, `wider`) counts at or above this.
pub(super) const RELATION_COUNTS: f64 = 0.5;
/// `prefix_safe` at or above this lets a clear carry the promotion safe-vote.
const PREFIX_VOUCH: f64 = 0.8;

/// The relation request's reading, before the fold has said how many were needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) struct Reading {
    pub candidates: usize,
    pub same: usize,
    pub wider: usize,
}

/// The fold's result: the stance, the sentence the session and the human gate
/// read, and the requirement it applied.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Decision {
    pub stance: Stance,
    pub reason: String,
    pub required: Option<usize>,
}

/// What put a move at its level: the sketch's own reading, or an axis that
/// raised it past the sketch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Cause {
    Sketch(Option<f64>),
    Raised(Axis, f64),
}

/// A shipping axis that raises a move's level past the sketch's reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Axis {
    DeniedPath,
    Secrets,
    WatchFor,
    OutsideGrants,
    ScopeFit,
}

impl Axis {
    /// The term the rung's own reason names it by ([`Decision::reason`]).
    fn term(self) -> &'static str {
        match self {
            Self::DeniedPath => "a denied path",
            Self::Secrets => "possible secrets",
            Self::WatchFor => "a watch-for match",
            Self::OutsideGrants => "outside the grants",
            Self::ScopeFit => "scope fit",
        }
    }

    /// What the tripped axis says about the move, for whoever decides the gate
    /// ([`plain_reason`]).
    fn plain(self) -> &'static str {
        match self {
            Self::DeniedPath => "it names a path this session is denied",
            Self::Secrets => "it could expose secret material",
            Self::WatchFor => "it may match something the operator asked to watch for",
            Self::OutsideGrants => "it reaches outside the paths this session is granted",
            Self::ScopeFit => "its reviewer is unsure this fits the session's scope",
        }
    }
}

/// What the sketch's own level says about a move, for whoever decides the gate,
/// lowest first — the levels `ask.rs` describes to the model, without their numbers.
const SKETCH_PLAIN: [&str; LEVELS] = [
    "its reviewer rates it routine",
    "its reviewer rates it mild",
    "its reviewer rates it notable, a move to understand before it runs",
    "its reviewer rates it serious: hard to undo, or reaching other people",
    "its reviewer rates it severe: it could expose a credential or destroy data",
];

/// The move's sketchiness level and what set it.
pub(super) fn level_of(axes: &Axes) -> (usize, Cause) {
    let (mut level, mut cause) = match axes.sketch.as_deref() {
        Some(sketch) if !sketch.is_empty() => {
            let (level, p) =
                sketch
                    .iter()
                    .copied()
                    .enumerate()
                    .fold(
                        (0, f64::MIN),
                        |best, (i, p)| if p > best.1 { (i, p) } else { best },
                    );
            (level, Cause::Sketch(Some(p)))
        }
        _ => (0, Cause::Sketch(None)),
    };
    // A tripped shipping axis raises the level to at least the one it names.
    let bumps: [(Axis, usize, Option<f64>); 5] = [
        (
            Axis::DeniedPath,
            3,
            Some(axes.path_denied).filter(|p| *p > 0.25),
        ),
        (
            Axis::Secrets,
            3,
            Some(axes.exposes_secrets).filter(|p| *p > 0.15),
        ),
        (Axis::WatchFor, 3, axes.watch_for_hit.filter(|p| *p > 0.25)),
        (
            Axis::OutsideGrants,
            2,
            Some(axes.path_outside).filter(|p| *p > 0.35),
        ),
        (
            Axis::ScopeFit,
            2,
            Some(axes.in_scope).filter(|p| *p <= 0.30),
        ),
    ];
    for (axis, to, figure) in bumps {
        if let Some(figure) = figure
            && to > level
        {
            level = to;
            cause = Cause::Raised(axis, figure);
        }
    }
    (level.min(LEVELS - 1), cause)
}

/// Fold the answers into a stance (docs/permissions.md). `precedent` is `None`
/// when no relation reading exists (no candidates, or the request failed), which
/// folds as zero precedents. `probe` is whether a promotion probe rode the call.
pub(super) fn decide(axes: &Axes, precedent: Option<Reading>, probe: bool) -> Decision {
    let (level, cause) = level_of(axes);
    let required = REQUIRED[level];
    let reading = precedent.unwrap_or_default();
    // Exposing a secret is the one move no history sanctions: the value is out
    // the moment it is printed.
    if axes.exposes_secrets >= SECRETS_GATE {
        return Decision {
            stance: Stance::Gate,
            reason: format!(
                "this looks like it would expose secret material ({} confidence) — no earlier \
                 approval clears that, so it stops here.",
                pct(axes.exposes_secrets)
            ),
            required,
        };
    }
    let lead = lead(level, cause);
    let further = match reading.wider {
        0 => String::new(),
        n => format!("; goes further than {}", approved_calls(n)),
    };
    let cleared = required.is_some_and(|need| reading.same >= need);
    if !cleared {
        let needs = match required {
            Some(need) => format!(
                "needs {need} approved {} like it, found {}",
                plural(need, "call"),
                match reading.same {
                    0 => "none".to_string(),
                    n => n.to_string(),
                }
            ),
            None => "no number of approved calls clears this level".to_string(),
        };
        return Decision {
            stance: Stance::Gate,
            reason: format!("{lead}: {needs}{further}."),
            required,
        };
    }
    let matched = match reading.same {
        0 => String::new(),
        n => format!("; matches {}", approved_calls_earlier(n)),
    };
    let (stance, vouch) = match axes.prefix_safe.filter(|p| probe && *p >= PREFIX_VOUCH) {
        Some(safe) => (
            Stance::ClearAlways,
            format!(
                "; the offered prefix reads safe for any argument ({})",
                pct(safe)
            ),
        ),
        None => (Stance::Clear, String::new()),
    };
    Decision {
        stance,
        reason: format!("{lead}{matched}{further}{vouch}."),
        required,
    }
}

/// Why the decision rung held a move, in plain words for whoever decides the gate
/// (docs/permissions.md): what put it at its level, then what the session's allowed
/// calls said. Phrased from the recorded answers — the axes and the precedent
/// reading `Spot::axes` carries — never from [`Decision::reason`], whose levels and
/// figures are the rung's own terms. `None` when the answers fold to a clear.
///
/// An entry without a precedent reading (the relation request failed) folded as no
/// precedents, under the requirement its level sets; the history clause is left out,
/// since there is no reading to report.
pub fn plain_reason(axes: &Axes) -> Option<String> {
    if axes.exposes_secrets >= SECRETS_GATE {
        return Some("it may expose secret material".to_string());
    }
    let (level, cause) = level_of(axes);
    let precedent = axes.precedent.as_ref();
    // The requirement the fold applied is the recorded one; a different table
    // since would otherwise re-judge the call it held.
    let required = precedent.map_or(REQUIRED[level], |p| p.required);
    let same = precedent.map_or(0, |p| p.same);
    if required.is_some_and(|need| same >= need) {
        return None;
    }
    let mut clauses = vec![match cause {
        Cause::Raised(axis, _) => axis.plain().to_string(),
        Cause::Sketch(_) => SKETCH_PLAIN[level].to_string(),
    }];
    match (required, precedent) {
        (None, _) => clauses.push("no earlier approval clears a call this risky".to_string()),
        (Some(_), None) => {}
        (Some(_), Some(p)) if p.same == 0 => {
            clauses.push("no earlier call like it was allowed".to_string());
        }
        (Some(need), Some(p)) => clauses.push(format!(
            "only {} earlier {} like it {} allowed, and a call like this needs {need}",
            p.same,
            plural(p.same, "call"),
            if p.same == 1 { "was" } else { "were" },
        )),
    }
    if let Some(p) = precedent.filter(|p| p.wider > 0) {
        clauses.push(format!(
            "it goes further than {} {} allowed earlier",
            p.wider,
            plural(p.wider, "call")
        ));
    }
    Some(clauses.join("; "))
}

fn pct(p: f64) -> String {
    format!("{:.0}%", p * 100.0)
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}

/// "2 calls the operator approved".
fn approved_calls(n: usize) -> String {
    format!("{n} {} the operator approved", plural(n, "call"))
}

/// "2 earlier calls the operator approved".
fn approved_calls_earlier(n: usize) -> String {
    format!("{n} earlier {} the operator approved", plural(n, "call"))
}

/// The level and what set it: "routine (level 0)", or "level 2 (outside the
/// grants, 62%)" with the figure of the axis (or the sketch) behind it.
fn lead(level: usize, cause: Cause) -> String {
    if level == 0 {
        return "routine (level 0)".to_string();
    }
    match cause {
        Cause::Sketch(Some(p)) => format!("level {level} (sketch, {})", pct(p)),
        Cause::Sketch(None) => format!("level {level}"),
        Cause::Raised(axis, p) => format!("level {level} ({}, {})", axis.term(), pct(p)),
    }
}
