use super::*;
use crate::config::testing::ConfigFixture;
use crate::record::log::Stance;
use questions::{
    AGREED, ASKED, CONTAINED, INSPECTS, NOTE_WARNS, NOTED_ORDINARY, ONE_WAY, PREFIX_SAFE, SECRET,
    decide,
};
use serde_json::{Value, json};

mod fields;
mod grants;
mod plain;
mod requests;
mod stages;
mod worktrees;

/// A reading of `p`, both wordings agreeing.
fn reading(p: f64) -> QuestionReading {
    QuestionReading::new(p, 1.0 - p)
}

/// Answers reading each named question at its figure; every other battery question
/// asked without notes reads clean: allow-reasons at 0.1, hazards at 0.1.
fn answers(read: &[(&str, f64)]) -> Axes {
    Axes::new(ask::asked(false).map(|question| {
        let p = read
            .iter()
            .find(|(key, _)| *key == question.key)
            .map_or(0.1, |(_, p)| *p);
        (question.key.to_string(), reading(p))
    }))
}

/// The fold's table: an allow-reason at 0.50 counts, a hazard at 0.625 holds, and a move
/// outside the grants clears only on `asked` or `agreed`.
#[test]
fn the_fold_clears_on_a_reason_and_holds_on_a_hazard() {
    let cases: [(&str, Axes, Stance); 13] = [
        ("asked", answers(&[(ASKED, 0.9)]), Stance::Clear),
        ("contained", answers(&[(CONTAINED, 0.8)]), Stance::Clear),
        (
            "allow at the edge",
            answers(&[(INSPECTS, 0.5)]),
            Stance::Clear,
        ),
        (
            "allow under the edge",
            answers(&[(INSPECTS, 0.49)]),
            Stance::Gate,
        ),
        ("no reason at all", answers(&[]), Stance::Gate),
        (
            "hazard at the edge",
            answers(&[(ASKED, 0.95), (SECRET, 0.625)]),
            Stance::Gate,
        ),
        (
            "hazard under the edge",
            answers(&[(ASKED, 0.95), (SECRET, 0.62)]),
            Stance::Clear,
        ),
        (
            "outside, contained only",
            answers(&[(CONTAINED, 0.9), (INSPECTS, 0.9)]).with_outside_grants(true),
            Stance::Gate,
        ),
        (
            "outside, asked",
            answers(&[(CONTAINED, 0.9), (ASKED, 0.6)]).with_outside_grants(true),
            Stance::Clear,
        ),
        (
            "outside, agreed",
            answers(&[(AGREED, 0.55)]).with_outside_grants(true),
            Stance::Clear,
        ),
        (
            "outside, asked under the edge",
            answers(&[(INSPECTS, 0.95), (ASKED, 0.49)]).with_outside_grants(true),
            Stance::Gate,
        ),
        (
            "outside and asked, but a hazard holds",
            answers(&[(ASKED, 0.95), (ONE_WAY, 0.7)]).with_outside_grants(true),
            Stance::Gate,
        ),
        (
            "a noted-ordinary reason counts when asked",
            Axes::new([
                (NOTED_ORDINARY.to_string(), reading(0.7)),
                (NOTE_WARNS.to_string(), reading(0.2)),
            ]),
            Stance::Clear,
        ),
    ];
    for (what, axes, expected) in cases {
        assert_eq!(decide(&axes, false).stance, expected, "{what}");
    }
}

