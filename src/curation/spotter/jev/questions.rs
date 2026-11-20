//! What the decision model's answers mean, and how they become a [`Stance`]
//! (docs/permissions.md).
//!
//! The prose spotter is asked one question — "what is your stance?" — and does the
//! whole judgement inside the model. A decision model answers a fixed question with a
//! probability and says nothing else, so the judgement is split into a battery of
//! yes/no questions, and the combination back into a stance happens here, in code:
//!
//! ```text
//!   some reason to clear  ×  no hazard  ×  (inside the grants, or asked for)  ──▶  Clear | Gate
//! ```
//!
//! Each question is a reason to clear (an *allow-reason*: the operator asked for it,
//! agreed to it, it is contained, it only reads, a note calls it ordinary) or a
//! *hazard* (it goes further than asked, against a limit, prints a secret, is one-way,
//! edits its own permissions, is opaque, a note warns against it). The rule is
//! inspectable — the stance is a function of numbers that are all recorded in
//! [`Axes`] — and it never returns `Watch`.

use crate::record::log::{Axes, Stance};

use super::ask::BATTERY;

/// The battery's keys, as they go on the wire and come back in `answers`, spelled as they
/// were asked when the thresholds were fitted.
pub(super) const ASKED: &str = "asked";
pub(super) const AGREED: &str = "agreed";
pub(super) const CONTAINED: &str = "contained";
pub(super) const INSPECTS: &str = "inspects";
pub(super) const NOTED_ORDINARY: &str = "noted-ordinary";
pub(super) const FURTHER: &str = "further";
pub(super) const NARROWED: &str = "narrowed";
pub(super) const SECRET: &str = "secret";
pub(super) const ONE_WAY: &str = "one-way";
pub(super) const SELF_MODIFIES: &str = "self-modifies";
pub(super) const OPAQUE: &str = "opaque";
pub(super) const NOTE_WARNS: &str = "note-warns";
/// The promotion question, asked once beside the battery when a probe is present.
pub(super) const PREFIX_SAFE: &str = "prefix_safe";

/// An allow-reason reading at or above this is a reason to clear (docs/permissions.md).
/// Fitted with [`HAZARD_HOLDS`] on labelled corpus gates and validated on real gates
/// with must-hold moves transplanted into their sessions.
pub(super) const ALLOW_COUNTS: f64 = 0.50;
/// A hazard reading at or above this holds the move, whatever reason to clear it has
/// (docs/permissions.md).
pub(super) const HAZARD_HOLDS: f64 = 0.625;
/// `prefix_safe` at or above this lets a clear carry the promotion safe-vote.
const PREFIX_VOUCH: f64 = 0.8;

/// Which way a battery question's yes points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    /// A reason to clear.
    Allow,
    /// A reason to hold.
    Hazard,
}

/// The fold's result: the stance and the sentence the acting agent and the human gate
/// read.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Decision {
    pub stance: Stance,
    pub reason: String,
}

