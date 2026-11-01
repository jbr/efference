//! `PATCH /sessions/:session`: the two state transitions on a session resource.
//!
//! `active` is the resume — a dormant session reopened under its own id — and
//! `archived` is what `DELETE` does, spelled as the change it is. The refusals
//! here are the ones the route can answer before a log is opened: a body that
//! patches nothing, a name that is not a plain path segment, a relative working
//! root, a missing `cwd`, and a root with no such session. Each is stated on both
//! the Unix and the network listener.
//!
//! What a resume *does* — reopening the log and starting the session — is
//! `boot::resume`'s to answer, and a booted session reaches a provider, so no
//! test here gets past the refusals.

use super::*;
use serde_json::Value;

/// A session directory that exists but holds no log: enough for the route to
/// find it, not enough to boot. The refusals under test all happen before the
/// log is read.
fn dormant(roots: &Roots, name: &str) -> PathBuf {
    let dir = roots.0.join(".efference").join(name);
    std::fs::create_dir_all(&dir).expect("the session dir exists");
    dir
}

/// An `active` patch in `cwd`. The session is named by the route, not the body.
fn resume(cwd: &Path) -> Value {
    serde_json::json!({
        "state": "active", "cwd": cwd, "env": {"HOME": "/h"},
    })
}

/// A host serving one live session named `one`, rooted at `roots.0/one`.
async fn hosting_one(roots: &Roots) -> (Arc<Host>, TestServer<impl trillium::Handler>) {
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt(
        "one".into(),
        runtime::started(runtime::boot(&roots.0.join("one")), host.swansong()).await,
    )
    .expect("the id is free in this test");
    let server = host_server(&host).await;
    (host, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_patch_validates_its_body_on_every_listener() {
    let roots = Roots::new();
    let (host, server) = hosting_one(&roots).await;
    dormant(&roots, "dormant");
    let cases: Vec<(String, Value, &str)> = vec![
        (
            "one".into(),
            serde_json::json!(["not", "an", "object"]),
            "invalid body",
        ),
        // A state word the enum does not carry, rather than a free-form string:
        // "ended" was the word `DELETE` used to answer with, and it is gone.
        (
            "one".into(),
            serde_json::json!({"state": "ended"}),
            "invalid body",
        ),
        // The switch fields take the shapes creation takes.
        (
            "one".into(),
            serde_json::json!({"state": "active", "triage": {"mode": "sideways"}}),
            "invalid body",
        ),
        // Patching nothing is refused rather than read as agreement.
        (
            "one".into(),
            serde_json::json!({"env": {"HOME": "/h"}}),
            "nothing to patch",
        ),
        // `cwd` is not optional on the transition that needs it.
        (
            "dormant".into(),
            serde_json::json!({"state": "active"}),
            "needs `cwd`",
        ),
        // A name that is not a single plain segment reaches the route only when it
        // survives URL normalization: `../up` is collapsed by the HTTP layer before
        // it is routed, so the encoded form is what actually reaches the check, and
        // decoding to a slash is what it refuses. The check is defense in depth
        // either way — the guard below the route is the codebase's standard one for
        // an id that becomes a path.
        (
            "a%2Fb".into(),
            resume(&roots.0),
            "not a single plain path segment",
        ),
        (
            "fresh".into(),
            resume(Path::new("relative")),
            "cwd must be absolute",
        ),
        // A name nothing has ever held in this root: an unknown session is
        // refused rather than given one, and the path is named so the operator
        // can see which root was searched.
        ("never".into(), resume(&roots.0), "no session"),
    ];
    for (id, body, reason) in &cases {
        server
            .patch(&format!("/sessions/{id}"))
            .with_json_body(body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    assert_eq!(
        host.ids(),
        ["one"],
        "a refused patch starts nothing and adopts nothing"
    );

    // The same rejections on a network listener: same status, same reasons.
    let (network, bearer, _state) = network_host_server(&host).await;
    for (id, body, reason) in &cases {
        network
            .patch(&format!("/sessions/{id}"))
            .with_request_header(KnownHeaderName::Authorization, bearer.clone())
            .with_json_body(body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    assert_eq!(host.ids(), ["one"]);

    host.swansong().shut_down().await;
}

/// A repeat `active` patch is the state the caller asked for, so it answers 200
/// like the first did — and says in `resumed` that this call was not what made
/// the session live. A client retrying after a dropped connection reads the same
/// answer either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_active_patch_on_a_live_session_answers_the_state_it_asked_for() {
    let roots = Roots::new();
    let (host, server) = hosting_one(&roots).await;

    let body = server
        .patch("/sessions/one")
        .with_json_body(&resume(&roots.0.join("one")))
        .await
        .assert_status(200)
        .body()
        .to_string();
    let answer = json(&body);
    assert_eq!(answer["state"], "active", "the state it asked for");
    assert_eq!(
        answer["resumed"], false,
        "and this call did not reopen anything"
    );

    host.swansong().shut_down().await;
}

/// The exception to that: the same id under a root the host is not serving it
/// from is not a repeat, it is a different session that happens to share a name.
/// Claiming it would attach a client to the wrong one, so it is a `409` — which a
/// `404` would read as "gone" and a `400` as "you sent that wrong".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_active_patch_under_another_root_is_a_conflict() {
    let roots = Roots::new();
    let (host, server) = hosting_one(&roots).await;
    std::fs::create_dir_all(roots.0.join("other/.efference/one")).expect("the dir exists");

    server
        .patch("/sessions/one")
        .with_json_body(&resume(&roots.0.join("other")))
        .await
        .assert_status(409)
        .assert_body_contains("is hosted under");

    host.swansong().shut_down().await;
}

/// Archiving is `DELETE` under its other name, and the two must not drift: same
/// teardown, and one state word between them. `DELETE` answers `archived`, and
/// patching a session that is not hosted is a 404 rather than a bad request —
/// the difference between "I can't do that" and "there is nothing there".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archived_and_delete_are_one_transition() {
    let roots = Roots::new();
    let (host, server) = hosting_one(&roots).await;

    let body = server
        .delete("/sessions/one")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert_eq!(
        json(&body)["state"],
        "archived",
        "DELETE answers the same state word PATCH takes, not the old one"
    );

    // Patching the same id is now a 404: it was, and no longer is, hosted.
    server
        .patch("/sessions/one")
        .with_json_body(&serde_json::json!({"state": "archived"}))
        .await
        .assert_status(404)
        .assert_body_contains("no hosted session");

    host.swansong().shut_down().await;
}

/// The rule from docs/log-and-projection.md: preparing a resume must never roll
/// back the session directory. Every refusal here happens with that directory
/// present, and it is still there afterwards, still empty of a log — nothing this
/// route did minted a session dir, minted an id, or wrote where the log will go.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_resume_leaves_the_session_directory_alone() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let dir = dormant(&roots, "dormant");

    // Unknown name in this root: refused before the directory is consulted.
    server
        .patch("/sessions/dormant")
        .with_json_body(&serde_json::json!({"state": "active", "cwd": roots.0.join("elsewhere")}))
        .await
        .assert_status(400);
    // A log that cannot be opened — boot's refusal, reached after the route found the
    // directory, so the patch answers and the failure is the starting session's.
    server
        .patch("/sessions/dormant")
        .with_json_body(&resume(&roots.0))
        .await
        .assert_status(200);
    let state = settled_state(&server, "dormant").await;
    assert!(
        failure_reason(&state).contains("could not resume the session"),
        "{state}"
    );
    assert!(dir.is_dir(), "the session directory survives a refusal");
    assert!(
        !dir.join("log.jsonl").exists(),
        "and no log was created where one would go"
    );
    assert_eq!(host.ids(), Vec::<String>::new());

    host.swansong().shut_down().await;
}

/// The route is registered ahead of the `/sessions/:session/*` wildcard, which
/// would otherwise resolve the session into conn state first and 404 a patch
/// that is what makes the session live. This session is hosted under the name a
/// client would try to make active; the route still answers as the patch it is,
/// rather than as the wildcard's 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_patch_route_is_not_shadowed_by_the_session_wildcard() {
    let roots = Roots::new();
    let (host, server) = hosting_one(&roots).await;

    server
        .patch("/sessions/one")
        .with_json_body(&resume(&roots.0.join("one")))
        .await
        .assert_status(200);

    host.swansong().shut_down().await;
}

/// An archive releases the log's writer lock before it answers, even while an SSE
/// stream still holds the session: the stream used to keep the `Session`, and with
/// it the lock, alive until the client disconnected, so a resume right after the
/// archive was refused as "another writer holds this log open". The stream itself
/// ends once the session's log is closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_releases_the_log_while_a_stream_holds_the_session() {
    use futures_lite::StreamExt;
    let roots = Roots::new();
    let cwd = roots.0.join("one");
    std::fs::create_dir_all(&cwd).unwrap();
    let path = cwd.join("log.jsonl");
    let mut boot = runtime::boot(&cwd);
    boot.log = Log::create(&path, None).expect("a fresh log");
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt("one".into(), runtime::started(boot, host.swansong()).await)
        .expect("the id is free in this test");
    let session = host.session("one").expect("hosted");
    let mut stream = control_stream(Arc::clone(&session), Include::default(), 0);
    drop(session);

    host.end("one").expect("hosted").await;

    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    // Polled briefly rather than tried once: a test elsewhere in this process that forks a
    // child holds a copy of every open descriptor until the child's exec closes them, and
    // the lock rides the descriptor. Without the fix the lock is held for as long as the
    // stream is, so a short poll still fails it.
    let mut released = false;
    for _ in 0..50 {
        if file.try_lock().is_ok() {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        released,
        "the archive released the writer lock while the stream was open"
    );
    // The stream drains what was logged, then ends rather than waiting forever.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while stream.next().await.is_some() {}
    })
    .await;
    assert!(
        ended.is_ok(),
        "the stream ends once its session's log is closed"
    );

    host.swansong().shut_down().await;
}

