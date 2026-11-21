//! Shared result triage's effort and the cache evidence that withdraws `low`
//! (docs/triage.md).

use super::*;
use crate::inference::api::SharedEffort;
use crate::inference::api::Upstream;
use crate::inference::api::client::LowEffortEvidence;
use crate::inference::api::test::forced_choice::scripted;
use crate::record::project::WireTurn;

const REFUSAL: &str = r#"{"error":{"message":"unsupported output_config.effort: low"}}"#;

/// A finished stream whose usage reports `read` tokens read from the cache and `write`
/// written to it.
fn stream(read: u64, write: u64) -> &'static str {
    Box::leak(
        format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\
             \"message\":{{\"id\":\"gen-1\",\"model\":\"m\",\"usage\":{{\"input_tokens\":1,\
             \"output_tokens\":0,\"cache_read_input_tokens\":{read},\
             \"cache_creation_input_tokens\":{write}}}}}}}\n\n\
             event: message_delta\ndata: {{\"delta\":{{\"stop_reason\":\"end_turn\"}},\
             \"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}\n\n\
             event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
        )
        .into_boxed_str(),
    )
}

fn turns() -> Vec<WireTurn> {
    vec![WireTurn::text("retain useful lines")]
}

async fn turn(client: &Client) {
    client
        .with_origin(RequestOrigin::Turn)
        .complete(&turns(), &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
}

/// One shared result triage, as its first request: returns the effort it chose.
async fn triage(client: &Client, retries: &mut Vec<String>) -> SharedEffort {
    let mut effort = None;
    client
        .with_origin(RequestOrigin::Triage)
        .complete_shared_triage(
            &turns(),
            &[],
            ToolChoice::auto(),
            |_| {},
            &mut effort,
            retries,
        )
        .await
        .unwrap();
    effort.expect("the first request chooses the triage's effort")
}

/// A request body without the turn's cache-diagnostics opt-in, which only a turn sends
/// (docs/transport.md): what remains is the envelope a triage must match.
fn envelope(mut body: Value) -> Value {
    body.as_object_mut().unwrap().remove("diagnostics");
    body
}

fn agent(client: Client) -> Client {
    client.with_effort(Some("high".into()))
}

#[tokio::test]
async fn low_changes_only_the_effort_and_keeps_the_agents_thinking() {
    let (client, server) = scripted(vec![(200, stream(0, 1000)), (200, stream(1000, 0))]);
    let client = agent(client);
    turn(&client).await;
    assert_eq!(triage(&client, &mut Vec::new()).await, SharedEffort::Low);
    let bodies: Vec<Value> = server.bodies().into_iter().map(envelope).collect();
    assert_eq!(bodies[0]["thinking"]["type"], "adaptive");
    assert_eq!(bodies[0]["output_config"]["effort"], "high");
    assert_eq!(bodies[1]["output_config"]["effort"], "low");
    let mut triage = bodies[1].clone();
    triage["output_config"]["effort"] = "high".into();
    assert_eq!(bodies[0], triage, "nothing but the effort differs");
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Neutral);
}

#[tokio::test]
async fn a_neutral_route_ignores_a_later_miss() {
    let (client, _server) = scripted(vec![
        (200, stream(0, 1000)),
        (200, stream(1000, 0)),
        (200, stream(1000, 200)),
        (200, stream(0, 1200)),
    ]);
    let client = agent(client);
    turn(&client).await;
    triage(&client, &mut Vec::new()).await;
    turn(&client).await;
    assert_eq!(triage(&client, &mut Vec::new()).await, SharedEffort::Low);
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Neutral);
}

#[tokio::test]
async fn two_short_low_reads_around_a_full_control_inherit_the_agents_effort() {
    let (client, server) = scripted(vec![
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
        (200, stream(1000, 100)),
        (200, stream(1100, 0)),
        (200, stream(1100, 100)),
        // Most of the prefix, from the first `low` triage's write: still short.
        (200, stream(1100, 100)),
        (200, stream(1200, 100)),
        (200, stream(1300, 0)),
    ]);
    let client = agent(client);
    let mut retries = Vec::new();
    turn(&client).await;
    assert_eq!(triage(&client, &mut retries).await, SharedEffort::Low);
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::ControlNext);
    turn(&client).await;
    assert_eq!(triage(&client, &mut retries).await, SharedEffort::Inherited);
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Contrasted);
    turn(&client).await;
    assert_eq!(triage(&client, &mut retries).await, SharedEffort::Low);
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Inherit);
    assert_eq!(
        retries.len(),
        1,
        "the fallback is recorded once: {retries:?}"
    );
    turn(&client).await;
    assert_eq!(triage(&client, &mut retries).await, SharedEffort::Inherited);
    let bodies = server.bodies();
    assert_eq!(bodies[3]["output_config"]["effort"], "high", "the control");
    assert_eq!(
        bodies[7]["output_config"]["effort"], "high",
        "after the fallback"
    );
}

