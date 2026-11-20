//! The two requests an assessment sends, against a scripted decisions endpoint: their
//! shape, the averaging of their answers, and the fail-closed paths.

use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};
use crate::record::log::{Provider, WireDialect};
use crate::record::utterances::{Speaker, Utterance, Utterances};

/// The decisions endpoint `upstream` answers on.
pub(super) fn endpoint(upstream: &ScriptedUpstream) -> Endpoint {
    Endpoint::without_credential(
        Provider::Custom(WireDialect::SystemOne),
        ScriptedUpstream::url("/v1/decisions"),
    )
    .with_http(upstream.http())
}

/// A response answering each of `answers`, with usage.
pub(super) fn response(answers: &Value) -> Canned {
    Canned::json(
        200,
        json!({
            "model": "jev",
            "answers": answers,
            "usage": { "input_tokens": 10, "output_tokens": 0 },
        })
        .to_string(),
    )
}

/// Answers to every question asked without notes: `asked` at `asked`, the rest at 0.5.
/// Each request is answered the same way whichever wording it asks, so a reading is
/// 0.5 unless the two answers to it differ.
pub(super) fn answered(asked: f64) -> Value {
    nouls(ask::asked(false).map(|question| {
        let p = if question.key == ASKED { asked } else { 0.5 };
        (question.key.to_string(), p)
    }))
}

fn said() -> Utterances {
    Utterances {
        id: "t".into(),
        said: vec![
            Utterance::new(Speaker::Operator, "list the sources"),
            Utterance::new(Speaker::Agent, "Listing them."),
        ],
    }
}

fn call() -> ToolCall {
    ToolCall::new(
        crate::record::names::tools::BASH,
        "c1",
        json!({ "command": "ls src" }),
    )
}

/// Which of the two requests the upstream read was the positive one: the one asking
/// `asked` as written.
fn positive_first(upstream: &ScriptedUpstream) -> bool {
    let requests = upstream.requests();
    assert_eq!(requests.len(), 2, "{}", upstream.describe());
    requests[0].json()["questions"][ASKED]["instructions"]
        .as_str()
        .is_some_and(|text| text.starts_with("Did the operator"))
}

/// The two requests carry one state — with the conversation and the guidance — and the
/// battery in each wording; each question's reading is the average of its two answers
/// with the negation flipped, and the spot's usage is both requests'.
#[tokio::test]
async fn both_wordings_are_asked_of_one_state_and_averaged() {
    // Answered in arrival order: whichever request arrives first is told `asked` is
    // 0.9, the second 0.3. The reading depends on which of them was the positive one.
    let upstream = ScriptedUpstream::new(|n| response(&answered(if n == 0 { 0.9 } else { 0.3 })));
    let spot = JevSpot::new(endpoint(&upstream), "jev").with_directories(
        crate::config::policy::Directories::at(std::path::Path::new("/work/proj")),
    );
    let utterances = said();
    let circumstances = Circumstances::default().with_utterances(Some(&utterances));
    let role = ConfigFixture::new()
        .with_scope("work here")
        .with_path(".", true, true)
        .build();
    let spotting = spot.assess(&role, &call(), &circumstances).await;

    let requests = upstream.requests();
    assert_eq!(requests.len(), 2);
    let bodies: Vec<Value> = requests
        .iter()
        .map(crate::inference::testing::upstream::Request::json)
        .collect();
    assert_eq!(bodies[0]["state"], bodies[1]["state"]);
    assert_eq!(bodies[0]["model"], "jev");
    let state = &bodies[0]["state"];
    assert_eq!(
        state["conversation"],
        json!([
            { "writer": "operator", "text": "list the sources" },
            { "writer": "agent", "text": "Listing them." },
        ])
    );
    assert!(
        state["guidance"]
            .as_str()
            .is_some_and(|g| g.starts_with("You are the **spotter**"))
    );
    assert_eq!(state["move"]["command"], "ls src");
    // The body keeps the state's field order.
    let body = &requests[0].body;
    assert!(body.find("\"guidance\"") < body.find("\"conversation\""));
    assert!(body.find("\"conversation\"") < body.find("\"move\""));
    for body in &bodies {
        assert_eq!(
            body["questions"].as_object().map(serde_json::Map::len),
            Some(10)
        );
    }

    let (positive, negative) = match positive_first(&upstream) {
        true => (0.9, 0.3),
        false => (0.3, 0.9),
    };
    let axes = spotting.axes.expect("the answers are recorded");
    assert_eq!(
        axes.readings[ASKED],
        QuestionReading::new(positive, negative)
    );
    assert_eq!(spotting.origin, Origin::Model);
    let usage = spotting.usage.expect("both requests billed");
    assert_eq!(usage.usage.input_tokens, 20);
}

/// The bodies `dev spotter-request --json` prints are the two the rung sends, positive
/// first, and only the positive one asks the promotion question.
#[test]
fn the_request_bodies_are_the_positive_then_the_negative() {
    let upstream = ScriptedUpstream::answering([]);
    let spot = JevSpot::new(endpoint(&upstream), "jev");
    let circumstances = Circumstances::default().with_probe(Some("ls"));
    let bodies = spot
        .request_bodies(&ConfigFixture::new().build(), &call(), &circumstances)
        .expect("bodies");
    let [positive, negative] = bodies.as_slice() else {
        panic!("two bodies: {bodies:?}");
    };
    let positive: Value = serde_json::from_str(positive).unwrap();
    let negative: Value = serde_json::from_str(negative).unwrap();
    assert!(
        positive["questions"][ASKED]["instructions"]
            .as_str()
            .is_some_and(|t| t.starts_with("Did the operator"))
    );
    assert!(
        negative["questions"][ASKED]["instructions"]
            .as_str()
            .is_some_and(|t| t.starts_with("Is this a move that no operator"))
    );
    assert!(positive["questions"].get(PREFIX_SAFE).is_some());
    assert!(negative["questions"].get(PREFIX_SAFE).is_none());
    assert_eq!(positive["state"]["promotion_probe"], "ls");
}

/// A request that fails, or answers without a question it was asked, fails closed: the
/// gate goes to the human, with whatever was billed.
#[tokio::test]
async fn a_failed_or_partial_request_fails_closed() {
    let refused = ScriptedUpstream::new(|n| match n {
        0 => response(&answered(0.9)),
        _ => Canned::json(400, r#"{"error":"nope"}"#),
    });
    let spotting = JevSpot::new(endpoint(&refused), "jev")
        .assess(&Config::default(), &call(), &Circumstances::default())
        .await;
    assert_eq!(spotting.origin, Origin::Fallback);
    assert_eq!(spotting.read.stance, Stance::Gate);
    assert!(spotting.axes.is_none());
    assert_eq!(spotting.usage.map(|u| u.usage.input_tokens), Some(10));

    let partial = ScriptedUpstream::new(|n| {
        let mut answers = answered(0.9);
        if n == 1 {
            answers.as_object_mut().expect("object").remove(SECRET);
        }
        response(&answers)
    });
    let spotting = JevSpot::new(endpoint(&partial), "jev")
        .assess(&Config::default(), &call(), &Circumstances::default())
        .await;
    assert_eq!(spotting.origin, Origin::Fallback);
    assert_eq!(spotting.usage.map(|u| u.usage.input_tokens), Some(20));
}
