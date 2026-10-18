use super::*;
use crate::log::{AssistantOutput, ToolCall};

fn response(text: &str) -> AssistantOutput {
    AssistantOutput::Messages(vec![crate::log::MessagesOutputBlock::Text(text.into())])
}

#[test]
fn rejects_skip_short_tools_failures_and_caps() {
    assert_eq!(
        account(&response(" SKIP \n"), None, None).unwrap_err(),
        "skip"
    );
    assert_eq!(
        account(&response(""), None, None).unwrap_err(),
        "short account"
    );
    assert_eq!(
        account(&response("not enough"), None, None).unwrap_err(),
        "short account"
    );
    let text = "supported ".repeat(100);
    assert_eq!(account(&response(&text), None, None).unwrap(), text.trim());
    let capped = crate::api::Finish {
        reason: FinishReason::OutputCap,
        raw: "max_tokens".into(),
    };
    assert!(account(&response(&text), Some(&capped), None).is_err());
    assert!(account(&response(&text), None, Some("interrupted")).is_err());
    let mut called = response(&text);
    if let AssistantOutput::Messages(blocks) = &mut called {
        blocks.push(crate::log::MessagesOutputBlock::ToolUse(ToolCall::new(
            "read",
            "call",
            serde_json::json!({}),
        )));
    }
    assert_eq!(account(&called, None, None).unwrap_err(), "tool call");
}

use crate::api::{Endpoint, Provider};
use crate::log::{RawResult, Summary, WireState};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn session() -> Arc<Session> {
    let (shared, _) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    shared
        .append(
            0,
            Payload::User(vec![ContentBlock::Text("inspect this source".into())]),
        )
        .unwrap();
    shared
        .append(
            0,
            Payload::Assistant {
                output: AssistantOutput::Messages(vec![crate::log::MessagesOutputBlock::ToolUse(
                    ToolCall::new("read", "r", serde_json::json!({"path":"source.rs"})),
                )]),
                excluded: None,
            },
        )
        .unwrap();
    shared
        .append(
            0,
            Payload::tool_result(
                "r",
                RawResult::text("evidence ".repeat(9000)),
                WireState::Raw,
            ),
        )
        .unwrap();
    shared
        .append(
            0,
            Payload::Assistant {
                output: response("the investigation is complete"),
                excluded: None,
            },
        )
        .unwrap();
    shared
}

struct Mock {
    client: Client,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.thread.take().unwrap().join();
    }
}
fn mock(text: &str, fail: bool) -> Mock {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let body = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"m\",\"model\":\"m\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":0}}}}}}\n\nevent: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":{}}}}}\n\nevent: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":100}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
        serde_json::to_string(text).unwrap()
    );
    let reply = if fail {
        "HTTP/1.1 400 Bad Request\r\ncontent-length: 3\r\nconnection: close\r\n\r\nbad".into()
    } else {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let count = requests.clone();
    let done = stop.clone();
    let thread = std::thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            let (mut sock, _) = match listener.accept() {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            sock.set_nonblocking(false).unwrap();
            sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 8192];
            while !String::from_utf8_lossy(&request).contains("Reply with the account text or") {
                match sock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            if !String::from_utf8_lossy(&request).contains("Reply with the account text or") {
                continue;
            }
            count.fetch_add(1, Ordering::SeqCst);
            sock.write_all(reply.as_bytes()).unwrap();
        }
    });
    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: vec![],
        request: Default::default(),
        base: None,
    };
    Mock {
        client: Client::new(endpoint, "m", "sys", None),
        requests,
        stop,
        thread: Some(thread),
    }
}

#[tokio::test]
async fn unchanged_skip_not_rechecked_but_new_forward_context_is() {
    let shared = session();
    let server = mock("skip", false);
    let check = ConsolidationCheck::new(&server.client, &[]);
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert!(
        !shared
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::Summary(_)))
    );
    shared
        .append(
            1,
            Payload::User(vec![ContentBlock::Text("next task".into())]),
        )
        .unwrap();
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_failed_request_changes_nothing() {
    let shared = session();
    let server = mock("", true);
    let check = ConsolidationCheck::new(&server.client, &[]);
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert!(shared.snapshot().iter().any(|e| matches!(
        &e.payload,
        Payload::Telemetry(Telemetry::Consolidation { outcome, .. })
            if outcome.starts_with("request error")
    )));
    assert!(
        !shared
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::Summary(_)))
    );
}

