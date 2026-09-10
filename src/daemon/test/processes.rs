//! Tests for the `/processes` resource routes (`http.rs`, docs/control-api.md,
//! docs/supervisor.md).

use super::*;

/// The listing, the single-resource read, and the cursor output read over the real
/// router: a spawned background process is listed with its flags, and its retained
/// output seeds a cold reader (no cursor) then advances a cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn processes_listing_and_output_read() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("echo seeded; sleep 5".into(), None)
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
    for (key, value) in procs[0].as_object().expect("an object") {
        assert_eq!(
            single.get(key),
            Some(value),
            "the single read speaks the listing's vocabulary for {key}"
        );
    }
    assert_eq!(single["running"], true);
    assert_eq!(
        single["exit"],
        Value::Null,
        "a running process claims no exit"
    );

    let missing = app.get("/v1/processes/99").await;
    missing.assert_status(404);
    // Routes take the bare ordinal, never the p# rendering.
    app.get("/v1/processes/p%233").await.assert_status(400);

    shared.kill_all_bg();
}

/// A minted handle stays addressable at one URL for the life of the session
/// (docs/supervisor.md): the live registry answers while the process runs and
/// after it exits, and a log fold answers when the registry never had it — which
/// is what a recorded session and a restarted daemon are.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_minted_handle_resolves_after_the_exit_and_after_the_daemon() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("true".into(), Some("toolu_abc".into()))
        .expect("spawn");
    let app = test_server(&shared).await;

    // Exited, same daemon: the registry still holds the entry, now with its exit.
    let mut tries = 0;
    let exited = loop {
        let v: Value = serde_json::from_str(app.get("/v1/processes/1").await.body()).unwrap();
        if v["running"] == false {
            break v;
        }
        tries += 1;
        assert!(tries < 200, "the process never exited: {v}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(exited["command"], "true");
    assert_eq!(exited["call_id"], "toolu_abc");
    assert_eq!(exited["exit"]["outcome"], serde_json::json!({"code": 0}));
    // The listing is unchanged: an exit is still how a process leaves it.
    let listing: Value = serde_json::from_str(app.get("/v1/processes").await.body()).unwrap();
    assert_eq!(listing.as_array().expect("an array").len(), 0);

    // Log-only: a second daemon over the same entries has an empty registry, so
    // everything it answers came from the fold.
    let entries = shared.snapshot();
    assert!(
        entries
            .iter()
            .any(|e| matches!(&e.payload, Payload::ProcessEvent(p) if p.id == 1)),
        "the lifecycle records reached the log"
    );
    let (replay, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::in_memory_with(entries),
    );
    let app = test_server(&replay).await;
    let v: Value = serde_json::from_str(app.get("/v1/processes/1").await.body()).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["command"], "true");
    assert_eq!(v["call_id"], "toolu_abc");
    assert_eq!(v["running"], false);
    assert_eq!(v["exit"]["outcome"], serde_json::json!({"code": 0}));
    assert_eq!(
        v["spawned_at"], exited["spawned_at"],
        "the record's spawn stamp is the process's, not the entry's"
    );
    // A handle nothing ever minted is a clean 404, not an error.
    app.get("/v1/processes/9").await.assert_status(404);
}

/// `PATCH {muted}` applies synchronously both ways and answers with the
/// representation; the process stays running and listed (flagged, not filtered).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_mutes_and_unmutes() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("sleep 5".into(), None)
        .expect("spawn");
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
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let fg = shared.register_foreground("cargo build".into(), Some(1234), None);
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
    shared
        .spawn_background("sleep 5".into(), None)
        .expect("spawn");
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
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("sleep 5".into(), None)
        .expect("spawn");
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
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("sleep 30".into(), None)
        .expect("spawn");
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
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let client = test_client(&shared).await;

    assert_eq!(
        client.background(None).await.unwrap(),
        None,
        "nothing mid-tool → no-op"
    );

    let fg = shared.register_foreground("cargo build".into(), Some(1234), None);
    assert_eq!(
        client.background(Some("keep going")).await.unwrap(),
        Some(fg)
    );
    assert_eq!(shared.take_detach_note().as_deref(), Some("keep going"));
}

/// The call↔process mapping reaches the wire (docs/control-api.md): the `tool_use`
/// id that spawned a process is on its `/processes` representation, so a client
/// holding the transcript entry can read `GET /processes/:id/output` for a
/// backgrounded command, and a client holding the process row can jump to the entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_carries_the_call_that_spawned_it() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("sleep 5".into(), Some("toolu_abc".into()))
        .expect("spawn");
    let fg = shared.register_foreground("cargo build".into(), Some(1234), None);
    let app = test_server(&shared).await;

    let listing: Value = serde_json::from_str(app.get("/v1/processes").await.body()).unwrap();
    let procs = listing.as_array().expect("a JSON array");
    assert_eq!(procs[0]["call_id"], "toolu_abc");
    assert_eq!(
        procs[1]["call_id"],
        Value::Null,
        "a process registered outside a call says so rather than inventing an id"
    );
    let _ = fg;
    shared.kill_all_bg();
}

/// A watch fires into the inbox (docs/supervisor.md): the drain scan matches a line
/// of a live background process and posts an ambient notification, which is what
/// wakes an idle agent. No slot — a hit must not supersede, or be superseded by, the
/// process's exit notification on the `p#<n>` slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_hit_reaches_the_inbox_as_a_notification() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("sleep 0.2; echo ready to serve; sleep 5".into(), None)
        .expect("spawn");
    assert_eq!(
        shared.watch_bg("p#1", "ready".into()),
        Some(1),
        "registration is immediate and non-blocking"
    );
    assert_eq!(
        shared.watch_bg("p#99", "ready".into()),
        None,
        "an unknown handle is refused rather than silently armed"
    );

    let mut tries = 0;
    let body = loop {
        let pending = shared.pending_notifications();
        if let Some(n) = pending
            .iter()
            .find(|n| n.body.contains("matched the watch"))
        {
            assert_eq!(n.source, "p#1");
            assert_eq!(n.slot, None, "a once-firing hit supersedes nothing");
            break n.body.clone();
        }
        tries += 1;
        assert!(tries < 200, "the watch never fired: {pending:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(body.contains("ready to serve"), "{body}");

    shared.kill_all_bg();
}
