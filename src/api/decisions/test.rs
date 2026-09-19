use super::*;

/// The documented wire shape for a boolean question. The route names the type
/// `noul` and requires criteria for both answers; a rename or a dropped side
/// would be accepted by serde and rejected by the provider, so it is pinned here.
#[test]
fn a_boolean_question_serializes_as_the_route_documents() {
    let question = Question::boolean("Is it in scope?", "It serves the mission.", "It does not.");
    assert_eq!(
        serde_json::to_value(&question).expect("serializes"),
        serde_json::json!({
            "type": "noul",
            "instructions": "Is it in scope?",
            "criteria": { "true": "It serves the mission.", "false": "It does not." }
        })
    );
}

fn response(answers: serde_json::Value) -> Decisions {
    serde_json::from_value(serde_json::json!({
        "id": "gen-1",
        "model": "typesafe/jev-1.13",
        "answers": answers,
        "usage": { "input_tokens": 476, "output_tokens": 70, "cost": 0.000_019_992 },
    }))
    .expect("parses")
}

#[test]
fn a_boolean_answer_reads_as_its_probability() {
    let decisions = response(serde_json::json!({
        "in_scope": { "type": "noul", "noul": 0.96 },
    }));
    assert_eq!(decisions.probability("in_scope"), Some(0.96));
    // A question that went unanswered is absent, not zero — the caller's
    // fail-closed path depends on telling those apart.
    assert_eq!(decisions.probability("reversible"), None);
}

/// An answer shape this harness doesn't model must not fail the whole response
/// away: the consumer reads only the questions it asked, and a strict tagged enum
/// would turn one unexpected sibling into a fail-closed gate.
#[test]
fn an_unmodeled_answer_shape_does_not_break_the_response() {
    let decisions = response(serde_json::json!({
        "in_scope": { "type": "noul", "noul": 0.9 },
        "team": { "type": "choice", "choice": "payments", "confidence": 0.75 },
        "urgency": { "type": "score", "score": 1.99, "confidence": 0.99 },
    }));
    assert_eq!(decisions.probability("in_scope"), Some(0.9));
    assert_eq!(decisions.probability("team"), None);
}

/// The route bills input only and has no cache, so the cache counts are a
/// truthful zero rather than a missing reading (docs/log-and-projection.md).
#[test]
fn usage_maps_with_zero_cache_counts() {
    let usage = response(serde_json::json!({})).request_usage();
    assert_eq!(usage.id.as_deref(), Some("gen-1"));
    assert_eq!(usage.usage.input_tokens, 476);
    assert_eq!(usage.usage.output_tokens, 70);
    assert_eq!(usage.usage.cache_read_input_tokens, 0);
    assert_eq!(usage.usage.cache_creation_input_tokens, 0);
    assert_eq!(usage.usage.prompt_tokens(), 476);
}

/// The first-party API validates its body and answers an unrecognized top-level
/// key with a 422, so `session_id` rides only where the provider documents it.
#[test]
fn the_session_id_rides_only_where_it_is_documented() {
    let body = |provider: Provider| {
        let mut endpoint =
            Endpoint::from_credentials(provider, "k".into(), None).expect("credentials");
        endpoint.session_id = Some("sess-1".into());
        serde_json::to_value(DecisionsRequest {
            model: "jev-latest",
            state: &Value::Null,
            questions: &BTreeMap::new(),
            session_id: endpoint
                .provider
                .decisions_takes_session_id()
                .then_some(endpoint.session_id.as_deref())
                .flatten(),
        })
        .expect("serializes")
    };
    assert_eq!(body(Provider::OpenRouter)["session_id"], "sess-1");
    assert!(body(Provider::TypeSafe).get("session_id").is_none());
}

/// The first-party response carries neither a generation id nor a cost — the
/// fields OpenRouter's does — and must still parse.
#[test]
fn the_first_party_response_shape_parses() {
    let decisions: Decisions = serde_json::from_value(serde_json::json!({
        "model": "jev-latest",
        "answers": { "in_scope": { "type": "noul", "noul": 0.91 } },
        "usage": { "input_tokens": 412, "output_tokens": 0 },
    }))
    .expect("parses");
    assert_eq!(decisions.probability("in_scope"), Some(0.91));
    let usage = decisions.request_usage();
    assert_eq!(usage.id, None);
    assert_eq!(usage.usage.input_tokens, 412);
}

/// A provider with no decisions route says so rather than posting a decisions
/// body at its messages URL.
#[tokio::test]
async fn a_provider_without_the_route_refuses() {
    let endpoint = Endpoint::from_credentials(Provider::Anthropic, "k".into(), None)
        .expect("anthropic credentials");
    let error = endpoint
        .decide("typesafe/jev-1.13", &Value::Null, &BTreeMap::new())
        .await
        .expect_err("anthropic serves no decisions route");
    assert!(
        error.to_string().contains("no decisions route"),
        "unexpected error: {error}"
    );
}
