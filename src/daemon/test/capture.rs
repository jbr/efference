//! A result captured before its triage, against a real turn loop and a scripted
//! upstream (docs/triage.md): the bytes the gate is shown are the masked ones
//! the log holds, and a completed batch projects the same wire whether its results were
//! captured first or logged with their verdicts.

use super::runtime::boot;
use super::upstream_hold::{Answer, end_turn, recording, tool_call, wait_until};
use super::*;
use crate::daemon::SessionRuntime;
use crate::frontend::SubmittedTurn;
use crate::log::Telemetry;
use crate::log::{AssistantOutput, ContentBlock, Keep, RawResult, Triaged, WireState};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use trillium_tokio::Swansong;

/// A completion making `calls` bash calls in one turn — the shape that forces a
/// multi-result gate.
fn completion(calls: &[(&str, &str)]) -> String {
    let events: Vec<String> = calls
        .iter()
        .enumerate()
        .flat_map(|(index, (id, command))| {
            // `partial_json` carries the arguments as the text the model wrote, so
            // the JSON is encoded twice: once as that text, once as the string holding it.
            let arguments = serde_json::json!({ "command": command }).to_string();
            let input = serde_json::to_string(&arguments).unwrap();
            [
                format!(
                    r#"{{"type":"content_block_start","index":{index},"content_block":{{"type":"tool_use","id":"{id}","name":"bash","input":{{}}}}}}"#
                ),
                format!(
                    r#"{{"type":"content_block_delta","index":{index},"delta":{{"type":"input_json_delta","partial_json":{input}}}}}"#
                ),
                format!(r#"{{"type":"content_block_stop","index":{index}}}"#),
            ]
        })
        .collect();
    let mut data = vec![r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string()];
    data.extend(events);
    data.push(
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#
            .to_string(),
    );
    data.push(r#"{"type":"message_stop"}"#.to_string());
    super::upstream_hold::events(&data)
}

/// A `retain` call answering for each label of a multi-result gate.
pub(super) fn verdict(keeps: &[&str]) -> String {
    let results: Vec<serde_json::Value> = keeps
        .iter()
        .enumerate()
        .map(|(index, keep)| serde_json::json!({ "result": format!("r{}", index + 1), "keep": keep }))
        .collect();
    tool_call(
        "toolu_r",
        "retain",
        &serde_json::json!({ "results": results }).to_string(),
    )
}

/// A session on a fresh directory, pointed at `addr`. `env` is the session's `[env]`
/// table, which is what installs the masker over the session's appends.
fn start(addr: SocketAddr, env: crate::env_table::EnvPolicy) -> (Swansong, SessionRuntime) {
    let mut boot = boot(&cwd());
    boot.client = Client::new(
        crate::api::Endpoint {
            provider: crate::api::Provider::Anthropic,
            url: Some(format!("http://{addr}/v1/messages")),
            auth_header: "x-api-key".into(),
            auth_value: "k".into(),
            session_id: None,
            extra_headers: Vec::new(),
            request: Default::default(),
            base: None,
        },
        "m",
        "sys",
        None,
    );
    boot.env_policy = env;
    let host = Swansong::new();
    let runtime = SessionRuntime::start(boot, &host);
    (host, runtime)
}

fn cwd() -> std::path::PathBuf {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let dir = std::env::temp_dir().join(format!("efference-capture-{nonce:016x}"));
    std::fs::create_dir_all(dir.join("scratch")).unwrap();
    dir.canonicalize().unwrap()
}

/// An `[env]` table masking one literal value.
fn masking(value: &str) -> crate::env_table::EnvPolicy {
    let mut table = BTreeMap::new();
    table.insert(
        "API_TOKEN".to_string(),
        crate::env_table::EnvEntry {
            mask: Some(true),
            value: Some(value.to_string()),
            ..Default::default()
        },
    );
    crate::env_table::EnvPolicy::build(&table).policy
}

/// **The gate reads the bytes the log holds.** A result is masked on the way into the
/// log (docs/env.md), so what the fork is shown is already the masked form — triage
/// never sees a value the log would refuse to show a client either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn triage_is_shown_the_masked_result() {
    let secret = "s3cr3t-token-value";
    let requests = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let addr = recording(
        Arc::clone(&requests),
        |n| match n {
            0 => Answer::Stream(completion(&[(
                "toolu_1",
                "printf 'token=%s\\n' \"$API_TOKEN\"",
            )])),
            1 => Answer::Stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
            _ => Answer::Stream(end_turn()),
        },
        &bodies,
    );
    // The value reaches the child as an ordinary `[env]` entry, so the tool prints it
    // and the masker is what removes it — not the command.
    let env = masking(secret);
    let (host, runtime) = start(addr, env);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    // The loop is idle until the submission reaches it, so the turn is waited for by
    // what it did rather than by `running`, which is false in both states.
    wait_until("the turn has run", || requests.load(Ordering::SeqCst) >= 3).await;
    wait_until("the turn finishes", || {
        !session.running.load(Ordering::SeqCst)
    })
    .await;
    host.shut_down().await;

    let peek = bodies.lock().unwrap()[1].clone();
    assert!(
        peek.contains("token="),
        "the peek carries the result: {peek}"
    );
    assert!(
        !peek.contains(secret),
        "the gate is shown the masked bytes: {peek}"
    );
    assert!(
        peek.contains("API_TOKEN"),
        "and the marker names what was elided: {peek}"
    );

    // The log holds the masked form too, so nothing downstream can recover it.
    let log = session.snapshot();
    let logged = log
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { raw, .. } => Some(raw),
            _ => None,
        })
        .expect("the call's result is in the log");
    assert!(!logged.content.contains(secret), "the log is masked too");
}

