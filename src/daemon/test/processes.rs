//! Tests for the `/processes` resource routes (`http.rs`, docs/control-api.md,
//! docs/supervisor.md).

use super::*;

/// The listing, the single-resource read, and the cursor output read over the real
/// router: a spawned background process is listed with its flags, and its retained
/// output seeds a cold reader (no cursor) then advances a cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn processes_listing_and_output_read() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared
        .spawn_background("echo seeded; sleep 5".into())
        .expect("spawn");
    let app = test_server(&shared).await;

    // Wait for the drain task to land the first chunk in the capture.
    let mut tries = 0;
    loop {
        let read = app.get("/v1/processes/1/output").await;
        let v: Value = serde_json::from_str(read.body()).unwrap();
        if v["output"].as_str().unwrap_or_default().contains("seeded") {
            // The cursor resumes after what this read returned.
            let cursor = v["cursor"].as_u64().expect("a cursor");
            assert_eq!(v["lost"], 0);
            assert_eq!(v["running"], true);
            let again = app
                .get(format!("/v1/processes/1/output?since={cursor}").as_str())
                .await;
            let v: Value = serde_json::from_str(again.body()).unwrap();
            assert_eq!(v["output"], "", "nothing new after the cursor");
            break;
        }
        tries += 1;
        assert!(tries < 100, "output never reached the capture: {v}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let listing = app.get("/v1/processes").await;
    let v: Value = serde_json::from_str(listing.body()).unwrap();
    let procs = v.as_array().expect("a JSON array");
    assert_eq!(procs.len(), 1);
    assert_eq!(procs[0]["id"], 1);
    assert_eq!(procs[0]["background"], true);
    assert_eq!(procs[0]["muted"], false);

    let one = app.get("/v1/processes/1").await;
    let single: Value = serde_json::from_str(one.body()).unwrap();
    assert_eq!(
        &single, &procs[0],
        "listing and single read share one representation"
    );

    let missing = app.get("/v1/processes/99").await;
    missing.assert_status(404);
    // Routes take the bare ordinal, never the p# rendering.
    app.get("/v1/processes/p%233").await.assert_status(400);

    shared.kill_all_bg();
}

/// `PATCH {muted}` applies synchronously both ways and answers with the
/// representation; the process stays running and listed (flagged, not filtered).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_mutes_and_unmutes() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.spawn_background("sleep 5".into()).expect("spawn");
    let app = test_server(&shared).await;

    let muted = app
        .patch("/v1/processes/1")
        .with_body(r#"{"muted":true}"#)
        .await;
    muted.assert_status(200);
    let v: Value = serde_json::from_str(muted.body()).unwrap();
    assert_eq!(v["muted"], true);

    let listing: Value = serde_json::from_str(app.get("/v1/processes").await.body()).unwrap();
    assert_eq!(
        listing[0]["muted"], true,
        "muted rides the listing as a flag"
    );

    let unmuted = app
        .patch("/v1/processes/1")
        .with_body(r#"{"muted":false}"#)
        .await;
    let v: Value = serde_json::from_str(unmuted.body()).unwrap();
    assert_eq!(v["muted"], false, "the dismiss has an inverse");

    shared.kill_all_bg();
}

/// `PATCH {background: true}` on the foreground entry is the handoff: 202 (the flip
/// lands only when the detach wake reaches the running tool), the note stashed for
/// the loop. On an already-background process it is an idempotent 200 no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_background_requests_the_handoff() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let fg = shared.register_foreground("cargo build".into(), Some(1234));
    let app = test_server(&shared).await;

    let listing: Value = serde_json::from_str(app.get("/v1/processes").await.body()).unwrap();
    assert_eq!(
        listing[0]["background"], false,
        "foreground is listed from spawn"
    );

    let patched = app
        .patch(format!("/v1/processes/{fg}").as_str())
        .with_body(r#"{"background":true,"note":"check the logs"}"#)
        .await;
    // The handoff is asynchronous: accepted, not yet applied.
    patched.assert_status(202);
    assert_eq!(
        shared.take_detach_note().as_deref(),
        Some("check the logs"),
        "the note rides the handoff for the loop to deliver"
    );

    // Already-background: nothing to do, and saying so is a success.
    shared.spawn_background("sleep 5".into()).expect("spawn");
    let idempotent = app
        .patch("/v1/processes/2")
        .with_body(r#"{"background":true}"#)
        .await;
    idempotent.assert_status(200);
    assert_eq!(shared.take_detach_note(), None, "no handoff was requested");

    shared.kill_all_bg();
}

/// The `PATCH` rejections: un-backgrounding names no transition, a note needs a
/// handoff to ride, and an unknown id is 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_rejections() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.spawn_background("sleep 5".into()).expect("spawn");
    let app = test_server(&shared).await;

    let foregrounded = app
        .patch("/v1/processes/1")
        .with_body(r#"{"background":false}"#)
        .await;
    foregrounded.assert_status(400);

    let bare_note = app
        .patch("/v1/processes/1")
        .with_body(r#"{"note":"hi"}"#)
        .await;
    bare_note.assert_status(400);

    let unknown = app
        .patch("/v1/processes/9")
        .with_body(r#"{"muted":true}"#)
        .await;
    unknown.assert_status(404);

    shared.kill_all_bg();
}

/// `DELETE /processes/:id` kills the group; the drain task then sees EOF and the
/// exit notification fires like any other exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_kills_a_running_process() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.spawn_background("sleep 30".into()).expect("spawn");
    let app = test_server(&shared).await;

    let killed = app.delete("/v1/processes/1").await;
    killed.assert_status(200);

    let mut tries = 0;
    loop {
        let pending = shared.pending_notifications();
        if pending.iter().any(|n| n.source == "p#1") {
            break;
        }
        tries += 1;
        assert!(tries < 100, "exit notification never arrived: {pending:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // An exited process is no longer a kill target.
    app.delete("/v1/processes/1").await.assert_status(404);
}

/// The client's `background` verb resolves the foreground entry from the listing and
/// patches it — and is a clean no-op when nothing is mid-tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_background_resolves_the_foreground_process() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let client = test_client(&shared).await;

    assert_eq!(
        client.background(None).await.unwrap(),
        None,
        "nothing mid-tool → no-op"
    );

    let fg = shared.register_foreground("cargo build".into(), Some(1234));
    assert_eq!(
        client.background(Some("keep going")).await.unwrap(),
        Some(fg)
    );
    assert_eq!(shared.take_detach_note().as_deref(), Some("keep going"));
}
