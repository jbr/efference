//! A triage request's text and thinking on the `output` class (docs/control-api.md):
//! `triage_delta` and `triage_thinking_delta`, naming the results being triaged, from the
//! shared triage and the separate-model triage alike, and never in the log.

use super::runtime::boot;
use super::upstream_hold::{Answer, end_turn, events, scripted, tool_call};
use super::*;
use crate::daemon::{SessionRuntime, StreamDelta};
use crate::frontend::SubmittedTurn;
use crate::triage::Triager;
use std::sync::atomic::{AtomicUsize, Ordering};
use trillium_tokio::Swansong;

/// A completion that thinks, writes a line of text, then makes the `retain` call.
fn thinking_verdict() -> String {
    events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#.to_string(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing the read"}}"#.to_string(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#.to_string(),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.to_string(),
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"keeping it all"}}"#.to_string(),
        r#"{"type":"content_block_stop","index":1}"#.to_string(),
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_r","name":"retain","input":{}}}"#.to_string(),
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"keep\":\"all\"}"}}"#.to_string(),
        r#"{"type":"content_block_stop","index":2}"#.to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#.to_string(),
        r#"{"type":"message_stop"}"#.to_string(),
    ])
}

fn endpoint(addr: std::net::SocketAddr) -> crate::api::Endpoint {
    crate::api::Endpoint::at(
        crate::log::Provider::Anthropic,
        format!("http://{addr}/v1/messages"),
    )
}

/// Run one turn whose single bash call is triaged by `triage_script`'s upstream, and
/// return every triage delta the stream carried as `(kind, call_ids, text)`, plus the
/// session for log assertions. `own` puts the triage on its own client.
async fn run(own: bool) -> (Vec<(&'static str, Vec<String>, String)>, Arc<Session>) {
    let main_requests = Arc::new(AtomicUsize::new(0));
    let triage_requests = Arc::new(AtomicUsize::new(0));
    let (main_addr, triage_addr) = if own {
        (
            scripted(Arc::clone(&main_requests), |n| match n {
                0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
                _ => Answer::Stream(end_turn()),
            }),
            Some(scripted(Arc::clone(&triage_requests), |_| {
                Answer::Stream(thinking_verdict())
            })),
        )
    } else {
        (
            scripted(Arc::clone(&main_requests), |n| match n {
                0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
                1 => Answer::Stream(thinking_verdict()),
                _ => Answer::Stream(end_turn()),
            }),
            None,
        )
    };
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let cwd = std::env::temp_dir().join(format!("efference-triage-stream-{nonce:016x}"));
    std::fs::create_dir_all(cwd.join("scratch")).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let mut boot = boot(&cwd);
    boot.client = Client::new(endpoint(main_addr), "m", "sys", None);
    if let Some(addr) = triage_addr {
        boot.triager = Triager::own(Client::new(endpoint(addr), "t", "sys", None));
    }
    let host = Swansong::new();
    let runtime = SessionRuntime::start(boot, &host);
    let session = Arc::clone(runtime.session());
    let mut rx = session.subscribe_stream();
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    session
        .poll_until("the turn finishes", || {
            main_requests.load(Ordering::SeqCst) >= if own { 2 } else { 3 }
                && session.loop_phase() == LoopPhase::Idle
        })
        .await;
    host.shut_down().await;

    let mut seen = Vec::new();
    while let Ok(delta) = rx.try_recv() {
        match delta {
            StreamDelta::TriageText { call_ids, text } => seen.push(("text", call_ids, text)),
            StreamDelta::TriageThinking { call_ids, text } => {
                seen.push(("thinking", call_ids, text));
            }
            _ => {}
        }
    }
    (seen, session)
}

fn assert_streamed(seen: &[(&'static str, Vec<String>, String)], session: &Session) {
    let ids = vec!["toolu_1".to_string()];
    assert!(
        seen.contains(&("thinking", ids.clone(), "weighing the read".into())),
        "{seen:?}"
    );
    assert!(
        seen.contains(&("text", ids, "keeping it all".into())),
        "{seen:?}"
    );
    // Ephemeral: the fragments are logged by no entry of their own. The thinking
    // survives once, on the verdict's `Triage` entry (docs/triage.md), which is that
    // entry's field and not a stream record.
    let log = serde_json::to_string(&session.snapshot()).unwrap();
    assert!(!log.contains("keeping it all"), "never logged: {log}");
    assert_eq!(log.matches("weighing the read").count(), 1, "{log}");
}

/// **The shared triage streams.** The fork rides the agent's client; its fragments
/// still reach the stream as triage kinds, naming the call being triaged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_triage_streams_its_thinking_and_text() {
    let (seen, session) = run(false).await;
    assert_streamed(&seen, &session);
}

/// **The separate-model triage streams too.**
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_separate_triage_streams_its_thinking_and_text() {
    let (seen, session) = run(true).await;
    assert_streamed(&seen, &session);
}