/// What held a move.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Held {
    /// This hazard, the strongest at or over [`HAZARD_HOLDS`].
    Hazard(&'static str, f64),
    /// The move reaches outside the grants and neither `asked` nor `agreed` counted.
    OutsideUnasked,
    /// No allow-reason counted; the strongest one, when any was read.
    NoReason(Option<(&'static str, f64)>),
}

/// The asked questions on `side` with their readings, in battery order.
fn read_side(axes: &Axes, side: Side) -> impl Iterator<Item = (&'static str, f64)> + '_ {
    BATTERY
        .iter()
        .filter(move |question| question.side == side)
        .filter_map(|question| {
            axes.readings
                .get(question.key)
                .map(|reading| (question.key, reading.reading))
        })
}

/// The strongest of `readings`, the earliest on a tie.
fn strongest(readings: impl Iterator<Item = (&'static str, f64)>) -> Option<(&'static str, f64)> {
    readings.reduce(|best, next| if next.1 > best.1 { next } else { best })
}

/// The fold (docs/permissions.md): clear when some allow-reason counts and no hazard
/// holds, and, for a move outside the grants, only when `asked` or `agreed` counts.
/// `Ok` is the allow-reason the move cleared on, with its reading.
fn fold(axes: &Axes) -> Result<(&'static str, f64), Held> {
    if let Some((key, reading)) =
        strongest(read_side(axes, Side::Hazard).filter(|(_, reading)| *reading >= HAZARD_HOLDS))
    {
        return Err(Held::Hazard(key, reading));
    }
    let allowed = match strongest(read_side(axes, Side::Allow)) {
        Some(best) if best.1 >= ALLOW_COUNTS => best,
        best => return Err(Held::NoReason(best)),
    };
    if !axes.outside_grants {
        return Ok(allowed);
    }
    // Outside the grants, only what the operator or a supervisor said clears: a move
    // can be contained in its effects and still reach somewhere nobody granted.
    match strongest(read_side(axes, Side::Allow).filter(|(key, _)| [ASKED, AGREED].contains(key))) {
        Some(asked) if asked.1 >= ALLOW_COUNTS => Ok(asked),
        _ => Err(Held::OutsideUnasked),
    }
}

/// Fold the recorded answers into a stance and its reason (docs/permissions.md).
/// `probe` is whether a promotion probe was asked: a clear with `prefix_safe` at or
/// above [`PREFIX_VOUCH`] is a `ClearAlways`.
pub(super) fn decide(axes: &Axes, probe: bool) -> Decision {
    match fold(axes) {
        Ok((key, reading)) => {
            let reason = format!("clear: {} ({reading:.2})", allow_clause(key));
            match axes.prefix_safe.filter(|p| probe && *p >= PREFIX_VOUCH) {
                Some(safe) => Decision {
                    stance: Stance::ClearAlways,
                    reason: format!(
                        "{reason}, and the probed prefix reads safe for any argument ({safe:.2})"
                    ),
                },
                None => Decision {
                    stance: Stance::Clear,
                    reason,
                },
            }
        }
        Err(held) => Decision {
            stance: Stance::Gate,
            reason: match held {
                Held::Hazard(key, reading) => {
                    format!("held: {} ({reading:.2})", hazard_clause(key))
                }
                Held::NoReason(Some((key, reading))) => format!(
                    "held: no reason to clear it was clear enough (best: {key} {reading:.2})"
                ),
                held => format!("held: {}", held_clause(held)),
            },
        },
    }
}

/// Why the decision rung held a move, in plain words for whoever decides the gate
/// (docs/permissions.md): the clause [`decide`]'s reason names, with no figures.
/// Phrased by re-folding the recorded answers, never by rewriting `Spot::reason`.
/// `None` when the answers fold to a clear, and for an entry recorded under an earlier
/// fold, which has no readings to fold.
pub fn plain_reason(axes: &Axes) -> Option<String> {
    if axes.readings.is_empty() {
        return None;
    }
    fold(axes).err().map(held_clause)
}

/// What held the move, as a clause.
fn held_clause(held: Held) -> String {
    match held {
        Held::Hazard(key, _) => hazard_clause(key).to_string(),
        Held::OutsideUnasked => "it reaches outside the granted paths, and nothing the operator \
                                 or a supervising agent said asks for it"
            .to_string(),
        Held::NoReason(_) => {
            "it was not clearly asked for, agreed to, contained, or read-only".to_string()
        }
    }
}

/// What an allow-reason says about the move it cleared.
fn allow_clause(key: &str) -> &'static str {
    match key {
        ASKED => "the operator or a supervising agent asked for this",
        AGREED => "the operator agreed to this when the agent proposed it",
        CONTAINED => {
            "it stays inside the granted paths, where git or the scratch directory can undo it"
        }
        INSPECTS => "it only reads or inspects",
        NOTED_ORDINARY => "a spotter note says moves like this are ordinary here",
        _ => "a reason to clear it counted",
    }
}

/// What a hazard says about the move it held.
fn hazard_clause(key: &str) -> &'static str {
    match key {
        FURTHER => "it goes further than anything the operator or a supervising agent asked for",
        NARROWED => {
            "the operator or a supervising agent ruled out moves like this, or limited the task \
             in a way this goes against"
        }
        SECRET => "it would print, store or send a secret",
        ONE_WAY => {
            "it has an effect git or the scratch directory cannot undo, which nobody asked for"
        }
        SELF_MODIFIES => "it changes the configuration or hooks that govern what this agent may do",
        OPAQUE => "it runs code whose effect cannot be seen from the move itself",
        NOTE_WARNS => "a spotter note warns against moves like this",
        _ => "a hazard question held it",
    }
}
