use super::*;
use std::sync::{Arc, Mutex};
use trillium::Conn;
use trillium_router::{Router, RouterConnExt};

/// A stub daemon over a real UDS: records the bodies it received and answers
/// the handful of paths the client drives. Proves the `UnixClientConfig`
/// connector + request/response plumbing end to end without the live API or the
/// real agent loop — and exercises the same `trillium-router` the daemon uses.
/// Carried in trillium's server-global state, exactly like the real `Session`.
#[derive(Clone, Default)]
struct Recorder {
    turns: Arc<Mutex<Vec<String>>>,
    approvals: Arc<Mutex<Vec<(String, String)>>>,
    interrupts: Arc<Mutex<u32>>,
    creations: Arc<Mutex<Vec<String>>>,
    artifacts: Arc<Mutex<Vec<String>>>,
}

fn rec_of(conn: &Conn) -> Recorder {
    conn.shared_state::<Recorder>().cloned().unwrap_or_default()
}

fn stub_router() -> Router {
    Router::new()
        .post("/v1/turns", |mut conn: Conn| async move {
            let rec = rec_of(&conn);
            let body = conn.request_body_string().await.unwrap_or_default();
            rec.turns.lock().unwrap().push(body);
            conn.with_status(202).with_body(r#"{"status":"accepted"}"#)
        })
        .put("/v1/approvals/:call_id", |mut conn: Conn| async move {
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
        .post("/v1/loop/interrupt", |conn: Conn| async move {
            *rec_of(&conn).interrupts.lock().unwrap() += 1;
            conn.with_status(200)
                .with_body(r#"{"status":"interrupted"}"#)
        })
        .get("/v1/state", |conn: Conn| async move {
            conn.with_status(200)
                .with_body(r#"{"last_seq":3,"gate":null}"#)
        })
        .get("/v1/transcript", |conn: Conn| async move {
            // Echo the querystring so the test can assert `since` made the trip.
            let q = conn.querystring().to_string();
            conn.with_status(200).with_body(format!("q={q}"))
        })
        // A host's surface: creation at the root, one session's routes under its prefix,
        // and the credential routes at the root under either base.
        .post("/sessions", |mut conn: Conn| async move {
            let rec = rec_of(&conn);
            let body = conn.request_body_string().await.unwrap_or_default();
            rec.creations.lock().unwrap().push(body);
            conn.with_status(201).with_body(r#"{"id":"minted"}"#)
        })
        .get("/sessions/minted/v1/state", |conn: Conn| async move {
            conn.with_status(200)
                .with_body(r#"{"last_seq":9,"gate":null}"#)
        })
        .get(
            "/sessions/minted/v1/artifacts/:seq/:text_block/:fence",
            |conn: Conn| async move {
                let rec = rec_of(&conn);
                let seq = conn.param("seq").unwrap_or_default().to_string();
                let text_block = conn.param("text_block").unwrap_or_default().to_string();
                let fence = conn.param("fence").unwrap_or_default().to_string();
                rec.artifacts
                    .lock()
                    .unwrap()
                    .push(format!("{seq}/{text_block}/{fence}"));
                conn.with_status(200).with_body(
                    r#"{"url":"/artifacts/token/","external_url":"http://127.0.0.1:4321/artifacts/token/"}"#,
                )
            },
        )
        .get("/auth/devices", |conn: Conn| async move {
            conn.with_status(200).with_body(
                r#"{"devices":[{"id":"00112233445566778899aabbccddeeff","kind":"script","label":"root-level","joined":"add","created_at":1,"last_seen_at":null}]}"#,
            )
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
        .approve(
            "toolu_7",
            &Approval::Redirect("try the other file".into()),
            Some("mcp[claude-code]"),
        )
        .await
        .unwrap();
    client.interrupt(&Default::default()).await.unwrap();

    let state = client.state().await.unwrap();
    assert!(state.contains(r#""last_seq":3"#));

    let transcript = client
        .transcript(TranscriptRead {
            since: Some(12),
            me: Some("session[parent]"),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        transcript, "q=since=12&me=session%5Bparent%5D",
        "since and the reader should reach the daemon"
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
    assert!(approvals[0].1.contains(r#""source":"mcp[claude-code]""#));
    assert_eq!(*rec.interrupts.lock().unwrap(), 1);

    let _ = std::fs::remove_file(&socket);
}

/// Regression test for the UTF-8 mis-decode mojibake (docs/control-channel.md):
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
        .approve("missing", &Approval::Approve, None)
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("404"), "status in error: {msg}");
    assert!(msg.contains("no pending approval"), "body in error: {msg}");

    let _ = std::fs::remove_file(&socket);
}

/// A prefixed client (docs/control-api.md): `/v1` verbs join onto `/sessions/<id>`,
/// while creation and the `/auth/*` family stay at the socket root under either base.
/// The environment map travels in the creation body and the id comes back.
#[tokio::test]
async fn a_prefixed_client_joins_v1_only() {
    let rec = Recorder::default();
    let socket = spawn_stub(rec.clone());

    let request = CreateSession {
        name: Some("minted".into()),
        options: crate::inference::SessionOptions {
            model: Some("m".into()),
            ..Default::default()
        },
        env: Some([("HOME".to_string(), "/h".to_string())].into()),
        ..CreateSession::new(PathBuf::from("/work"))
    };
    let id = Client::new(&socket).create_session(&request).await.unwrap();
    assert_eq!(id, "minted");
    let sent: serde_json::Value = serde_json::from_str(&rec.creations.lock().unwrap()[0]).unwrap();
    assert_eq!(sent["env"]["HOME"], "/h");
    assert_eq!(sent["cwd"], "/work");
    assert!(
        sent.get("config").is_none(),
        "absent knobs are omitted: {sent}"
    );

    let client = Client::at(&socket, "sessions/minted/");
    let state = client.state().await.unwrap();
    assert!(state.contains(r#""last_seq":9"#), "{state}");
    let artifact = client.resolve_artifact(12, 3, 2).await.unwrap();
    assert_eq!(artifact.url, "/artifacts/token/");
    assert_eq!(
        artifact.external_url.as_deref(),
        Some("http://127.0.0.1:4321/artifacts/token/")
    );
    assert_eq!(
        rec.artifacts.lock().unwrap().as_slice(),
        &[String::from("12/3/2")]
    );
    let devices = client.devices().await.unwrap();
    assert_eq!(
        devices.iter().map(|d| d.label.as_str()).collect::<Vec<_>>(),
        ["root-level"]
    );
    // Root-based requests do not reach the prefixed session.
    let unprefixed = Client::new(&socket).state().await.unwrap();
    assert!(unprefixed.contains(r#""last_seq":3"#), "{unprefixed}");

    let _ = std::fs::remove_file(&socket);
}

/// An allow's body names the turn that carries its note, beside the decider.
#[test]
fn an_allows_body_names_its_notes_turn() {
    let body: serde_json::Value = serde_json::from_str(&approval_body(
        &Approval::Approve,
        Some("mcp[claude-code]"),
        Some("sub-1"),
    ))
    .unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "decision": "approve",
            "source": "mcp[claude-code]",
            "note_submission": "sub-1",
        })
    );
    let bare: serde_json::Value =
        serde_json::from_str(&approval_body(&Approval::Approve, None, None)).unwrap();
    assert_eq!(bare, serde_json::json!({ "decision": "approve" }));
}