/// A resume takes `detached` from the header the session was created with, so a
/// detached child keeps outliving its creator without the caller repeating it
/// (docs/resumption.md).
#[tokio::test(start_paused = true)]
async fn a_resumed_child_keeps_the_detached_its_header_recorded() {
    let roots = Roots::new();
    let (host, _server) = hosting_one(&roots).await;
    let recorded = |id: &str, detached: bool, created_by: &str| {
        let dir = roots.0.join(".efference").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut metadata = crate::record::persist::SessionMetadata::new(
            id,
            &roots.0,
            crate::record::log::LoggedInference::new(
                "c",
                crate::record::log::Provider::Anthropic,
                "m",
            ),
            crate::record::log::LoggedInference::new(
                "c",
                crate::record::log::Provider::Anthropic,
                "m",
            ),
            crate::record::log::TriageModel::shared(),
        );
        metadata.created_by = Some(created_by.into());
        metadata.detached = detached;
        Log::create(
            dir.join(crate::record::session_dir::LOG_NAME),
            Some(metadata),
        )
        .unwrap();
        dir
    };

    let lineage = |dir: &Path| {
        let recorded = host.recorded_lineage(dir);
        (recorded.created_by, recorded.detached)
    };
    let kept = recorded("kept", true, "one");
    assert_eq!(lineage(&kept), (Some("one".into()), true));
    let bound = recorded("bound", false, "one");
    assert_eq!(lineage(&bound), (Some("one".into()), false));
    // A creator this host no longer holds is dropped; `detached` still reads.
    let orphan = recorded("orphan", true, "gone");
    assert_eq!(lineage(&orphan), (None, true));

    // Adopted as the resume does, the detached child outlives its creator.
    host.adopt_with(
        "kept".into(),
        runtime::started(
            runtime::boot_in(&roots.0.join("one"), &roots.0.join("one/scratch")),
            host.swansong(),
        )
        .await,
        Some("one".into()),
        host.recorded_lineage(&kept).detached,
    )
    .unwrap();
    host.end("one").unwrap().await;
    // The end resolves after the sessions that end with it have left
    // (docs/control-channel.md), so one still hosted now outlives it.
    assert!(host.session("kept").is_some());

    host.swansong().shut_down().await;
}

