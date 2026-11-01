//! A triage request's text and thinking on the `output` class (docs/control-api.md):
//! `triage_delta` and `triage_thinking_delta`, naming the results being triaged, from the
//! shared triage and the separate-model triage alike, and never in the log.

use super::runtime::{boot, started, workdir};
use super::*;
use crate::frontend::SubmittedTurn;
use crate::protocol::stream::StreamDelta;
use crate::record::log::Provider;
use crate::testing::upstream::{Canned, ScriptedUpstream, end_turn, events, tool_call};
use crate::triage::Triager;
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

/// Run one turn whose single bash call is triaged on the main upstream or, with `own`,
/// on a triage client of its own against a second upstream, and return every triage
/// delta the stream carried as `(kind, call_ids, text)`, plus the session for log
/// assertions.
async fn run(own: bool) -> (Vec<(&'static str, Vec<String>, String)>, Arc<Session>) {
    let (main, triage) = if own {
        (
            ScriptedUpstream::new(|n| match n {
                0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
                _ => Canned::stream(end_turn()),
            }),
            Some(ScriptedUpstream::new(
                |_| Canned::stream(thinking_verdict()),
            )),
        )
    } else {
        (
            ScriptedUpstream::new(|n| match n {
                0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
                1 => Canned::stream(thinking_verdict()),
                _ => Canned::stream(end_turn()),
            }),
            None,
        )
    };
    let (cwd, _dir) = workdir("triage-stream");
    let mut boot = boot(&cwd);
    let endpoint =
        |upstream: &ScriptedUpstream| upstream.endpoint(Provider::Anthropic, "/v1/messages");
    boot.client = Client::new(endpoint(&main), "m", "sys");
    if let Some(triage) = &triage {
        boot.triager = Triager::own(Client::new(endpoint(triage), "t", "sys"));
    }
    let host = Swansong::new();
    let runtime = started(boot, &host).await;
    let session = Arc::clone(runtime.session());
    let mut rx = session.subscribe_stream();
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    session
        .wait_until_with(&main, "the turn finishes", || {
            main.count() >= if own { 2 } else { 3 } && session.loop_phase() == LoopPhase::Idle
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
