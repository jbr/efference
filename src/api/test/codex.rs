//! Tests for Codex-specific wire behaviour: turn-routing-token replay and
//! drop, plan windows read and retained, plan windows ignored off the
//! backend, and the Codex request headers.

use super::*;
use crate::log::RequestOrigin;
use crate::testing::upstream::{Canned, ScriptedUpstream};

/// The Codex route an upstream answers on.
const CODEX: &str = "/backend-api/codex/responses";

/// A completed Responses stream with no output.
const COMPLETED: &str = "event: response.completed\ndata: {\"type\":\"response.completed\",\
    \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";

/// A completed Responses stream with no output, 900 of its 1000 input tokens read from
/// the cache and 16 written.
const CACHED: &str = "event: response.completed\ndata: {\"type\":\"response.completed\",\
    \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
    \"usage\":{\"input_tokens\":1000,\"input_tokens_details\":\
    {\"cached_tokens\":900},\"output_tokens\":16}}}\n\n";

/// The Codex backend's turn-routing contract (docs/transport.md): the first request of
/// a turn carries no token, the token the backend issues on that response rides every
/// later request of the turn unchanged (a second issued value does not replace it), and
/// a new turn starts without one. The model hint rides every request.
#[tokio::test]
async fn codex_turn_routing_token_is_replayed_within_a_turn_and_dropped_at_the_next() {
    let upstream = ScriptedUpstream::answering([
        Canned::stream(COMPLETED).with_header("x-codex-turn-state", "tok-one"),
        Canned::stream(COMPLETED).with_header("x-codex-turn-state", "tok-two"),
        Canned::stream(COMPLETED),
    ]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenAICodex, CODEX),
        "gpt-5.6-sol",
        "sys",
    );
    assert_eq!(client.turn_routing_token(), None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(client.turn_routing_token().as_deref(), Some("tok-one"));
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        client.turn_routing_token().as_deref(),
        Some("tok-one"),
        "the first issued token holds for the whole turn"
    );
    client.begin_turn();
    assert_eq!(client.turn_routing_token(), None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();

    let requests = upstream.requests();
    for (i, request) in requests.iter().enumerate() {
        assert_eq!(
            request.header("x-codex-routing-hint"),
            Some("model=gpt-5.6-sol"),
            "request {i} names the model for the router"
        );
    }
    assert_eq!(
        requests[0].header("x-codex-turn-state"),
        None,
        "a turn's first request carries no token"
    );
    assert_eq!(
        requests[1].header("x-codex-turn-state"),
        Some("tok-one"),
        "the second request replays it"
    );
    assert_eq!(
        requests[2].header("x-codex-turn-state"),
        None,
        "a new turn starts without one"
    );
}

/// A client outside any turn (the spotter's) never holds a token, whatever the
/// backend sends.
#[test]
fn a_client_without_turn_routing_holds_no_token() {
    let endpoint = Endpoint::at(Provider::OpenAICodex, "http://127.0.0.1:1/x");
    let client = Client::new(endpoint, "gpt-5.6-luna", "sys").without_turn_routing();
    client.begin_turn();
    assert_eq!(client.turn_routing_token(), None);
}

