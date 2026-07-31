use super::*;
use std::sync::{Arc, Mutex};
use trillium::Conn;
use trillium_router::{Router, RouterConnExt};

/// A stub daemon over a real UDS: records the bodies it received and answers
/// the handful of paths the client drives. Proves the `UnixClientConfig`
/// connector + request/response plumbing end to end without the live API or the
/// real agent loop — and exercises the same `trillium-router` the daemon uses.
/// Carried in trillium's server-global state, exactly like the real `Shared`.
#[derive(Clone, Default)]
struct Recorder {
    turns: Arc<Mutex<Vec<String>>>,
    approvals: Arc<Mutex<Vec<(String, String)>>>,
    interrupts: Arc<Mutex<u32>>,
}

fn rec_of(conn: &Conn) -> Recorder {
    conn.shared_state::<Recorder>().cloned().unwrap_or_default()
}

fn stub_router() -> Router {
    Router::new()
        .post("/turn", |mut conn: Conn| async move {
            let rec = rec_of(&conn);
            let body = conn.request_body_string().await.unwrap_or_default();
            rec.turns.lock().unwrap().push(body);
            conn.with_status(202).with_body(r#"{"status":"accepted"}"#)
        })
        .post("/approvals/:call_id", |mut conn: Conn| async move {
            let rec = rec_of(&conn);
            let id = conn.param("call_id").unwrap_or_default().to_string();
            // Stand in for "no such gate" so the client's non-2xx path is tested.
            if id == "missing" {
                return conn
                    .with_status(404)
                    .with_body(r#"{"error":"no pending approval with that id"}"#);
            }
            let body = conn.request_body_string().await.unwrap_or_default();
            rec.approvals.lock().unwrap().push((id, body));
            conn.with_status(200).with_body(r#"{"status":"resolved"}"#)
        })
        .post("/interrupt", |conn: Conn| async move {
            *rec_of(&conn).interrupts.lock().unwrap() += 1;
            conn.with_status(200)
                .with_body(r#"{"status":"interrupted"}"#)
        })
        .get("/state", |conn: Conn| async move {
            conn.with_status(200)
                .with_body(r#"{"last_seq":3,"pending_approval":null}"#)
        })
        .get("/log", |conn: Conn| async move {
            // Echo the querystring so the test can assert `since` made the trip.
            let q = conn.querystring().to_string();
            conn.with_status(200).with_body(format!(r#"{{"q":"{q}"}}"#))
        })
}

/// Bind the stub on a scratch UDS, returning its path. The socket file appears
/// synchronously on `bind_uds`, so a client may dial immediately.
fn spawn_stub(rec: Recorder) -> PathBuf {
    // Darwin limits Unix-socket pathnames to `SUN_LEN` (104 bytes). The harness
    // supplies a deliberately long per-session `$TMPDIR`, so tests bind under
    // the system's short `/tmp` instead of `temp_dir()`.
    let dir = PathBuf::from("/tmp");
    // A per-test unique-ish name without Date/rand: use the Arc pointer.
    let nonce = Arc::as_ptr(&rec.turns) as usize;
    let socket = dir.join(format!("eff-client-{nonce}.sock"));
    let _ = std::fs::remove_file(&socket);
    trillium_tokio::config()
        .with_shared_state(rec)
        .listeners()
        .bind_uds(&socket)
        .expect("bind stub uds")
        .spawn(stub_router());
    socket
}

#[tokio::test]
async fn verbs_round_trip_over_the_unix_connector() {
    let rec = Recorder::default();
    let socket = spawn_stub(rec.clone());
    let client = Client::new(&socket);

    client.turn("hello world").await.unwrap();
    client
        .approve("toolu_7", &Approval::Redirect("try the other file".into()))
        .await
        .unwrap();
    client.interrupt().await.unwrap();

    let state = client.state().await.unwrap();
    assert!(state.contains(r#""last_seq":3"#));

    let log = client.log(Some(12)).await.unwrap();
    assert!(
        log.contains("since=12"),
        "since should reach the daemon: {log}"
    );

    // The stub saw exactly what the client claims to have sent.
    let turns = rec.turns.lock().unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].contains("hello world"));
    let approvals = rec.approvals.lock().unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].0, "toolu_7");
    assert!(approvals[0].1.contains("redirect"));
    assert!(approvals[0].1.contains("try the other file"));
    assert_eq!(*rec.interrupts.lock().unwrap(), 1);

    let _ = std::fs::remove_file(&socket);
}

/// Regression test for the UTF-8 mis-decode mojibake (backlog/dogfooding-friction.md):
/// `conn.request_body_string()` — the same call the real daemon's `/turn` handler
/// uses — decodes the body according to the request's `Content-Type` charset,
/// defaulting away from UTF-8 when none is given. `Client::post` must pin
/// `charset=utf-8` so non-ASCII text (em-dashes, smart quotes, accents) survives
/// the real Unix-socket round trip byte-for-byte instead of coming out as
/// `â€"`-style mojibake.
#[tokio::test]
async fn non_ascii_turn_round_trips_cleanly() {
    let rec = Recorder::default();
    let socket = spawn_stub(rec.clone());
    let client = Client::new(&socket);

    let text = "em dash — smart “quotes” — café";
    client.turn(text).await.unwrap();

    let turns = rec.turns.lock().unwrap();
    assert_eq!(turns.len(), 1);
    assert!(
        turns[0].contains(text),
        "non-ASCII text should survive the round trip verbatim: {:?}",
        turns[0]
    );

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn non_2xx_surfaces_the_daemon_error_body() {
    let rec = Recorder::default();
    let socket = spawn_stub(rec.clone());
    let client = Client::new(&socket);

    // The stub answers /approvals/missing with a 404 + JSON error body.
    let err = client
        .approve("missing", &Approval::Approve)
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("404"), "status in error: {msg}");
    assert!(msg.contains("no pending approval"), "body in error: {msg}");

    let _ = std::fs::remove_file(&socket);
}
