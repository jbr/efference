//! Upstream failures against a real turn loop and a scripted upstream
//! (docs/transport.md, docs/triage.md): a failed request takes the retry
//! schedule instead of stalling at once, a triage without a verdict holds
//! its results rather than showing the agent an error, and the operator's
//! continue retries either wait now.

use super::runtime::boot;
use super::*;
use crate::daemon::{SessionRuntime, StallReason};
use crate::frontend::SubmittedTurn;
use crate::log::{Origin, WireState};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use trillium_tokio::Swansong;

/// A `tool_use` completion calling `name` with `input` (JSON text).
pub(super) fn tool_call(id: &str, name: &str, input: &str) -> String {
    let input = serde_json::to_string(input).unwrap();
    events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
        format!(
            r#"{{"type":"content_block_start","index":0,"content_block":{{"type":"tool_use","id":"{id}","name":"{name}","input":{{}}}}}}"#
        ),
        format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":{input}}}}}"#
        ),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#.to_string(),
        r#"{"type":"message_stop"}"#.to_string(),
    ])
}

/// A plain end of turn.
pub(super) fn end_turn() -> String {
    events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#.to_string(),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#.to_string(),
        r#"{"type":"message_stop"}"#.to_string(),
    ])
}

pub(super) fn events(data: &[String]) -> String {
    data.iter()
        .map(|d| {
            let kind: serde_json::Value = serde_json::from_str(d).unwrap();
            format!("event: {}\ndata: {d}\n\n", kind["type"].as_str().unwrap())
        })
        .collect()
}

/// The title ask, recognizable by the one phrase no other request carries
/// (docs/session-title.md): the peek's tail instruction.
pub(super) fn is_title_ask(body: &str) -> bool {
    body.contains("say what this session is for")
}

/// One scripted answer.
pub(super) enum Answer {
    /// A 200 event stream with this body.
    Stream(String),
    /// A non-2xx with this JSON body.
    Status(u16, &'static str),
}

/// Read one request off a kept-alive connection; `false` once the client closed it.
/// Read one request off a kept-alive connection, with its body; `None` once the
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
    String::from_utf8(body).ok()
}

/// A Messages upstream answering request *n* with `script(n)`; `requests` counts
/// arrivals.
pub(super) fn scripted(requests: Arc<AtomicUsize>, script: fn(usize) -> Answer) -> SocketAddr {
    serve(requests, script, None)
}

/// [`scripted`] keeping every request body it read, for a test that asserts what the
/// fork was shown (docs/triage.md).
pub(super) fn recording(
    requests: Arc<AtomicUsize>,
    script: fn(usize) -> Answer,
    bodies: &Arc<Mutex<Vec<String>>>,
) -> SocketAddr {
    serve(requests, script, Some(Arc::clone(bodies)))
}

/// The one upstream both harnesses are: `bodies`, when given, collects each request
/// before it is answered.
fn serve(
    requests: Arc<AtomicUsize>,
    script: fn(usize) -> Answer,
    bodies: Option<Arc<Mutex<Vec<String>>>>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { return };
            let requests = Arc::clone(&requests);
            let bodies = bodies.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(sock.try_clone().unwrap());
                let mut sock = sock;
                while let Some(body) = read_request(&mut reader) {
                    // The title ask rides its own detached task (docs/session-title.md)
                    // and lands between the scripted requests: answered out of band,
                    // before the script, and not counted — the scripts and their
                    // count assertions describe the main and fork traffic only.
                    // `end_turn` answers it; the triage's parse takes the text line.
                    if is_title_ask(&body) {
                        let body = end_turn();
                        let _ = write!(
                            sock,
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                             content-length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = sock.flush();
                        continue;
                    }
                    if let Some(bodies) = &bodies {
                        bodies.lock().unwrap().push(body);
                    }
                    let n = requests.fetch_add(1, Ordering::SeqCst);
                    let _ = match script(n) {
                        Answer::Stream(body) => write!(
                            sock,
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                             content-length: {}\r\n\r\n{body}",
                            body.len()
                        ),
                        Answer::Status(status, body) => write!(
                            sock,
                            "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\n\r\n{body}",
                            body.len()
                        ),
                    };
                    let _ = sock.flush();
                }
            });
        }
    });
    addr
}

