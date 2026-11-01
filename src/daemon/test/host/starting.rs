//! A session is `starting` from its reservation until it is adopted
//! (docs/control-api.md, docs/resumption.md): listed and looked up as such, taking
//! turns, refusing every other verb, and ending in a started session, a failure that
//! hands its turns back, or an archive that leaves nothing.

use super::*;
use futures_lite::StreamExt;

/// A hook that holds the preparation until `gate` exists, then fails.
fn gated_hook(gate: &Path) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "while [ ! -e {} ]; do sleep 0.05; done; exit 7",
            gate.display()
        ),
    ]
}

/// A hook that outlasts the test, so the session stays starting until something
/// cancels it.
pub(super) fn endless_hook() -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "sleep 30".to_string(),
    ]
}

/// Every frame on `events` until it ends, as parsed JSON.
async fn frames_to_end(events: &mut trillium_client::sse::EventStream) -> Vec<Value> {
    let mut frames = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .expect("the stream ends within the timeout");
        match next {
            Some(event) => frames.push(json(event.expect("the event parsed").data())),
            None => return frames,
        }
    }
}

/// Frames from `events` until one satisfies `done`, which is returned.
async fn frame_where(
    events: &mut trillium_client::sse::EventStream,
    done: impl Fn(&Value) -> bool,
) -> Value {
    loop {
        let (_, _, data) = next_event(events).await;
        let frame = json(&data);
        if done(&frame) {
            return frame;
        }
    }
}

/// A turn sent while a session is starting is queued there, and once the session
/// is adopted it is the first turn the session runs; a stream opened while starting
/// carries on into the session's own, where `starting` clears. Meanwhile the session
/// is listed and looked up as `starting`, and every other verb answers `409
/// starting`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_sent_while_starting_is_the_first_turn_the_session_runs() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let cwd = roots.0.join("one");
    let starting = host.reserve_starting("early", &cwd);

    let listing = json(server.get("/sessions").await.body());
    assert_eq!(listing[0]["id"], "early");
    assert_eq!(listing[0]["status"], "starting");
    let row = server.get("/sessions/early").await;
    row.assert_status(200);
    assert_eq!(
        json(row.body())["status"],
        "starting",
        "a lookup is not a 404"
    );

    let client = host_client(&host, "early").await;
    let mut events = client.events(Some(0), Some("log,state")).await.unwrap();
    let first = frame_where(&mut events, |frame| frame["kind"] == "starting").await;
    assert_eq!(first["data"]["stage"], "environment");
    assert_eq!(first["data"]["failure"], Value::Null);

    server
        .post("/sessions/early/v1/turns")
        .with_json_body(&serde_json::json!({"text": "first words", "submission_id": "s1"}))
        .await
        .assert_status(202);
    let state = json(server.get("/sessions/early/v1/state").await.body());
    assert_eq!(state["queued"][0]["text"], "first words");
    assert_eq!(state["verbs"]["turn"]["delivery"], "queued");
    assert_eq!(state["verbs"]["continue"]["accepted"], false);
    let refused = server.post("/sessions/early/v1/loop/continue").await;
    refused.assert_status(409);
    assert_eq!(json(refused.body())["code"], "starting");
    server
        .get("/sessions/early/v1/log?tail=10")
        .await
        .assert_status(409);

    // Adopted as a preparation adopts.
    let runtime = runtime::started(
        runtime::boot_in(&cwd, &cwd.join(".efference/early")),
        host.swansong(),
    )
    .await;
    let session = Arc::clone(runtime.session());
    host.adopt_prepared("early".into(), runtime, None, false, Some(&starting))
        .expect("the starting entry gives way to the session");
    assert!(starting.started(Arc::clone(&session)));

    let cleared = frame_where(&mut events, |frame| frame["kind"] == "starting").await;
    assert_eq!(
        cleared["data"],
        Value::Null,
        "the session's own stream clears it"
    );
    let user = frame_where(&mut events, |frame| frame["kind"] == "user").await;
    assert_eq!(user["data"]["submission_id"], "s1");
    let first_user = session
        .snapshot()
        .into_iter()
        .find(|entry| matches!(entry.payload, Payload::User(_)))
        .expect("the queued turn ran");
    assert_eq!(first_user.submission_id.as_deref(), Some("s1"));
    assert_eq!(
        json(server.get("/sessions/early").await.body())["status"],
        "running",
        "listed by what the loop is doing once it has started",
    );

    host.swansong().shut_down().await;
}

