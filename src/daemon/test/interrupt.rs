//! The interrupt against a real turn loop and a scripted upstream
//! (docs/control-channel.md).

use super::runtime::boot;
use super::upstream_hold::is_title_ask;
use super::*;
use crate::daemon::SessionRuntime;
use crate::frontend::SubmittedTurn;
use crate::log::WireState;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use trillium_tokio::Swansong;

/// The first request's answer: one bash call, so the loop runs it and opens a triage.
const TOOL_CALL: &[(&str, &str)] = &[
    (
        "message_start",
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#,
    ),
    (
        "content_block_start",
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash","input":{}}}"#,
    ),
    (
        "content_block_delta",
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"echo hi\"}"}}"#,
    ),
    (
        "content_block_stop",
        r#"{"type":"content_block_stop","index":0}"#,
    ),
    (
        "message_delta",
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
    ),
    ("message_stop", r#"{"type":"message_stop"}"#),
];

/// Any later request's answer: a plain end of turn.
const END_TURN: &[(&str, &str)] = &[
    (
        "message_start",
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#,
    ),
    (
        "content_block_start",
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    ),
    (
        "content_block_delta",
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
    ),
    (
        "content_block_stop",
        r#"{"type":"content_block_stop","index":0}"#,
    ),
    (
        "message_delta",
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
    ),
    ("message_stop", r#"{"type":"message_stop"}"#),
];

fn chunk(sock: &mut TcpStream, events: &[(&str, &str)]) {
    let body: String = events
        .iter()
        .map(|(kind, data)| format!("event: {kind}\ndata: {data}\n\n"))
        .collect();
    write!(sock, "{:x}\r\n{body}\r\n", body.len()).unwrap();
    sock.flush().unwrap();
}

/// Read one request off a kept-alive connection — its body, `None` once the
/// client closed it.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<String> {
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return None;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// A Messages upstream that answers by arrival order: request 0 calls a tool,
/// request 1 (the triage) opens its stream once `open` is set and then sends nothing
/// until `release`, and every later request ends the turn. `requests` counts
/// arrivals.
fn scripted_upstream(
    requests: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
    open: Arc<AtomicBool>,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { return };
            let (requests, release, open) = (
                Arc::clone(&requests),
                Arc::clone(&release),
                Arc::clone(&open),
            );
            std::thread::spawn(move || {
                let mut reader = BufReader::new(sock.try_clone().unwrap());
                let mut sock = sock;
                while let Some(body) = read_request(&mut reader) {
                    // The title ask rides its own detached task
                    // (docs/session-title.md) and lands between the scripted
                    // requests: answered out of band, before the script, and not
                    // counted — the arrival indexing describes the main and fork
                    // traffic only.
                    if is_title_ask(&body) {
                        chunk(&mut sock, END_TURN);
                        let _ = write!(sock, "0\r\n\r\n");
                        let _ = sock.flush();
                        continue;
                    }
                    let n = requests.fetch_add(1, Ordering::SeqCst);
                    while n == 1 && !open.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    write!(
                        sock,
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                         transfer-encoding: chunked\r\n\r\n"
                    )
                    .unwrap();
                    if n == 1 {
                        chunk(&mut sock, &TOOL_CALL[..1]);
                        while !release.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        return;
                    }
                    chunk(&mut sock, if n == 0 { TOOL_CALL } else { END_TURN });
                    let _ = write!(sock, "0\r\n\r\n");
                    let _ = sock.flush();
                }
            });
        }
    });
    addr
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting: {what}"));
}

