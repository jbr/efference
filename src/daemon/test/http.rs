//! Tests for `http.rs`.

use super::*;

/// `POST /review-level` over the real router + control client (docs/permissions.md):
/// a valid level is set and echoed on `/state`; an unknown level is rejected (400) and the dial
/// is left untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_level_over_the_socket() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-review-level-test.jsonl");
    let socket = dir.join("efference-review-level-test.sock");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&socket);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .expect("bind uds")
        .spawn(router());

    let client = ControlClient::new(&socket);
    assert_eq!(
        client.review_level("human_only").await.unwrap(),
        "human_only"
    );
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert_eq!(state["review_level"], "human_only");
    assert_eq!(state["autonomous"], false, "human_only is not autonomous");

    // An unknown level is rejected and does not move the dial.
    assert!(client.review_level("wat").await.is_err(), "unknown → error");
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert_eq!(state["review_level"], "human_only", "unchanged after a 400");

    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);
}

/// A `config_edit` decision body decodes into its change, the wire shape the orchestrator
/// posts to upgrade a gate (docs/permissions.md). A `lifetime` from an older
/// client is ignored rather than rejected — every committed change is written to the file now,
/// so there is no lifetime left to choose.
#[test]
fn config_edit_decision_body_decodes() {
    let body = r#"{"decision":"config_edit","lifetime":"persist","note":"in scope",
                       "change":{"command":{"command":"cargo nextest"}}}"#;
    let d: DecisionBody = serde_json::from_str(body).unwrap();
    assert_eq!(d.decision, "config_edit");
    assert_eq!(
        d.change,
        Some(crate::log::ConfigChange::Command {
            command: "cargo nextest".into()
        })
    );
}
