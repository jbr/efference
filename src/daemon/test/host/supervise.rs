//! `PATCH /sessions/:session {"supervisor"}`: setting and clearing a running
//! session's supervisor over the host route (docs/control-api.md).

use super::*;

/// A host serving `a` and `b` in one project, both human roots.
async fn hosting_two(roots: &Roots) -> (Arc<Host>, TestServer<impl trillium::Handler>) {
    let host = Host::new(one_connection_registry(), baseline_env());
    for id in ["a", "b"] {
        let session_dir = roots.0.join(".efference").join(id);
        std::fs::create_dir_all(&session_dir).expect("the session dir exists");
        host.adopt(
            id.into(),
            SessionRuntime::start(runtime::boot_in(&roots.0, &session_dir), host.swansong()),
        )
        .expect("the id is free in this test");
    }
    let server = host_server(&host).await;
    (host, server)
}

/// A string sets the pointer and `null` clears it, each answering the pointer as it
/// now reads; a session that is not hosted is a `404`; a refused setting, a patch
/// combining `supervisor` with `state`, and a patch with neither are `400`s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisor_patch_sets_clears_and_refuses() {
    let roots = Roots::new();
    let (host, server) = hosting_two(&roots).await;

    let body = server
        .patch("/sessions/a")
        .with_json_body(&serde_json::json!({ "supervisor": "b" }))
        .await
        .assert_status(200)
        .body()
        .to_string();
    let answer = json(&body);
    assert_eq!(answer["id"], "a");
    assert_eq!(answer["state"], "active");
    assert_eq!(answer["supervisor"], "b");
    assert_eq!(host.supervisor_of("a").as_deref(), Some("b"));

    for (body, reason) in [
        (serde_json::json!({ "supervisor": "a" }), "would cycle"),
        (serde_json::json!({ "supervisor": "ghost" }), "no session"),
        (
            serde_json::json!({ "supervisor": "b", "state": "archived" }),
            "separate patches",
        ),
        (serde_json::json!({}), "nothing to patch"),
    ] {
        server
            .patch("/sessions/b")
            .with_json_body(&body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    server
        .patch("/sessions/ghost")
        .with_json_body(&serde_json::json!({ "supervisor": "b" }))
        .await
        .assert_status(404);

    let body = server
        .patch("/sessions/a")
        .with_json_body(&serde_json::json!({ "supervisor": null }))
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert!(json(&body)["supervisor"].is_null());
    assert_eq!(host.supervisor_of("a"), None);
    assert_eq!(host.ids(), ["a", "b"], "no patch here archived anything");

    host.swansong().shut_down().await;
}