#[tokio::test]
async fn a_short_control_means_another_cause_and_restarts_the_count() {
    let (client, _server) = scripted(vec![
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
    ]);
    let client = agent(client);
    turn(&client).await;
    triage(&client, &mut Vec::new()).await;
    turn(&client).await;
    assert_eq!(
        triage(&client, &mut Vec::new()).await,
        SharedEffort::Inherited
    );
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Untested);
    turn(&client).await;
    assert_eq!(triage(&client, &mut Vec::new()).await, SharedEffort::Low);
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::ControlNext);
}

#[tokio::test]
async fn no_evidence_without_a_recent_cached_turn() {
    let (client, _server) = scripted(vec![
        (200, stream(0, 0)),
        (200, stream(0, 0)),
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
    ]);
    let client = agent(client);
    // A route that caches nothing gives the read nothing to fall short of.
    turn(&client).await;
    triage(&client, &mut Vec::new()).await;
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Untested);
    // A turn older than the window may have expired while the tools ran.
    turn(&client).await;
    client.age_turn_cache(std::time::Duration::from_secs(241));
    triage(&client, &mut Vec::new()).await;
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Untested);
}

#[tokio::test]
async fn a_refused_low_resends_at_the_agents_effort_and_settles_on_it() {
    let (client, server) = scripted(vec![
        (200, stream(0, 1000)),
        (400, REFUSAL),
        (200, stream(1000, 0)),
        (200, stream(1000, 0)),
    ]);
    let client = agent(client);
    let mut retries = Vec::new();
    turn(&client).await;
    assert_eq!(triage(&client, &mut retries).await, SharedEffort::Inherited);
    assert_eq!(retries.len(), 1);
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Inherit);
    assert_eq!(triage(&client, &mut retries).await, SharedEffort::Inherited);
    let bodies: Vec<Value> = server.bodies().into_iter().map(envelope).collect();
    assert_eq!(bodies[1]["output_config"]["effort"], "low");
    assert_eq!(bodies[2], bodies[0]);
    assert_eq!(bodies[3], bodies[0]);
}

#[tokio::test]
async fn the_agents_own_low_goes_unchanged_and_teaches_nothing() {
    let (client, server) = scripted(vec![(200, stream(0, 1000)), (200, stream(0, 1000))]);
    let client = client.with_effort(Some("low".into()));
    turn(&client).await;
    let mut effort = None;
    client
        .complete_shared_triage(
            &turns(),
            &[],
            ToolChoice::auto(),
            |_| {},
            &mut effort,
            &mut Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Untested);
    let bodies: Vec<Value> = server.bodies().into_iter().map(envelope).collect();
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn a_triages_later_requests_keep_its_effort_and_are_not_evidence() {
    let (client, server) = scripted(vec![
        (200, stream(0, 1000)),
        (200, stream(0, 1000)),
        (200, stream(1000, 50)),
    ]);
    let client = agent(client);
    turn(&client).await;
    let mut effort = None;
    for _ in 0..2 {
        client
            .complete_shared_triage(
                &turns(),
                &[],
                ToolChoice::auto(),
                |_| {},
                &mut effort,
                &mut Vec::new(),
            )
            .await
            .unwrap();
    }
    // The first read was short; the retry's full read is the fork's own tail.
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::ControlNext);
    let bodies = server.bodies();
    assert_eq!(bodies[1]["output_config"]["effort"], "low");
    assert_eq!(bodies[2]["output_config"]["effort"], "low");
}

#[tokio::test]
async fn a_new_upstream_starts_without_evidence() {
    let (client, _server) = scripted(vec![
        (200, stream(0, 1000)),
        (400, REFUSAL),
        (200, stream(1000, 0)),
    ]);
    let client = agent(client);
    turn(&client).await;
    triage(&client, &mut Vec::new()).await;
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Inherit);
    let rerouted = client.clone().with_upstream(Some(Upstream {
        order: vec!["other".into()],
        allow_fallbacks: false,
    }));
    assert_eq!(rerouted.low_effort_evidence(), LowEffortEvidence::Untested);
    assert_eq!(client.low_effort_evidence(), LowEffortEvidence::Inherit);
}