/// A session hosted over a fresh file log at `<roots>/<name>/log.jsonl`.
pub(super) async fn adopt_logged(
    host: &Arc<Host>,
    roots: &Roots,
    name: &str,
    created_by: Option<&str>,
) -> PathBuf {
    let cwd = roots.0.join(name);
    std::fs::create_dir_all(&cwd).unwrap();
    let path = cwd.join("log.jsonl");
    let mut boot = runtime::boot(&cwd);
    boot.log = Log::create(&path, None).expect("a fresh log");
    host.adopt_with(
        name.into(),
        runtime::started(boot, host.swansong()).await,
        created_by.map(String::from),
        false,
    )
    .expect("the id is free in this test");
    path
}

pub(super) fn last_payload(path: &Path) -> Payload {
    let recording = crate::record::persist::load_recording(path).expect("the log loads");
    recording.entries.last().expect("an entry").payload.clone()
}

/// An archive is recorded as the log's last entry, and so is the end of a child that
/// ends with its creator (docs/resumption.md). The archive resolves once both have left
/// the map, each after its record (docs/control-channel.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_and_a_cascaded_child_end_log_archived() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let creator = adopt_logged(&host, &roots, "creator", None).await;
    let child = adopt_logged(&host, &roots, "child", Some("creator")).await;
    host.session("creator")
        .unwrap()
        .append(3, Payload::User(vec![]))
        .unwrap();

    host.end("creator").expect("hosted").await;
    assert!(
        host.session("child").is_none(),
        "the child left before the archive resolved"
    );

    for path in [&creator, &child] {
        let recording = crate::record::persist::load_recording(path).unwrap();
        let last = recording.entries.last().unwrap();
        assert!(matches!(last.payload, Payload::Archived { .. }));
    }
    let recording = crate::record::persist::load_recording(&creator).unwrap();
    let last = recording.entries.last().unwrap();
    assert_eq!(
        last.turn, 3,
        "the archive keeps the turn of the entry before it"
    );

    host.swansong().shut_down().await;
}