/// A preparation that fails hands back the turns sent while starting, with the
/// reason, on the session's state and as the last frame of its stream. A creation
/// leaves no directory and gives its id back, and a turn sent after the failure is
/// refused with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_preparation_hands_back_the_turns_sent_while_starting() {
    let roots = Roots::new();
    let gate = roots.0.join("gate");
    let host = Host::new(
        registry_with_hook(&gated_hook(&gate))
            .with_history(crate::history::History::at(roots.0.join("state.db"))),
        marked_baseline(),
    );
    let register = crate::session_ids::SessionIds::at(roots.0.join("state.db"));
    let server = host_server(&host).await;
    let cwd = roots.0.join("one");

    server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&cwd))
        .await
        .assert_status(201);
    let client = host_client(&host, "fresh").await;
    let mut events = client
        .events(Some(0), Some("state:starting"))
        .await
        .unwrap();
    server
        .post("/sessions/fresh/v1/turns")
        .with_json_body(&serde_json::json!({"text": "do the thing", "submission_id": "s1"}))
        .await
        .assert_status(202);
    std::fs::write(&gate, "").unwrap();

    let state = settled_state(&server, "fresh").await;
    assert!(failure_reason(&state).contains("exited 7"), "{state}");
    let withdrawn = &state["starting"]["failure"]["withdrawn"];
    assert_eq!(withdrawn[0]["text"], "do the thing", "{state}");
    assert_eq!(state["queued"], serde_json::json!([]));
    assert_eq!(state["status"]["condition"]["kind"], "failed_to_start");
    assert!(
        state["status"]["condition"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("exited 7")),
        "{state}"
    );

    let frames = frames_to_end(&mut events).await;
    let last = frames.last().expect("frames before the end");
    assert_eq!(
        last["data"]["failure"]["withdrawn"][0]["submission_id"], "s1",
        "the stream ends on the failure: {frames:?}"
    );

    assert!(host.ids().is_empty(), "the failed session left the map");
    assert!(
        !cwd.join(".efference/fresh").exists(),
        "and minted no directory"
    );
    assert_eq!(register.holder("fresh"), None, "and gave its id back");
    let refused = server
        .post("/sessions/fresh/v1/turns")
        .with_json_body(&serde_json::json!({"text": "again"}))
        .await;
    refused.assert_status(409);
    assert!(
        refused.body().contains("did not start"),
        "{}",
        refused.body()
    );

    host.swansong().shut_down().await;
}

/// Archiving a starting creation cancels its preparation — the hook here would
/// otherwise run for half a minute — and leaves nothing: no entry, no directory, no
/// reserved id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_a_starting_creation_cancels_it_and_leaves_nothing() {
    let roots = Roots::new();
    let host = Host::new(
        registry_with_hook(&endless_hook())
            .with_history(crate::history::History::at(roots.0.join("state.db"))),
        marked_baseline(),
    );
    let register = crate::session_ids::SessionIds::at(roots.0.join("state.db"));
    let server = host_server(&host).await;
    let cwd = roots.0.join("one");

    server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&cwd))
        .await
        .assert_status(201);
    let starting = host
        .starting("fresh")
        .expect("starting while the hook runs");
    assert_eq!(register.holder("fresh"), Some(cwd.clone()));

    let archived = tokio::time::timeout(Duration::from_secs(10), server.delete("/sessions/fresh"))
        .await
        .expect("the archive does not wait out the hook");
    archived.assert_status(200);
    assert_eq!(json(archived.body())["state"], "discarded");
    assert!(
        starting
            .finished()
            .await
            .is_err_and(|reason| reason.contains("archived"))
    );
    assert!(host.ids().is_empty());
    assert!(!cwd.join(".efference/fresh").exists());
    assert_eq!(register.holder("fresh"), None);
    server.get("/sessions/fresh").await.assert_status(404);

    host.swansong().shut_down().await;
}

/// A resume that fails leaves the session dormant, listed with why the resume
/// failed — still resumable, unlike a row with a `problem` — and archiving one still
/// starting ends its log in `Archived`, so it reads as archived rather than interrupted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_resume_is_listed_dormant_with_its_reason() {
    let roots = Roots::new();
    let root = roots.0.join("one");
    // No `config.toml` beside the log, which `boot::resume` refuses.
    archived::write_session(&root, "dormant", "Dormant work");
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;

    server
        .patch("/sessions/dormant")
        .with_json_body(&serde_json::json!({"state": "active", "cwd": root, "env": {"HOME": "/h"}}))
        .await
        .assert_status(200);
    let state = settled_state(&server, "dormant").await;
    assert!(
        failure_reason(&state).contains("config.toml is missing"),
        "{state}"
    );

    let rows = json(
        server
            .get(&format!(
                "/sessions?state=archived&cwd={}",
                root.to_string_lossy().replace('/', "%2F")
            ))
            .await
            .body(),
    );
    assert_eq!(rows[0]["id"], "dormant");
    assert_eq!(rows[0]["state"], "archived");
    assert_eq!(
        rows[0]["problem"],
        Value::Null,
        "the session itself reads fine"
    );
    assert!(
        rows[0]["resume_failure"]
            .as_str()
            .is_some_and(|reason| reason.contains("config.toml is missing")),
        "{rows}"
    );
    assert!(host.ids().is_empty());

    // Archived while starting: its log ends in `Archived`.
    let hooked = Host::new(registry_with_hook(&endless_hook()), marked_baseline());
    let hooked_server = host_server(&hooked).await;
    hooked_server
        .patch("/sessions/dormant")
        .with_json_body(&serde_json::json!({"state": "active", "cwd": root}))
        .await
        .assert_status(200);
    let archived = hooked_server.delete("/sessions/dormant").await;
    archived.assert_status(200);
    assert_eq!(json(archived.body())["state"], "archived");
    let entries = crate::record::persist::load(root.join(".efference/dormant/log.jsonl"))
        .expect("the log reads");
    assert!(
        matches!(
            entries.last().map(|e| &e.payload),
            Some(Payload::Archived { .. })
        ),
        "the archive is the log's last entry"
    );

    hooked.swansong().shut_down().await;
    host.swansong().shut_down().await;
}
