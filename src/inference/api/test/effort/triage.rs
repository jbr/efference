use super::*;
use crate::inference::api::Upstream;
use crate::inference::api::test::forced_choice::scripted;
use crate::record::project::WireTurn;

const REFUSAL: &str = r#"{"error":{"message":"unsupported output_config.effort: low"}}"#;
const OTHER_ERROR: &str = r#"{"error":{"message":"invalid messages"}}"#;
const STREAM: &str = "event: message_start\ndata: {\"type\":\"message_start\",\
    \"message\":{\"id\":\"gen-1\",\"model\":\"m\",\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n\
    event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n\
    event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

async fn foreground(client: &Client) -> (Result<Completion, Error>, Vec<String>) {
    let mut retries = Vec::new();
    let (answer, _) = client
        .complete_separate_triage(
            &[WireTurn::text("retain useful lines")],
            &[],
            ToolChoice::auto(),
            |_| {},
            &mut retries,
        )
        .await;
    (answer, retries)
}

fn without_off(client: Client) -> Client {
    client.with_thinking(ThinkingSupport {
        adaptive: true,
        off: ThinkingOff::Omit,
    })
}

#[tokio::test]
async fn low_changes_only_effort_and_leaves_ordinary_requests_alone() {
    let (client, server) = scripted(vec![(200, STREAM), (200, STREAM)]);
    let client = without_off(client).with_effort(Some("high".into()));
    let (answer, retries) = foreground(&client).await;
    answer.unwrap();
    assert!(retries.is_empty());
    client
        .complete(
            &[WireTurn::text("retain useful lines")],
            &[],
            ToolChoice::auto(),
            |_| {},
        )
        .await
        .unwrap();
    let mut bodies = server.bodies();
    assert_eq!(bodies[0]["output_config"]["effort"], "low");
    assert_eq!(bodies[1]["output_config"]["effort"], "high");
    bodies[0]["output_config"]["effort"] = "high".into();
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn fallback_unsets_effort_and_is_remembered_only_on_the_same_route() {
    let (client, server) = scripted(vec![
        (400, REFUSAL),
        (200, STREAM),
        (200, STREAM),
        (200, STREAM),
    ]);
    let client = without_off(client).with_effort(Some("high".into()));
    let clone = client.clone();
    let rerouted = client.clone().with_upstream(Some(Upstream {
        order: vec!["other".into()],
        allow_fallbacks: false,
    }));
    for (client, count) in [(&client, 1), (&clone, 0), (&rerouted, 0)] {
        let (answer, retries) = foreground(client).await;
        answer.unwrap();
        assert_eq!(retries.len(), count);
    }
    let bodies = server.bodies();
    assert_eq!(bodies[0]["output_config"]["effort"], "low");
    assert!(bodies[1].get("output_config").is_none());
    assert_eq!(bodies[1], bodies[2]);
    assert_eq!(bodies[3]["output_config"]["effort"], "low");
}

#[tokio::test]
async fn off_is_preferred_and_sent_without_main_effort_even_with_a_forced_tool() {
    let (client, server) = scripted(vec![(200, STREAM)]);
    let client = client.with_effort(Some("high".into()));
    let (answer, mode) = client
        .complete_separate_triage(
            &[WireTurn::text("retain useful lines")],
            &[],
            ToolChoice::force("retain"),
            |_| {},
            &mut Vec::new(),
        )
        .await;
    answer.unwrap();
    assert_eq!(mode, TriageReasoning::Off);
    let bodies = server.bodies();
    assert_eq!(bodies[0]["thinking"]["type"], "between_tools");
    assert_eq!(bodies[0]["tool_choice"]["name"], "retain");
    assert!(bodies[0].get("output_config").is_none());
}

#[tokio::test]
async fn rejected_off_tries_low_and_then_unset() {
    let (client, server) = scripted(vec![
        (
            400,
            r#"{"error":{"message":"thinking between_tools is not supported"}}"#,
        ),
        (400, REFUSAL),
        (200, STREAM),
        (200, STREAM),
    ]);
    let (answer, retries) = foreground(&client).await;
    answer.unwrap();
    assert_eq!(retries.len(), 2);
    foreground(&client).await.0.unwrap();
    let bodies = server.bodies();
    assert_eq!(bodies[0]["thinking"]["type"], "between_tools");
    assert_eq!(bodies[1]["thinking"]["type"], "adaptive");
    assert_eq!(bodies[1]["output_config"]["effort"], "low");
    assert!(bodies[2].get("output_config").is_none());
    assert_eq!(bodies[2], bodies[3]);
}

#[tokio::test]
async fn a_double_rejection_preserves_the_first_error_and_does_not_teach_the_client() {
    let (client, server) = scripted(vec![(400, REFUSAL), (400, OTHER_ERROR), (200, STREAM)]);
    let client = without_off(client);
    let (answer, retries) = foreground(&client).await;
    assert!(
        answer
            .err()
            .unwrap()
            .to_string()
            .contains("unsupported output_config.effort")
    );
    assert_eq!(retries.len(), 1);
    foreground(&client).await.0.unwrap();
    let bodies = server.bodies();
    assert_eq!(bodies[0], bodies[2]);
    assert!(bodies[1].get("output_config").is_none());
}

#[tokio::test]
async fn unrelated_bad_requests_and_auth_errors_do_not_trigger_an_effort_retry() {
    for (status, body) in [(400, OTHER_ERROR), (401, REFUSAL)] {
        let (client, server) = scripted(vec![(status, body)]);
        let (answer, retries) = foreground(&without_off(client)).await;
        assert!(answer.is_err());
        assert!(retries.is_empty());
        assert_eq!(server.bodies()[0]["output_config"]["effort"], "low");
    }
}

#[tokio::test]
async fn a_result_keepalive_reuses_the_reasoning_mode_sent_on_the_result() {
    let ping = r#"{"id":"p","model":"m","usage":{"input_tokens":1,"output_tokens":0}}"#;
    let (client, server) = scripted(vec![(200, STREAM), (200, ping)]);
    let client = without_off(client).with_effort(Some("high".into()));
    let turns = vec![WireTurn::text("retain useful lines")];
    let choice = ToolChoice::auto();
    let (answer, mode) = client
        .complete_separate_triage(&turns, &[], choice, |_| {}, &mut Vec::new())
        .await;
    answer.unwrap();
    let mut sent = client.sent_form(turns, &[], choice);
    sent.triage_reasoning = Some(mode);
    client
        .keepalive(RequestOrigin::TriageKeepalive, &sent)
        .await
        .unwrap();
    let bodies = server.bodies();
    assert_eq!(bodies[0]["output_config"]["effort"], "low");
    assert_eq!(bodies[1]["output_config"], bodies[0]["output_config"]);
    assert_eq!(bodies[1]["thinking"], bodies[0]["thinking"]);
}
