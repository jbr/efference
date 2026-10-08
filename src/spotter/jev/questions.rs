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

use serde::{Deserialize, Serialize};

use crate::log::Stance;

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

/// One assessment's answers, as the model reported them: P(true) per axis, and the
/// sketch distribution. The two optional axes are asked only when the state
/// carries the thing they are about — a watch-for list, a promotion probe — so
/// `None` means "not asked", never "answered zero".
///
/// Recorded alongside the stance on `Spot::axes`. Entries written before the
/// sketch fold carry `reversible` and `sanctioned`, which are ignored on read, and
/// lack `sketch` and `precedent`, which default to `None`.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Axes {
    pub in_scope: f64,
    /// Names a path outside every grant. Split from [`Self::path_denied`]
    /// because one question asking both was four hops — resolve the move's
    /// paths, resolve the grants, check containment, check the denies — and
    /// scored 0.21–0.56 on plainly in-scope read-only commands.
    pub path_outside: f64,
    /// Names a path under a grant marked `denied` — the carve-out case, which
    /// is a different question from being outside every grant.
    pub path_denied: f64,
    pub exposes_secrets: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_for_hit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_safe: Option<f64>,
    /// P(level) for sketch levels 0 to 4, lowest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sketch: Option<Vec<f64>>,
    /// What the relation request found, when the fold reached the precedent step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precedent: Option<Precedent>,
}

impl Axes {
    /// One `name value` string per recorded figure, for the dev verbs that print a
    /// spot's whole vector.
    pub fn summary(&self) -> Vec<String> {
        let show = |label: &str, p: f64| format!("{label} {p:.2}");
        let mut parts = Vec::new();
        if let Some(sketch) = &self.sketch {
            let figures: Vec<String> = sketch.iter().map(|p| format!("{p:.2}")).collect();
            parts.push(format!("sketch {}", figures.join("/")));
        }
        parts.push(show("in_scope", self.in_scope));
        parts.push(show("path_outside", self.path_outside));
        parts.push(show("path_denied", self.path_denied));
        parts.push(show("exposes_secrets", self.exposes_secrets));
        parts.extend(self.watch_for_hit.map(|p| show("watch_for_hit", p)));
        parts.extend(self.prefix_safe.map(|p| show("prefix_safe", p)));
        if let Some(p) = &self.precedent {
            let required = p.required.map_or("never".to_string(), |n| n.to_string());
            parts.push(format!(
                "precedent same {} wider {} of {} (required {required})",
                p.same, p.wider, p.candidates
            ));
        }
        parts
    }
}

/// What the relation request read: of the `candidates` earlier calls asked about,
/// how many the model read as the `same` action and how many as `wider`, with the
/// number the move's level required.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Precedent {
    pub candidates: usize,
    pub same: usize,
    pub wider: usize,
    /// `None` when no number of precedents clears the level.
    pub required: Option<usize>,
    /// Each candidate's reading, in the order asked. The counts above are these
    /// under the fold's cut; recording the figures is what lets a different cut be
    /// replayed, and a surprising clear be traced to the approved call it matched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compared: Vec<Compared>,
}

/// One candidate's reading: the earlier call, by id, and the mean of its two
/// askings for `same` and for `wider`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Compared {
    pub call_id: crate::log::CallId,
    pub same: f64,
    pub wider: f64,
}

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
    Raised(&'static str, f64),
}

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
    let bumps: [(&'static str, usize, Option<f64>); 5] = [
        (
            "a denied path",
            3,
            Some(axes.path_denied).filter(|p| *p > 0.25),
        ),
        (
            "possible secrets",
            3,
            Some(axes.exposes_secrets).filter(|p| *p > 0.15),
        ),
        (
            "a watch-for match",
            3,
            axes.watch_for_hit.filter(|p| *p > 0.25),
        ),
        (
            "outside the grants",
            2,
            Some(axes.path_outside).filter(|p| *p > 0.35),
        ),
        ("scope fit", 2, Some(axes.in_scope).filter(|p| *p <= 0.30)),
    ];
    for (label, to, figure) in bumps {
        if let Some(figure) = figure
            && to > level
        {
            level = to;
            cause = Cause::Raised(label, figure);
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
        Cause::Raised(label, p) => format!("level {level} ({label}, {})", pct(p)),
    }
}