/// **A completed batch projects the wire it always did.** Two calls, one gate, two
/// verdicts — and the result is byte-identical to a log written with each verdict on
/// its own result, which is the shape every session before capture has. This is the
/// property the cached prefix rests on: what decides a result cannot change what the
/// model reads, only when it was decided.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completed_batch_projects_the_wire_a_directly_logged_one_would() {
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = recording(
        Arc::clone(&requests),
        |n| match n {
            0 => Answer::Stream(completion(&[
                ("toolu_1", "printf 'first\\nsecond\\nthird\\n'"),
                ("toolu_2", "printf 'alpha\\nbeta\\n'"),
            ])),
            1 => Answer::Stream(verdict(&["1-2", "all"])),
            _ => Answer::Stream(end_turn()),
        },
        &Arc::new(Mutex::new(Vec::new())),
    );
    let (host, runtime) = start(addr, masking(""));
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    wait_until("the turn has run", || requests.load(Ordering::SeqCst) >= 3).await;
    wait_until("the turn finishes", || {
        !session.running.load(Ordering::SeqCst)
    })
    .await;
    let through_the_loop = session.project_messages();
    host.shut_down().await;

    // Every captured result was decided, and by its own verdict's wire form.
    let log = session.snapshot();
    let captured: Vec<u64> = log
        .iter()
        .filter(|e| {
            matches!(
                &e.payload,
                Payload::ToolResult {
                    wire: WireState::Pending,
                    ..
                }
            )
        })
        .map(|e| e.seq)
        .collect();
    assert_eq!(captured.len(), 2, "both calls captured their result");
    for seq in &captured {
        let decided = log.iter().any(|e| {
            matches!(
                &e.payload,
                Payload::ResultVerdict { for_result, wire: WireState::Triaged(_), .. }
                    if for_result == seq
            )
        });
        assert!(decided, "the result at {seq} has its own verdict");
        // The dispatch marker precedes the result it dispatched: a call that ran is
        // marked started before its output exists (docs/log-and-projection.md).
        let Payload::ToolResult { call_id, .. } = &log[*seq as usize].payload else {
            unreachable!("captured seqs index results");
        };
        let started = log[..*seq as usize]
            .iter()
            .any(|e| matches!(&e.payload, Payload::Telemetry(Telemetry::CallStarted { call_id: id }) if id == call_id));
        assert!(
            started,
            "the call behind the result at {seq} was marked started first"
        );
    }

    // The same two calls, logged the way a session before capture wrote them.
    let (direct, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    direct.append(1, user("go")).expect("an append succeeds");
    direct
        .append(
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![
                        ToolCall::new(
                            crate::bash::NAME,
                            "toolu_1",
                            serde_json::json!({ "command": "printf 'first\\nsecond\\nthird\\n'" }),
                        ),
                        ToolCall::new(
                            crate::bash::NAME,
                            "toolu_2",
                            serde_json::json!({ "command": "printf 'alpha\\nbeta\\n'" }),
                        ),
                    ],
                ),
                upstream_failure: None,
            },
        )
        .expect("an append succeeds");
    let mut logged_directly = Vec::new();
    for (id, output, keep) in [
        (
            "toolu_1",
            "first\nsecond\nthird\n",
            Keep::Ranges(vec![(1, 2)]),
        ),
        ("toolu_2", "alpha\nbeta\n", Keep::All),
    ] {
        let seq = direct
            .append(
                1,
                Payload::tool_result(
                    id,
                    RawResult::text(output),
                    WireState::from(Triaged::new(keep)),
                ),
            )
            .expect("an append succeeds");
        logged_directly.push(seq);
    }
    // The trailing turn the scripted upstream ends with, so both sides describe the
    // same conversation.
    direct
        .append(
            2,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![ContentBlock::Text("ok".into())],
                    vec![],
                    vec![],
                ),
                upstream_failure: None,
            },
        )
        .expect("an append succeeds");

    // Each result's wire text leads with its own `seq` (`render_result`), and capture
    // writes one more entry per result than the direct log does, so the two logs number
    // the same results differently. That label is the result's address and is checked
    // against the captured seqs; everything else must match byte for byte.
    let through_the_loop = serde_json::to_string(&through_the_loop).unwrap();
    for seq in &captured {
        assert!(
            through_the_loop.contains(&format!("[{seq}] ")),
            "the wire labels the result with the seq it was captured at ({seq})"
        );
    }
    let relabel = |wire: String, seqs: &[u64]| {
        seqs.iter().enumerate().fold(wire, |wire, (index, seq)| {
            wire.replace(&format!("[{seq}] "), &format!("[result {index}] "))
        })
    };
    assert_eq!(
        relabel(through_the_loop, &captured),
        relabel(
            serde_json::to_string(&direct.project_messages()).unwrap(),
            &logged_directly
        ),
        "capture changed when a verdict was recorded, not what the model reads"
    );
}