#[tokio::test]
async fn adopts_account_then_waits_for_new_tool_work() {
    let shared = session();
    let server = mock(&"supported outcome ".repeat(60), false);
    let check = ConsolidationCheck::new(&server.client, &[]);
    let before = shared.project_turns();
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    let after = shared.project_turns();
    assert_eq!(after.last(), before.last());
    assert!(after.len() < before.len());
    assert!(
        shared
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::Summary(_)))
    );
    shared
        .append(
            1,
            Payload::User(vec![ContentBlock::Text("new task".into())]),
        )
        .unwrap();
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
}

#[test]
fn frozen_adoption_preserves_new_messages_and_rejects_new_folds() {
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared
        .append(
            1,
            Payload::User(vec![ContentBlock::Text("new direction".into())]),
        )
        .unwrap();
    assert!(
        shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );
    assert_eq!(
        shared.project_turns().last(),
        Some(&WireTurn::text("new direction"))
    );
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared
        .append(
            1,
            Payload::Summary(Summary::consolidation(0, span.upto, "head summary")),
        )
        .unwrap();
    assert!(
        !shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );
}

#[test]
fn adoption_tolerates_concurrent_work_but_not_rewrites_of_the_range() {
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    // Work the trajectory did while the account was being written: a call still
    // waiting for its result, and a verdict on a result after the snapshot.
    shared
        .append(1, Payload::User(vec![ContentBlock::Text("go on".into())]))
        .unwrap();
    shared
        .append(
            1,
            Payload::Assistant {
                output: AssistantOutput::Messages(vec![crate::log::MessagesOutputBlock::ToolUse(
                    ToolCall::new("read", "later", serde_json::json!({"path":"other.rs"})),
                )]),
                excluded: None,
            },
        )
        .unwrap();
    let later = shared
        .append(
            1,
            Payload::tool_result("later", RawResult::text("other"), WireState::Raw),
        )
        .unwrap();
    shared
        .append(
            1,
            Payload::ResultVerdict {
                for_result: later,
                wire: WireState::Elided,
            },
        )
        .unwrap();
    assert!(
        shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );

    // A verdict on a result inside the frozen snapshot changes what the account
    // was written from.
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared
        .append(
            1,
            Payload::ResultVerdict {
                for_result: span.upto,
                wire: WireState::Elided,
            },
        )
        .unwrap();
    assert!(
        !shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );

    // A `consolidate` call made meanwhile computes its own range without this one.
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared
        .append(
            1,
            Payload::Assistant {
                output: AssistantOutput::Messages(vec![crate::log::MessagesOutputBlock::ToolUse(
                    ToolCall::new(
                        consolidate::NAME,
                        "manual",
                        serde_json::json!({"summary":"s"}),
                    ),
                )]),
                excluded: None,
            },
        )
        .unwrap();
    assert!(
        !shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );
}

#[test]
fn instruction_names_what_follows_the_range() {
    let shared = session();
    let entries = shared.snapshot();
    let span = consolidate::candidate(&entries).unwrap();
    assert!(instruction(&entries, span).contains("the report or question that follows it"));

    // An operator message that arrived mid-work, with no report before it,
    // anchors the range instead.
    let (shared, _) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    for payload in entries
        .iter()
        .take_while(|e| e.seq <= span.upto)
        .map(|e| e.payload.clone())
        .chain([Payload::User(vec![ContentBlock::Text(
            "actually, the ui".into(),
        )])])
    {
        shared.append(0, payload).unwrap();
    }
    let interjected = shared.snapshot();
    let span = consolidate::candidate(&interjected).unwrap();
    let text = instruction(&interjected, span);
    assert!(text.contains("arrived while this work was under way"));
}

#[tokio::test]
async fn a_juncture_reached_during_a_check_is_not_checked() {
    let shared = session();
    let server = mock("skip", false);
    let check = ConsolidationCheck::new(&server.client, &[]);
    check.flight.lock().unwrap().running = true;
    assert!(check.check(&shared).is_none());
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
}