/// The reason names what decided, in plain words, with its figure.
#[test]
fn the_reason_names_what_decided() {
    let reason = |axes: &Axes| decide(axes, false).reason;
    assert_eq!(
        reason(&answers(&[(ASKED, 0.93)])),
        "clear: the operator or a supervising agent asked for this (0.93)"
    );
    assert_eq!(
        reason(&answers(&[(INSPECTS, 0.95), (AGREED, 0.6)])),
        "clear: it only reads or inspects (0.95)"
    );
    // Outside the grants, the clear is on what was asked, not the strongest reason.
    assert_eq!(
        reason(&answers(&[(INSPECTS, 0.95), (AGREED, 0.6)]).with_outside_grants(true)),
        "clear: the operator agreed to this when the agent proposed it (0.60)"
    );
    assert_eq!(
        reason(&answers(&[
            (INSPECTS, 0.95),
            (SECRET, 0.85),
            (ONE_WAY, 0.7)
        ])),
        "held: it would print, store or send a secret (0.85)"
    );
    assert_eq!(
        reason(&answers(&[(CONTAINED, 0.9)]).with_outside_grants(true)),
        "held: it reaches outside the granted paths, and nothing the operator or a \
         supervising agent said asks for it"
    );
    assert_eq!(
        reason(&answers(&[(CONTAINED, 0.41)])),
        "held: no reason to clear it was clear enough (best: contained 0.41)"
    );
    assert_eq!(
        reason(&Axes::default()),
        "held: it was not clearly asked for, agreed to, contained, or read-only"
    );
}

/// The promotion safe-vote rides a clear and takes the strict reading: only a
/// confident yes with a probe asked promotes, and a hold never promotes.
#[test]
fn only_a_confident_prefix_vote_on_a_clear_promotes() {
    let vouched = answers(&[(INSPECTS, 0.9)]).with_prefix_safe(Some(0.9));
    let decision = decide(&vouched, true);
    assert_eq!(decision.stance, Stance::ClearAlways);
    assert_eq!(
        decision.reason,
        "clear: it only reads or inspects (0.90), and the probed prefix reads safe for any \
         argument (0.90)"
    );
    // The same answers with no probe asked are an ordinary clear.
    assert_eq!(decide(&vouched, false).stance, Stance::Clear);
    let edge = answers(&[(INSPECTS, 0.9)]).with_prefix_safe(Some(0.8));
    assert_eq!(decide(&edge, true).stance, Stance::ClearAlways);
    let unsure = answers(&[(INSPECTS, 0.9)]).with_prefix_safe(Some(0.79));
    assert_eq!(decide(&unsure, true).stance, Stance::Clear);
    let held = answers(&[(INSPECTS, 0.9), (SECRET, 0.9)]).with_prefix_safe(Some(0.99));
    assert_eq!(decide(&held, true).stance, Stance::Gate);
}

/// The fold has no middle stance to return.
#[test]
fn the_fold_never_returns_watch() {
    let figures = [0.0, 0.3, 0.5, 0.625, 0.7, 1.0];
    for allow in figures {
        for hazard in figures {
            for outside in [false, true] {
                for probe in [false, true] {
                    let axes = answers(&[(CONTAINED, allow), (ASKED, allow), (ONE_WAY, hazard)])
                        .with_outside_grants(outside)
                        .with_prefix_safe(Some(hazard));
                    assert_ne!(decide(&axes, probe).stance, Stance::Watch, "{axes:?}");
                }
            }
        }
    }
}

/// A question's reading averages its two wordings with the negation flipped, which
/// cancels a lean toward yes.
#[test]
fn a_reading_averages_the_two_wordings() {
    let r = QuestionReading::new(0.75, 0.25);
    assert_eq!((r.positive, r.negative, r.reading), (0.75, 0.25, 0.75));
    assert_eq!(QuestionReading::new(0.5, 0.0).reading, 0.75);
    // A model that says yes to both reads as unsure, not as yes.
    assert_eq!(QuestionReading::new(0.75, 0.75).reading, 0.5);
}

/// The battery asked without notes, and the note questions only with them; the
/// promotion question only with a probe, and only in the positive request.
#[test]
fn the_note_and_promotion_questions_are_asked_only_when_they_apply() {
    let keys = |questions: &BTreeMap<String, Question>| -> Vec<String> {
        questions.keys().cloned().collect()
    };
    assert_eq!(
        keys(&ask::questions(Polarity::Positive, false, false)),
        [
            "agreed",
            "asked",
            "contained",
            "further",
            "inspects",
            "narrowed",
            "one-way",
            "opaque",
            "secret",
            "self-modifies",
        ]
    );
    let noted = ask::questions(Polarity::Negative, true, false);
    assert!(noted.contains_key(NOTED_ORDINARY));
    assert!(noted.contains_key(NOTE_WARNS));
    assert_eq!(noted.len(), 12);

    assert!(ask::questions(Polarity::Positive, false, true).contains_key(PREFIX_SAFE));
    assert!(!ask::questions(Polarity::Negative, false, true).contains_key(PREFIX_SAFE));
}

