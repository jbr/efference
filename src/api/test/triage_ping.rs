//! On a lineage the primer plants, the triage keepalive's ping is the primer's own request
//! (docs/transport.md): over the same turns, envelope, and choice, the two sends carry the
//! same body bytes. The ping differs only in its request origin, which rides the stage
//! record and the usage line, not the body.

use super::*;
use crate::log::RequestOrigin;
use crate::testing::upstream::{Canned, ScriptedUpstream};

/// A completed Responses stream with no output, 900 of its 1000 input tokens read from
/// the cache and 16 written.
const CACHED: &str = "event: response.completed\ndata: {\"type\":\"response.completed\",\
    \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
    \"usage\":{\"input_tokens\":1000,\"input_tokens_details\":\
    {\"cached_tokens\":900},\"output_tokens\":16}}}\n\n";

/// The primer and the triage keepalive, sent over the same turns with the triage's
/// envelope, carry byte-identical bodies. A ping that re-sent anything else would
/// refresh a different entry from the one the primer planted.
#[tokio::test]
async fn the_triage_ping_body_is_the_primers_body() {
    let upstream = ScriptedUpstream::answering([Canned::stream(CACHED), Canned::stream(CACHED)]);
    let endpoint = upstream.endpoint(Provider::OpenAICodex, "/backend-api/codex/responses");
    let client = Client::new(endpoint, "gpt-6.1-sol", "sys", None)
        .with_prompt_cache(PromptCachePolicy::Implicit);
    let turns = vec![crate::project::WireTurn::text("hello")];
    let tools = vec![serde_json::json!({"type": "function", "name": "retain"})];
    let choice = ToolChoice::force("retain");

    let primer = client
        .prime(&turns, &tools, choice)
        .await
        .expect("an implicit Responses lineage takes a primer")
        .expect("the backend answers the primer");
    let ping = client
        .keepalive(
            RequestOrigin::TriageKeepalive,
            &SentRequest::new(turns.clone(), tools.clone(), choice),
        )
        .await
        .expect("the backend answers the ping");
    assert_eq!(primer.usage.cache_read_input_tokens, 900);
    assert_eq!(ping.usage.cache_read_input_tokens, 900);

    let requests = upstream.requests();
    assert_eq!(
        requests[1].body, requests[0].body,
        "the ping sends the primer's body"
    );
    assert!(
        requests[1].body.contains(r#""max_output_tokens":16"#),
        "and the ping keeps the primer's output cap"
    );
}