pub(super) async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting: {what}"));
}

pub(super) fn start(addr: std::net::SocketAddr) -> (Swansong, SessionRuntime) {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let cwd = std::env::temp_dir().join(format!("efference-upstream-{nonce:016x}"));
    std::fs::create_dir_all(cwd.join("scratch")).unwrap();
    let cwd = cwd.canonicalize().unwrap();
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
    (host, runtime)
}

const REFUSED: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"no"}}"#;
const OVERLOADED: &str = r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#;

/// **A triage without a verdict holds; continue retries it.** The triage's
/// request is refused, so the triage has no verdict. Nothing reaches the log for the
/// call — no error result the agent would read — and the session reports a stall
/// while its loop still runs. The operator's continue runs the triage again, the
/// verdict lands, and the held attempt's breadcrumb rides the `Triage`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gate_without_a_verdict_holds_until_continue() {
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Answer::Status(400, REFUSED),
        2 => Answer::Stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    wait_until("the triage is held", || session.stall().is_some()).await;
    let stall = session.stall().unwrap();
    assert_eq!(stall.reason, StallReason::TriageUnavailable);
    assert!(session.running.load(Ordering::SeqCst), "held, not parked");
    assert!(session.is_settled(), "a held triage is a decision point");
    // The result is durable from the moment it existed, but undecided: the entry is
    // there to read and projects to nothing, so a request built before the triage's
    // verdict could not carry half a batch.
    let captured = session
        .snapshot()
        .into_iter()
        .find_map(|e| match e.payload {
            Payload::ToolResult { raw, wire, .. } => Some((raw, wire)),
            _ => None,
        })
        .expect("the captured result is on the log while the triage holds");
    assert!(
        captured.0.content.contains("hi"),
        "the output itself survives the hold: {:?}",
        captured.0.content
    );
    assert!(
        matches!(captured.1, WireState::Pending),
        "and nothing has decided its wire form yet"
    );
    assert!(
        !session
            .project_messages()
            .iter()
            .flat_map(|m| m.content.iter())
            .any(|block| matches!(block, crate::project::ApiBlock::ToolResult { .. })),
        "nothing about the held call reaches the main wire"
    );

    assert_eq!(
        session.request_continue(),
        Ok(()),
        "continue retries the hold"
    );
    wait_until("the turn finishes", || {
        !session.running.load(Ordering::SeqCst)
    })
    .await;
    assert!(
        session.stall().is_none(),
        "the recovered hold is not a stall"
    );
    let log = session.snapshot();
    let captured_seq = log
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { .. } => Some(e.seq),
            _ => None,
        })
        .expect("the captured result is in the log");
    assert!(
        log.iter().any(|e| matches!(
            &e.payload,
            Payload::ResultVerdict { for_result, wire: WireState::Triaged(_), .. }
                if *for_result == captured_seq
        )),
        "the verdict names the result it was decided for"
    );
    assert!(
        !log.iter().any(|e| matches!(
            &e.payload,
            Payload::ResultVerdict {
                wire: WireState::TriageFailed { .. },
                ..
            }
        )) && !log.iter().any(|e| matches!(
            &e.payload,
            Payload::ToolResult {
                wire: WireState::TriageFailed { .. },
                ..
            }
        )),
        "no triage failure is ever written"
    );
    let triage = log
        .iter()
        .find_map(|e| match &e.payload {
            Payload::Triage(t) if t.for_result.is_some() => Some(t),
            _ => None,
        })
        .expect("the triage");
    assert_eq!(triage.origin, Origin::Model);
    assert!(
        triage.retries.iter().any(|r| r.starts_with("held: ")),
        "the hold is in the record: {:?}",
        triage.retries
    );
    host.shut_down().await;
}