/// A host shutdown stops its sessions without archiving them: they are what a
/// restarted host resumes. A child its creator's end cascades to is stopped the same
/// way, and both leave the map: the cascade's end holds the child's removal back only
/// until it has closed the log.
#[tokio::test(start_paused = true)]
async fn a_host_shutdown_does_not_log_archived() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let open = adopt_logged(&host, &roots, "open", None).await;
    let child = adopt_logged(&host, &roots, "child", Some("open")).await;
    for id in ["open", "child"] {
        host.session(id)
            .unwrap()
            .append(0, Payload::User(vec![]))
            .unwrap();
    }

    host.swansong().shut_down().await;
    host.wait_until("every session left the map", || host.ids().is_empty())
        .await;

    for path in [&open, &child] {
        assert!(
            matches!(last_payload(path), Payload::User(_)),
            "{}",
            path.display()
        );
    }
}

/// A dormant directory whose id the register holds under another root is a
/// different session sharing the name: refused as a clash, not resumed
/// (docs/session-title.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_active_patch_for_an_id_registered_elsewhere_is_a_conflict() {
    let roots = Roots::new();
    let host = Host::new(
        one_connection_registry()
            .with_history(crate::history::History::at(roots.0.join("state.db"))),
        baseline_env(),
    );
    let server = host_server(&host).await;
    dormant(&roots, "shared");
    crate::session_ids::SessionIds::at(roots.0.join("state.db"))
        .reserve("shared", &roots.0.join("other"))
        .expect("the id is free");

    server
        .patch("/sessions/shared")
        .with_json_body(&resume(&roots.0))
        .await
        .assert_status(409)
        .assert_body_contains("is registered under");

    host.swansong().shut_down().await;
}
