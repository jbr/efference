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
        SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong()),
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
    let (network, bearer) = network_host_server(&host).await;
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
        .with_json_body(&serde_json::json!({}))
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

    for body in [
        // A log that cannot be opened — boot's refusal, reached after the route
        // found the directory.
        resume(&roots.0),
        // Unknown name in this root: refused before the directory is consulted.
        serde_json::json!({"state": "active", "cwd": roots.0.join("elsewhere")}),
    ] {
        server
            .patch("/sessions/dormant")
            .with_json_body(&body)
            .await
            .assert_status(400);
    }
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
    boot.log = crate::persist::Log::create(&path, None).expect("a fresh log");
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt("one".into(), SessionRuntime::start(boot, host.swansong()))
        .expect("the id is free in this test");
    let session = host.session("one").expect("hosted");
    let mut stream = crate::daemon::stream::control_stream(
        Arc::clone(&session),
        crate::daemon::stream::Include::default(),
        0,
    );
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