/// **A compaction written during a hold is what the retried triage forks.** The triage holds,
/// a compaction's mark lands (as `compact_once` appends it), and the operator retries: the
/// retried triage carries the account, not the history it replaced. The triage's first
/// snapshot predates the mark, so a retry that reused it would still read the whole
/// unfolded prefix, which is the cold read the compaction exists to avoid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fold_during_a_hold_is_what_the_retry_triages() {
    let requests = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let addr = recording(
        Arc::clone(&requests),
        |n| match n {
            0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
            1 => Answer::Status(400, REFUSED),
            2 => Answer::Stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
            _ => Answer::Stream(end_turn()),
        },
        &bodies,
    );
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("zebra history to compact away", "s1"));

    wait_until("the triage is held", || session.stall().is_some()).await;
    let upto = session
        .snapshot()
        .iter()
        .find_map(|e| matches!(e.payload, Payload::User(_)).then_some(e.seq))
        .expect("the user turn is logged");
    session
        .append(
            1,
            Payload::Summary(crate::log::Summary::consolidation(
                0,
                upto,
                "the compacted account",
            )),
        )
        .unwrap();
    assert_eq!(session.request_continue(), Ok(()));
    wait_until("the turn finishes", || {
        !session.running.load(Ordering::SeqCst)
    })
    .await;

    let bodies = bodies.lock().unwrap().clone();
    let held = &bodies[1];
    let retried = &bodies[2];
    assert!(
        held.contains("zebra history"),
        "the held triage read the history"
    );
    assert!(
        retried.contains("the compacted account") && !retried.contains("zebra history"),
        "the retried triage reads the compaction: {retried}"
    );
    host.shut_down().await;
}

/// **A failed request takes the schedule.** The transport exhausts its own
/// attempts on an overloaded upstream and returns an error; the loop records it
/// and waits to retry instead of stalling, and the operator's continue cuts the
/// wait short.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_request_is_retried_and_continue_skips_the_wait() {
    let requests = Arc::new(AtomicUsize::new(0));
    // The transport makes four attempts before the loop sees the failure.
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0..4 => Answer::Status(503, OVERLOADED),
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    wait_until("the loop is waiting to retry", || {
        session.upstream_retry.get().is_some()
    })
    .await;
    assert!(
        session.stall().is_none(),
        "a transient failure does not stall"
    );
    assert_eq!(session.request_continue(), Ok(()), "continue retries now");
    wait_until("the turn finishes", || {
        !session.running.load(Ordering::SeqCst)
    })
    .await;
    assert!(session.stall().is_none());
    assert!(session.upstream_retry.get().is_none());
    assert!(
        session.snapshot().iter().any(|e| matches!(
            &e.payload,
            Payload::Assistant {
                upstream_failure: Some(_),
                ..
            }
        )),
        "the failed request is in the record"
    );
    host.shut_down().await;
}

/// **Fresh forks, then a harness defect.** A triage that answers every attempt of
/// every fork without a usable verdict is not an upstream condition: after
/// `TRIAGE_RESTARTS` fresh forks of `TRIAGE_ATTEMPTS` each, the triage holds under
/// `TriageDefect`, and still nothing reaches the agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_triage_that_never_answers_usably_is_held_as_a_defect() {
    use crate::triage::TRIAGE_RESTARTS;
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        // Prose, not a verdict: unusable every time.
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    wait_until("the triage is held", || session.stall().is_some()).await;
    assert_eq!(session.stall().unwrap().reason, StallReason::TriageDefect);
    // The tool call, then three attempts in each fork.
    assert_eq!(requests.load(Ordering::SeqCst), 1 + 3 * TRIAGE_RESTARTS);
    // Every fork is spent and the result is still undecided: durable in the log, since
    // it was captured before the triage opened, and absent from the wire, since no fork
    // answered for it.
    let captured = session
        .snapshot()
        .into_iter()
        .find_map(|e| match e.payload {
            Payload::ToolResult { raw, wire, .. } => Some((raw, wire)),
            _ => None,
        })
        .expect("the captured result is on the log while the triage holds");
    assert!(
        captured.0.content.contains("hi"),
        "the output itself survives the hold: {:?}",
        captured.0.content
    );
    assert!(matches!(captured.1, WireState::Pending));
    assert!(
        !session
            .project_messages()
            .iter()
            .flat_map(|m| m.content.iter())
            .any(|block| matches!(block, crate::project::ApiBlock::ToolResult { .. })),
        "nothing about the held call reaches the main wire"
    );
    host.shut_down().await;
}
