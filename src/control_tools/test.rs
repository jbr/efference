//! The control tools, exercised over the real control router (docs/operating.md).

use super::*;
use crate::client::Client;
use crate::persist::Log;
use mcplease::traits::Tool as _;
use mcplease::types::{JsonRpcRequest, JsonRpcResponse, RequestContext};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// A live [`router`](crate::daemon::router) for one session on `trillium_testing`'s
/// in-memory transport — the whole request/response cycle, with no socket bound
/// (`daemon/test.rs` explains why the tests never bind one). The session is named
/// `child` and served at `/sessions/child`, as a host serves it; the client is at the
/// root.
async fn session_over(log: Log) -> (Arc<crate::daemon::Session>, Client) {
    let (shared, _turns) = crate::daemon::Session::from_log(log);
    shared.set_identity(crate::protocol::state::SessionIdentity {
        id: "child".into(),
        title: None,
        model: "m".into(),
        cwd: std::env::current_dir().unwrap().display().to_string(),
        inference: None,
        successor: None,
    });
    let mut handler = (
        trillium::Init::new({
            let shared = Arc::clone(&shared);
            move |info: trillium::Info| {
                let shared = Arc::clone(&shared);
                async move { info.with_shared_state(shared) }
            }
        }),
        // Under the path a host serves the session at, so a client re-based on
        // `/sessions/child` reaches it.
        trillium_router::Router::new().all(
            "/sessions/child/*",
            crate::daemon::handler(
                crate::daemon::Transport::Socket { auth: None },
                crate::daemon::App::session(std::env::current_dir().unwrap()),
            ),
        ),
    );
    let context = trillium_testing::init(&mut handler).await;
    let client = Client::with_connector(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    );
    (shared, client)
}

/// Tools over `host` — [`session_over`]'s client, standing in for a host — with
/// `child` as their default session: `cli` picks the command line's presentation.
fn mounted(host: Client, cli: bool) -> Control {
    Control::new(
        Executor::Client(ClientExecutor::defaulting_to(host, "child")),
        cli,
    )
}

/// The context a call over MCP carries from a client named `name`.
fn from_client(name: &'static str) -> RequestContext {
    RequestContext::from_params(Some(&serde_json::json!({
        "_meta": {
            mcplease::types::meta_keys::CLIENT_INFO:
                mcplease::types::Implementation::new(name, "1.0"),
        }
    })))
}

/// One `tools/call` end to end: the JSON-RPC a mounted harness sends, through
/// mcplease's dispatch, through `execute`, through a real `GET /state`, and back
/// out as plain text with no structured half.
#[tokio::test]
async fn tools_call_status_reaches_the_daemon() {
    let (_shared, client) = session_over(Log::in_memory()).await;
    let request: JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "status", "arguments": {} }
    }))
    .unwrap();
    let config = mcplease::ServerConfig::new(mcplease::server_info!());
    let context = RequestContext::from_request(&request);
    let response = mcplease::handle_request::<mcp::Tools, Control>(
        request,
        mounted(client, false),
        context,
        &config,
    )
    .await
    .expect("nothing here cancels");

    let JsonRpcResponse { result, error, .. } = response;
    assert!(error.is_none(), "{error:?}");
    let result = result.expect("a result");
    assert_ne!(
        result.get("isError"),
        Some(&Value::Bool(true)),
        "the tool call failed: {result}"
    );
    assert!(result.get("structuredContent").is_none(), "{result}");
    let text = result["content"][0]["text"].as_str().expect("a text block");
    assert_eq!(
        text,
        "child: idle, its turn is done\nnext: message(agent: \"child\", text: \"<message>\") \
         sends its next message; archive(agent: \"child\") ends it\nm"
    );
}

/// One JSON-RPC request through mcplease's dispatch against the MCP tool set, mounted
/// against a host with no session: the schema and the argument parse are what answer.
async fn mcp_call(method: &str, params: Value) -> JsonRpcResponse {
    let request: JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }))
    .unwrap();
    let config = mcplease::ServerConfig::new(mcplease::server_info!());
    let context = RequestContext::from_request(&request);
    let state = Control::new(
        Executor::client(
            PathBuf::from("/nonexistent/efference/host.sock"),
            Fallback::None,
        ),
        false,
    );
    mcplease::handle_request::<mcp::Tools, Control>(request, state, context, &config)
        .await
        .expect("nothing here cancels")
}

