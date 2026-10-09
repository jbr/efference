//! `POST /attention`: clients report whether a person is attending, and an active
//! report from a client viewing a session cancels that session's pending
//! escalation (docs/web-push.md).

use super::*;

fn body(client: &str, session: Option<&str>, active: bool) -> String {
    serde_json::json!({ "client": client, "session": session, "active": active }).to_string()
}

/// A malformed report is a `400` naming the problem; a good one is a `204`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_route_accepts_a_report_and_rejects_a_malformed_one() {
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let server = host_server(&host).await;

    server
        .post("/attention")
        .with_body(body("c1", None, true))
        .await
        .assert_status(204);
    assert!(host.attention.attentive());

    for bad in [
        "not json".to_string(),
        "{}".to_string(),
        body("", None, true),
        r#"{"client":"c1","session":null,"active":"yes"}"#.to_string(),
    ] {
        let conn = server.post("/attention").with_body(bad.clone()).await;
        conn.assert_status(400);
        conn.assert_json_body_with(|body: &Value| {
            assert_eq!(body["code"], "bad_request", "{bad}");
            assert!(body["message"].as_str().unwrap().contains("attention"));
        });
    }
}

/// A moment held for an attended host is cancelled by an active report from a
/// client viewing that session, and by nothing else: not an inactive report, not a
/// report about another session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_an_active_report_viewing_the_session_cancels_its_escalation() {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let root = std::env::temp_dir().join(format!("efference-attention-{nonce:016x}"));
    let root = std::fs::create_dir_all(root.join(".efference/one"))
        .and_then(|()| root.canonicalize())
        .unwrap();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let boot = runtime::boot_in(&root, &root.join(".efference/one"));
    host.adopt("one".into(), SessionRuntime::start(boot, host.swansong()))
        .unwrap();
    let session = host.session("one").unwrap();
    let server = host_server(&host).await;

    let report = |session: Option<&'static str>, active: bool| {
        let server = &server;
        async move {
            server
                .post("/attention")
                .with_body(body("c1", session, active))
                .await
                .assert_status(204);
        }
    };

    // Attended, so the moment is held.
    report(None, true).await;
    session.push("Gate", "body".into());
    assert!(session.pending_escalation().is_some());

    report(Some("one"), false).await;
    assert!(session.pending_escalation().is_some(), "inactive does not");
    report(Some("two"), true).await;
    assert!(session.pending_escalation().is_some(), "another session");
    report(None, true).await;
    assert!(session.pending_escalation().is_some(), "no session open");

    report(Some("one"), true).await;
    assert!(session.pending_escalation().is_none(), "seen");

    // A report that predates a moment does not cancel the next one: nothing is
    // remembered, only what is pending is cancelled.
    session.push("Gate", "again".into());
    assert!(session.pending_escalation().is_some());

    host.swansong().shut_down().await;
    let _ = std::fs::remove_dir_all(&root);
}