/// The Codex backend's plan windows are read off every response, a 429 included,
/// and a response that carries none leaves the last snapshot standing
/// (docs/transport.md). Three responses: a 200 with both windows, a 200 with none, and
/// a 429 that moves the primary window.
#[tokio::test]
async fn codex_plan_windows_are_read_off_every_response_and_retained_when_absent() {
    let with_headers = |canned: Canned, headers: &[(&str, &str)]| {
        headers.iter().fold(canned, |canned, (name, value)| {
            canned.with_header(name, *value)
        })
    };
    let upstream = ScriptedUpstream::answering([
        with_headers(
            Canned::stream(COMPLETED),
            &[
                ("x-codex-plan-type", "plus"),
                ("x-codex-primary-used-percent", "12"),
                ("x-codex-primary-window-minutes", "300"),
                ("x-codex-primary-reset-at", "1788677725"),
                ("x-codex-secondary-used-percent", "19"),
                ("x-codex-secondary-window-minutes", "10080"),
                ("x-codex-secondary-reset-at", "1789180313"),
                ("x-codex-spark-primary-used-percent", "45"),
                ("x-codex-spark-limit-name", "spark"),
            ],
        ),
        Canned::stream(COMPLETED),
        with_headers(
            Canned::json(
                429,
                r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":15805}}"#,
            ),
            &[
                ("x-codex-plan-type", "plus"),
                ("x-codex-primary-used-percent", "100"),
                ("x-codex-primary-window-minutes", "300"),
                ("x-codex-primary-reset-at", "1788744444"),
                ("x-codex-active-limit", "codex-spark"),
                ("x-codex-spark-primary-used-percent", "100"),
            ],
        ),
    ]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenAICodex, CODEX),
        "gpt-5.6-sol",
        "sys",
    );
    let plan = client.plan_state();
    assert!(plan.lock().unwrap().is_none());

    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let first = plan
        .lock()
        .unwrap()
        .clone()
        .expect("read off the first response");
    assert_eq!(first.plan_type.as_deref(), Some("plus"));
    let ordinary = &first.limits["codex"];
    let primary = ordinary.primary.as_ref().unwrap();
    assert_eq!(primary.used_percent, 12.0);
    assert_eq!(primary.window_minutes, Some(300));
    assert_eq!(primary.resets_at, Some(1_788_677_725_000));
    assert_eq!(ordinary.secondary.as_ref().unwrap().used_percent, 19.0);
    assert_eq!(first.limits["codex_spark"].name.as_deref(), Some("spark"));
    assert_eq!(
        first.limits["codex_spark"]
            .primary
            .as_ref()
            .unwrap()
            .used_percent,
        45.0
    );

    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        plan.lock().unwrap().clone(),
        Some(first.clone()),
        "a response without plan headers retains the last snapshot"
    );

    let refused = client.complete(&[], &[], ToolChoice::auto(), |_| {}).await;
    assert!(refused.is_err(), "the terminal 429 is not retried");
    assert_eq!(upstream.count(), 3);
    let last = plan.lock().unwrap().clone().unwrap();
    assert_eq!(last.active.as_deref(), Some("codex_spark"));
    let ordinary = &last.limits["codex"];
    assert_eq!(ordinary.primary.as_ref().unwrap().used_percent, 100.0);
    assert_eq!(
        ordinary.primary.as_ref().unwrap().resets_at,
        Some(1_788_744_444_000)
    );
    assert!(
        ordinary.secondary.is_none(),
        "a response naming only one window replaces the whole snapshot"
    );
}

/// A client on a provider without plan headers never fills the cell, whatever
/// the response says.
#[tokio::test]
async fn plan_windows_are_ignored_off_the_codex_backend() {
    let upstream = ScriptedUpstream::answering([Canned::stream(
        "event: message_start\ndata: {\"message\":{\"id\":\"gen-1\",\
         \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
         event: data\ndata: [DONE]\n\n",
    )
    .with_header("x-codex-plan-type", "plus")
    .with_header("x-codex-primary-used-percent", "50")]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenRouter, "/api/v1/messages"),
        "m",
        "sys",
    );
    let _ = client.complete(&[], &[], ToolChoice::auto(), |_| {}).await;
    assert_eq!(upstream.count(), 1);
    assert!(client.plan_state().lock().unwrap().is_none());
}

/// The Responses HTTP boundary (docs/transport.md): a Codex request carries the bearer
/// token, the account header the endpoint resolved, and the originator attribution, and
/// no `anthropic-version` — that header belongs to the Messages dialect, not to a
/// provider.
#[tokio::test]
async fn a_codex_request_carries_its_headers_and_no_anthropic_version() {
    let upstream = ScriptedUpstream::answering([Canned::stream(COMPLETED).without_content_type()]);
    let endpoint = upstream
        .endpoint(Provider::OpenAICodex, CODEX)
        .with_auth_value("Bearer tok".into())
        .with_extra_headers(vec![("ChatGPT-Account-ID".into(), "acct-1".into())]);
    let client = Client::new(endpoint, "gpt-5.6-luna", "sys");
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("a stream with no content-type still assembles on the Codex arm");
    let request = &upstream.requests()[0];
    assert_eq!(request.header("authorization"), Some("Bearer tok"));
    assert_eq!(request.header("chatgpt-account-id"), Some("acct-1"));
    assert_eq!(request.header("originator"), Some("efference"));
    assert_eq!(request.header("anthropic-version"), None);
    assert_eq!(request.header("content-type"), Some("application/json"));
}

/// The Responses keepalive (docs/transport.md) is the turn's own envelope with the
/// output capped at the backend's minimum, which the Codex profile otherwise omits;
/// a turn still sends no cap. Its usage comes back as the ping's, cached read
/// included.
#[tokio::test]
async fn codex_keepalive_caps_output_where_a_turn_sends_no_cap() {
    let upstream = ScriptedUpstream::answering([Canned::stream(CACHED), Canned::stream(CACHED)]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenAICodex, CODEX),
        "gpt-6.1-sol",
        "sys",
    );
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let ping = client
        .keepalive(
            RequestOrigin::Keepalive,
            &SentRequest::new(Vec::new(), Vec::new(), ToolChoice::auto()),
        )
        .await
        .unwrap();
    assert_eq!(ping.usage.cache_read_input_tokens, 900);
    assert_eq!(ping.usage.output_tokens, 16);

    let requests = upstream.requests();
    assert!(
        !requests[0].body.contains("max_output_tokens"),
        "a turn sends no cap"
    );
    assert!(
        requests[1].body.contains(r#""max_output_tokens":16"#),
        "the ping caps its output"
    );
}