/// The MCP mount names its agent on every call but `create`, `status` and `wait`: a
/// call without it is refused before any tool runs (docs/operating.md). The schema
/// that says so is pinned in `surfaces`.
#[tokio::test]
async fn an_mcp_call_without_agent_is_refused() {
    for (name, arguments) in [
        ("message", serde_json::json!({ "text": "hi" })),
        (
            "decide",
            serde_json::json!({ "seq": "1", "decision": "allow" }),
        ),
        ("interrupt", serde_json::json!({})),
        ("archive", serde_json::json!({})),
        ("transcript", serde_json::json!({})),
    ] {
        let response = mcp_call(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
        .await;
        let refused = response.error.is_some()
            || response
                .result
                .as_ref()
                .is_some_and(|result| result["isError"] == true);
        assert!(refused, "{name} ran without an agent: {response:?}");
        let text = format!("{response:?}");
        assert!(text.contains("agent"), "{name}: {text}");
        assert!(
            !text.contains("no host at"),
            "{name} reached the host: {text}"
        );
    }
}

/// The MCP mount holds its own cursor per agent: a first read is the tail and says so,
/// a second has nothing new, a zoom leaves the cursor where it was, and a later read is
/// what landed since. Another mount reads from its own first read.
#[tokio::test]
async fn the_mcp_mount_reads_what_is_new_with_its_own_cursor() {
    let mut log = Log::in_memory();
    log.append_submission(
        1,
        crate::log::Payload::user("hello transcript"),
        Some("sub-1".into()),
        Some("mcp[claude-code]".into()),
    )
    .unwrap();
    let (shared, client) = session_over(log).await;
    let control = mounted(client.clone(), false);
    let read = |seq: Option<&str>| Transcript {
        agent: "child".into(),
        seq: seq.map(str::to_string),
        since: None,
        tail: None,
        audit: false,
    };
    let me = || from_client("claude-code");

    let first = read(None).execute(control.clone(), me()).await.unwrap();
    assert_eq!(
        first,
        "transcript of child, seq 0 (your first read)\n0  you: hello transcript\n"
    );
    let again = read(None).execute(control.clone(), me()).await.unwrap();
    assert_eq!(
        again,
        "transcript of child: nothing new since your last read (seq 0)\n"
    );
    let zoom = read(Some("0"))
        .execute(control.clone(), me())
        .await
        .unwrap();
    assert!(zoom.contains("hello transcript"), "{zoom}");

    shared
        .append(1, crate::log::Payload::user("later"))
        .unwrap();
    let later = read(None).execute(control.clone(), me()).await.unwrap();
    assert_eq!(
        later,
        "transcript of child, new since your last read: seq 1\n1  operator: later\n"
    );

    let other = mounted(client, false);
    let fresh = read(None).execute(other, me()).await.unwrap();
    assert!(
        fresh.starts_with("transcript of child, seq 0–1 (your first read)"),
        "{fresh}"
    );
}

/// The command line reads the conversation too; `--audit` is the entry-by-entry
/// grain, and either line's seq fetches the whole entry.
#[tokio::test]
async fn the_command_line_reads_the_audit_grain_on_request() {
    let mut log = Log::in_memory();
    log.append(1, crate::log::Payload::user("hello transcript"))
        .unwrap();
    let (_shared, client) = session_over(log).await;
    let control = mounted(client, true);
    let text = Transcript {
        agent: "child".into(),
        seq: None,
        since: None,
        tail: None,
        audit: true,
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap();
    let line = text
        .lines()
        .find(|l| l.contains("hello transcript"))
        .unwrap_or_else(|| panic!("no user line in transcript:\n{text}"));
    assert!(line.contains(" user "), "{line}");
    let seq: u64 = line.split_whitespace().next().unwrap().parse().unwrap();

    let fetched = Entries {
        seqs: vec![seq],
        agent: Default::default(),
    }
    .execute(control, RequestContext::default())
    .await
    .unwrap();
    let entry = &fetched.entries[0];
    assert_eq!(entry["seq"], seq);
    assert_eq!(entry["payload"]["User"][0]["Text"], "hello transcript");
}

/// A `Proposed` config edit as a `Payload` — the spotter's prefix-promotion shape,
/// inert until an orchestrator resolves it (docs/permissions.md).
fn proposed_edit(command: &str) -> crate::log::Payload {
    crate::log::Payload::ConfigEdit(crate::log::ConfigEdit::new(
        crate::log::EditState::Proposed,
        crate::log::ConfigChange::Command {
            command: command.into(),
        },
    ))
}

/// `proposal` over the real router: committing folds the queued change into the
/// effective config and drains it from `state.proposals`; cancelling drains it
/// without applying anything; an unknown `seq` is an error naming it.
///
/// The proposals are filed with `Session::append` after construction — the
/// pending-proposals queue is populated by the live append path, not folded from a
/// log a `Session` is merely opened over.
#[tokio::test]
async fn proposal_commits_and_cancels_through_the_client_executor() {
    let (shared, client) = session_over(Log::in_memory()).await;
    let commit_seq = shared.append(0, proposed_edit("npm")).unwrap();
    let cancel_seq = shared.append(0, proposed_edit("cargo")).unwrap();
    let control = mounted(client, true);
    let proposal = |seq, cancel, note: &str| Proposal {
        seq,
        cancel,
        note: note.into(),
        agent: Default::default(),
    };

    let result = proposal(commit_seq, false, "looks safe")
        .execute(control.clone(), RequestContext::default())
        .await;
    assert_eq!(result.unwrap(), format!("committed {commit_seq}"));
    let result = proposal(cancel_seq, true, "not now")
        .execute(control.clone(), RequestContext::default())
        .await;
    assert_eq!(result.unwrap(), format!("cancelled {cancel_seq}"));

    let remaining = State {
        agent: Default::default(),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap();
    assert_eq!(
        remaining.state.get("proposals").cloned(),
        Some(serde_json::json!([])),
        "both proposals drained: {:?}",
        remaining.state
    );

    let err = proposal(999, false, "?")
        .execute(control, RequestContext::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("999"), "{err}");
}

/// Mounted against the host, a call that names no agent is an error naming the
/// argument, not a discovery of whatever is live: the mount's session selection is
/// explicit per call (docs/operating.md). The host path here has no listener, so the
/// error also says the host is not up.
#[tokio::test]
async fn an_unbound_mount_requires_agent() {
    let executor = ClientExecutor::new(
        PathBuf::from("/nonexistent/efference/host.sock"),
        Fallback::None,
    );
    let err = match executor.client(None).await {
        Ok(_) => panic!("an unbound mount resolved a session"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("pass `agent`"), "{err}");
    assert!(err.contains("no host at"), "{err}");
}

/// A call over MCP is named from the caller's `clientInfo`, without the caller saying
/// anything (docs/control-api.md); no caller name (the command line) means the
/// operator.
#[test]
fn source_comes_from_client_info() {
    assert_eq!(
        args::turn_source(&from_client("claude-code")).as_deref(),
        Some("mcp[claude-code]")
    );
    assert_eq!(args::turn_source(&RequestContext::default()), None);
}

/// A state body with a phase marker, so a test can tell which answer it holds.
const STATE_BODY: &str = r#"{"loop_phase":{"phase":"requesting"}}"#;

/// A client executor over an in-memory host whose waits answer `efference-settled`
/// from `header(n)`, `n` counting the wait requests so far (`None` sends no header),
/// plus the counter. Reads without `wait` answer the same body, unmarked.
fn waiting_executor(
    header: fn(usize) -> Option<&'static str>,
) -> (ClientExecutor, Client, Arc<std::sync::atomic::AtomicUsize>) {
    let waits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&waits);
    let handler = move |conn: trillium::Conn| {
        let counted = Arc::clone(&counted);
        async move {
            let mut conn = conn.with_status(200).with_body(STATE_BODY);
            if conn.querystring().contains("wait") {
                let n = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Some(value) = header(n) {
                    if value == "false" {
                        // A capped wait is a long one on a real host; the pause also
                        // lets the caller's deadline fire between polls.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    conn = conn.with_response_header("efference-settled", value);
                }
            }
            conn.halt()
        }
    };
    let client = Client::with_connector(trillium_testing::ServerConnector::new(handler));
    (
        ClientExecutor::defaulting_to(client.clone(), "child"),
        client,
        waits,
    )
}

/// A capped answer is polled again; the call ends on the first settled one.
#[tokio::test(start_paused = true)]
async fn capped_answers_are_repolled_until_settled() {
    let (executor, client, waits) =
        waiting_executor(|n| Some(if n < 2 { "false" } else { "true" }));
    let result = executor
        .settle_within(&client, None, client.settle())
        .await
        .unwrap();
    assert!(result.settled);
    assert_eq!(waits.load(std::sync::atomic::Ordering::SeqCst), 3);
}

/// Every answer capped, and `timeout_s` runs out: the current state, unsettled.
#[tokio::test(start_paused = true)]
async fn timeout_s_ends_a_run_of_capped_answers_unsettled() {
    let (executor, client, _) = waiting_executor(|_| Some("false"));
    let result = executor
        .settle_within(&client, Some(1), client.settle())
        .await
        .unwrap();
    assert!(!result.settled);
    assert_eq!(result.body, STATE_BODY);
}

/// A host that sends no header is an older host: its answer is a settled one.
#[tokio::test]
async fn an_answer_without_the_header_counts_as_settled() {
    let (executor, client, waits) = waiting_executor(|_| None);
    let result = executor
        .settle_within(&client, None, client.settle())
        .await
        .unwrap();
    assert!(result.settled);
    assert_eq!(waits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

mod surfaces;
mod tool_bytes;