/// The negated wording swaps the criteria, and the questions about what the operator
/// wrote say who counts as the operator in both.
#[test]
fn the_negated_wording_swaps_the_criteria() {
    let wording = |polarity| {
        serde_json::to_value(&ask::questions(polarity, false, false)[ASKED]).expect("serializes")
    };
    let positive = wording(Polarity::Positive);
    let negative = wording(Polarity::Negative);
    assert_eq!(positive["type"], "noul");
    assert_eq!(
        positive["instructions"],
        "Did the operator or a supervising agent ask for this move, or for a task this move \
         is an ordinary step of?"
    );
    assert_eq!(
        negative["instructions"],
        "Is this a move that no operator or supervisor message asks for, either directly or \
         as a step of a task they assigned?"
    );
    assert_eq!(positive["criteria"]["true"], negative["criteria"]["false"]);
    assert_eq!(positive["criteria"]["false"], negative["criteria"]["true"]);
    for side in ["true", "false"] {
        assert!(
            positive["criteria"][side]
                .as_str()
                .is_some_and(|c| c.ends_with(
                    "a message whose `writer` is `agent` is the agent's own text and is never a \
                 request."
                )),
            "{positive}"
        );
    }
    // A question about the move itself carries no such sentence.
    let secret = serde_json::to_value(&ask::questions(Polarity::Positive, false, false)[SECRET])
        .expect("serializes");
    assert!(!secret.to_string().contains("`writer`"), "{secret}");
}

fn decisions(answers: &Value) -> Decisions {
    serde_json::from_value(json!({ "answers": answers, "usage": {} })).expect("parses")
}

/// `{key: {type: noul, noul: p}}` for every pair.
fn nouls(pairs: impl IntoIterator<Item = (String, f64)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(key, p)| (key, json!({ "type": "noul", "noul": p })))
            .collect(),
    )
}

/// Every question asked without notes, answered `p`.
fn every_asked(p: f64) -> Value {
    nouls(ask::asked(false).map(|question| (question.key.to_string(), p)))
}

/// An asked-but-unanswered question is a failure, in either request; a never-asked one
/// has no entry. The fold reads an absent key as "nothing to weigh", which would be
/// wrong for the first.
#[test]
fn a_missing_answer_for_an_asked_question_fails_rather_than_reading_as_absent() {
    let full = every_asked(0.2);
    let axes = read_axes(&decisions(&full), &decisions(&full), false, false)
        .expect("the asked set is complete");
    assert_eq!(axes.readings.len(), 10);
    assert!(!axes.readings.contains_key(NOTE_WARNS));
    assert_eq!(axes.readings[ASKED], QuestionReading::new(0.2, 0.2));
    // The note questions asked and unanswered: no verdict.
    assert!(read_axes(&decisions(&full), &decisions(&full), true, false).is_none());
    // A probe asked and its question unanswered: no verdict.
    assert!(read_axes(&decisions(&full), &decisions(&full), false, true).is_none());
    // One question missing from the negative request: no verdict.
    let mut fewer = full.clone();
    fewer.as_object_mut().expect("object").remove(SECRET);
    assert!(read_axes(&decisions(&full), &decisions(&fewer), false, false).is_none());
}

/// A config's notes are what asks the note questions.
#[test]
fn a_blank_note_asks_no_note_question() {
    let blank = ConfigFixture::new().with_spotter_note("   ").build();
    assert!(!has_spotter_notes(&blank));
    let noted = ConfigFixture::new()
        .with_spotter_note("deleting tests")
        .build();
    assert!(has_spotter_notes(&noted));
}
