use super::*;
use serde_json::json;

const NOW: u64 = 1_754_300_130_000;
const MODEL: &str = "z-ai/glm-5.3-flash";

fn text(topic: Value) -> Option<String> {
    operation_text(&parse_current_operations(&topic)?, MODEL, NOW)
}

fn request(origin: &str, stage: &str, ago_ms: u64) -> Value {
    json!({"request": 1, "origin": origin, "stage": stage, "since": NOW - ago_ms})
}

#[test]
fn nothing_open_is_no_view() {
    assert_eq!(parse_current_operations(&Value::Null), None);
}

#[test]
fn a_turn_request_names_the_model_and_the_wait() {
    let topic = json!({"requests": [request("turn", "sent", 130_000)], "tools": []});
    assert_eq!(
        text(topic).as_deref(),
        Some("waiting on z-ai/glm-5.3-flash · 2m 10s")
    );
    let topic = json!({"requests": [request("turn", "streaming", 5_000)], "tools": []});
    assert_eq!(text(topic).as_deref(), Some("z-ai/glm-5.3-flash streaming"));
}

#[test]
fn a_refused_attempt_reads_as_a_retry() {
    let mut retrying = request("turn", "retrying", 8_000);
    retrying["attempt"] = json!(2);
    retrying["status"] = json!(429);
    assert_eq!(
        text(json!({"requests": [retrying], "tools": []})).as_deref(),
        Some("retrying upstream · attempt 2 (429) · 8s")
    );
}

#[test]
fn side_requests_are_named_by_what_they_are() {
    let peek = json!({"requests": [request("peek", "sent", 40_000)], "tools": []});
    assert_eq!(text(peek).as_deref(), Some("triage peek · waiting 40s"));
    let spot = json!({"requests": [request("spot", "sent", 3_000)], "tools": []});
    assert_eq!(text(spot).as_deref(), Some("spotter judging · 3s"));
}

#[test]
fn a_running_tool_is_named_by_its_intent() {
    let topic = json!({"requests": [], "tools": [
        {"call_id": "t1", "tool": "bash", "intent": "run the suite", "since": NOW - 45_000}
    ]});
    assert_eq!(text(topic).as_deref(), Some("bash: run the suite · 45s"));
}

#[test]
fn the_turn_leads_then_a_tool_then_side_requests() {
    let tool = json!({"call_id": "t1", "tool": "read", "intent": null, "since": NOW - 2_000});
    let side = request("title", "sent", 1_000);
    let turn = request("turn", "sent", 3_000);
    assert_eq!(
        text(json!({"requests": [side.clone()], "tools": [tool.clone()]})).as_deref(),
        Some("read · 2s")
    );
    assert!(
        text(json!({"requests": [side.clone(), turn], "tools": [tool]}))
            .unwrap()
            .starts_with("waiting on")
    );
    assert_eq!(
        text(json!({"requests": [side], "tools": []})).as_deref(),
        Some("writing a title · 1s")
    );
}