/// **An interrupt during the triage ends the turn there.** The press ends the triage's
/// read and the results are withheld as the interrupt's *after* juncture withholds
/// one (`Elided`, nothing blaming triage); the loop then consumes the latch after
/// the triage and goes idle without another request. Left set, the latch rides into the
/// next completion and stops its first call as "aborted by user" — the model's
/// answer to whatever the operator queued to explain the press.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_during_the_triage_ends_the_turn_at_the_gate() {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let cwd = std::env::temp_dir().join(format!("efference-interrupt-{nonce:016x}"));
    std::fs::create_dir_all(cwd.join("scratch")).unwrap();
    let cwd = cwd.canonicalize().unwrap();

    let requests = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let open = Arc::new(AtomicBool::new(true));
    let addr = scripted_upstream(Arc::clone(&requests), Arc::clone(&release), open);
    let mut boot = boot(&cwd);
    let endpoint = crate::api::Endpoint {
        provider: crate::api::Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    boot.client = Client::new(endpoint, "m", "sys", None);

    let host = Swansong::new();
    let runtime = SessionRuntime::start(boot, &host);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    wait_until("the triage is open", || {
        requests.load(Ordering::SeqCst) == 2
    })
    .await;
    session.interrupt(crate::log::InterruptOrigin {
        source: Some("web".into()),
        control: Some("stop-button".into()),
    });
    wait_until("the loop is idle", || {
        session.loop_phase() == crate::daemon::LoopPhase::Idle
    })
    .await;
    // Room for a request the loop should not make.
    tokio::time::sleep(Duration::from_millis(300)).await;
    release.store(true, Ordering::SeqCst);

    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "the turn ended at the triage: no request after the interrupted triage"
    );
    assert!(!session.interrupt_pending(), "the press was consumed");
    let log = session.snapshot();
    // The origin is recorded before the signal it describes, so it precedes the
    // triage request's `failed: interrupted` stage.
    let recorded = log
        .iter()
        .position(|e| {
            matches!(
                &e.payload,
                Payload::Telemetry(crate::log::Telemetry::Interrupt(origin))
                    if origin.source.as_deref() == Some("web")
                        && origin.control.as_deref() == Some("stop-button")
            )
        })
        .expect("the interrupt is in the log");
    let failed = log
        .iter()
        .position(|e| {
            matches!(
                &e.payload,
                Payload::Telemetry(crate::log::Telemetry::RequestStage(stage))
                    if matches!(&stage.stage, crate::log::RequestStageKind::Failed { reason }
                        if reason.contains("interrupt"))
            )
        })
        .expect("the interrupted request recorded its failure");
    assert!(recorded < failed, "{recorded} before {failed}");
    // Withheld as at the interrupt's after juncture: the raw stays in the log, captured
    // before the triage, and the verdict that keeps it off the wire names its seq.
    let captured_seq = log
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { .. } => Some(e.seq),
            _ => None,
        })
        .expect("the ran call's result is in the log");
    assert!(
        log.iter().any(|e| matches!(
            &e.payload,
            Payload::ResultVerdict { for_result, wire: WireState::Elided } if *for_result == captured_seq
        )),
        "the interrupted triage withholds the result it captured"
    );

    host.shut_down().await;
    runtime.finished().await.unwrap();
    let _ = std::fs::remove_dir_all(&cwd);
}

/// The press lands after the triage's request reached the upstream but before its
/// stream opened — the window `notify_waiters`, which stores no permit, would lose
/// if the client armed its abort only once the stream was open. The upstream holds
/// the triage's response headers until the interrupt has fired, so this ordering is
/// forced rather than left to scheduling: the loop must still end the turn.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_before_the_triage_stream_opens_still_ends_it() {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let cwd = std::env::temp_dir().join(format!("efference-interrupt-open-{nonce:016x}"));
    std::fs::create_dir_all(cwd.join("scratch")).unwrap();
    let cwd = cwd.canonicalize().unwrap();

    let requests = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let open = Arc::new(AtomicBool::new(false));
    let addr = scripted_upstream(
        Arc::clone(&requests),
        Arc::clone(&release),
        Arc::clone(&open),
    );
    let mut boot = boot(&cwd);
    let endpoint = crate::api::Endpoint {
        provider: crate::api::Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    boot.client = Client::new(endpoint, "m", "sys", None);

    let host = Swansong::new();
    let runtime = SessionRuntime::start(boot, &host);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    wait_until("the triage request reached the upstream", || {
        requests.load(Ordering::SeqCst) == 2
    })
    .await;
    session.interrupt(Default::default());
    open.store(true, Ordering::SeqCst);
    wait_until("the loop is idle", || {
        session.loop_phase() == crate::daemon::LoopPhase::Idle
    })
    .await;
    release.store(true, Ordering::SeqCst);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "no request after the interrupted triage"
    );

    host.shut_down().await;
    runtime.finished().await.unwrap();
    let _ = std::fs::remove_dir_all(&cwd);
}