/// The triage primer (docs/transport.md) replays the turn's own
/// envelope with the backend-minimum cap: its input bytes match the turn's exactly,
/// so the Codex backend resumes where the main request ended. Its `tool_choice`
/// differs on purpose — forced `retain`, matching the triage's head rather than the
/// turn's `auto`, since the backend keys the resume on it.
#[tokio::test]
async fn codex_primer_replays_the_turn_envelope_capped() {
    let upstream = ScriptedUpstream::answering([Canned::stream(CACHED), Canned::stream(CACHED)]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenAICodex, CODEX),
        "gpt-6.1-sol",
        "sys",
    )
    .with_prompt_cache(PromptCachePolicy::Implicit);
    let turns = vec![crate::project::WireTurn::text("hello")];
    let tools = vec![serde_json::json!({"type": "function", "name": "x"})];
    client
        .complete(&turns, &tools, ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let primer = client
        .prime(&turns, &tools, ToolChoice::force("retain"))
        .await
        .expect("an implicit Responses lineage takes a primer")
        .expect("the mock backend answers the primer");
    assert_eq!(primer.usage.cache_read_input_tokens, 900);
    assert_eq!(primer.usage.output_tokens, 16);

    let requests = upstream.requests();
    let mut turn_body = requests[0].json();
    let mut primer_body = requests[1].json();
    assert!(
        !turn_body.to_string().contains("max_output_tokens"),
        "a turn sends no cap"
    );
    assert_eq!(
        primer_body.get("max_output_tokens"),
        Some(&serde_json::json!(16)),
        "the primer caps its output at the backend minimum"
    );
    assert_eq!(
        primer_body.get("tool_choice"),
        Some(&serde_json::json!({"type": "function", "name": "retain"})),
        "the primer forces retain, matching the triage's head rather than the turn's auto"
    );
    assert_eq!(
        primer_body.get("parallel_tool_calls"),
        Some(&serde_json::json!(false)),
        "forcing retain disables parallel calls, as the triage sends it"
    );
    for field in ["max_output_tokens", "tool_choice", "parallel_tool_calls"] {
        primer_body.as_object_mut().unwrap().remove(field);
        turn_body.as_object_mut().unwrap().remove(field);
    }
    assert_eq!(
        primer_body, turn_body,
        "the primer replays the turn's input byte-for-byte apart from cap and choice"
    );
}

/// The primer plants only where an implicit cache exists behind a Responses lineage:
/// Messages dialects and explicit policies resolve to no primer without sending.
#[tokio::test]
async fn primer_sends_nothing_without_an_implicit_responses_lineage() {
    let messages = Client::new(
        Endpoint::without_url(Provider::Anthropic),
        "claude-haiku-4-5",
        "sys",
    );
    assert!(
        messages.prime(&[], &[], ToolChoice::auto()).await.is_none(),
        "no primer on a Messages lineage"
    );

    let explicit = Client::new(
        Endpoint::without_url(Provider::OpenAICodex),
        "gpt-6.1-sol",
        "sys",
    )
    .with_prompt_cache(PromptCachePolicy::Explicit);
    assert!(
        explicit.prime(&[], &[], ToolChoice::auto()).await.is_none(),
        "no primer where the harness places explicit breakpoints"
    );
}

/// The primer wants the write, not the read: a 16-token budget at a call-eliciting
/// position truncates the model's function call mid-arguments, which marks the
/// completion without unmaking its input prefix. The primer still plants.
/// (docs/transport.md — pinned after the first live run
/// showed every primer failing this way.)
#[tokio::test]
async fn primer_plants_despite_truncated_output() {
    let upstream = ScriptedUpstream::answering([Canned::stream(
        "event: response.completed\ndata: {\"type\":\"response.completed\",\
         \"response\":{\"id\":\"r\",\"status\":\"completed\",\
         \"output\":[{\"type\":\"function_call\",\"call_id\":\"c\",\
         \"name\":\"bash\",\"arguments\":\"{not json\"}],\
         \"usage\":{\"input_tokens\":1000,\"output_tokens\":16}}}\n\n",
    )]);
    let endpoint = upstream.endpoint(Provider::OpenAICodex, CODEX);
    let client =
        Client::new(endpoint, "gpt-6-luna", "sys").with_prompt_cache(PromptCachePolicy::Implicit);
    let turns = vec![crate::project::WireTurn::text("hello")];
    let primer = client
        .prime(&turns, &[], ToolChoice::auto())
        .await
        .expect("an implicit Responses lineage takes a primer")
        .expect("truncated output still plants where input was processed");
    assert_eq!(primer.usage.input_tokens, 1000);
    assert_eq!(upstream.count(), 1);
}
