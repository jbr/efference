//! The control tools, exercised over the real control router (docs/operating.md).

use super::*;
use crate::persist::Log;
use mcplease::traits::AsToolsList;
use mcplease::traits::Tool as _;
use mcplease::types::{JsonRpcRequest, JsonRpcResponse, RequestContext};
use std::sync::Arc;

/// A [`Control`] whose default session is a live [`router`](crate::daemon::router)
/// on `trillium_testing`'s in-memory transport — the whole request/response cycle,
/// with no socket bound (`daemon/test.rs` explains why the tests never bind one).
async fn test_state(cli: bool) -> Control {
    test_state_over(cli, Log::in_memory()).await
}

/// [`test_state`] over a log the test has already written to: the router has no
/// loop behind it, so an entry a test wants to read back is appended before mounting.
async fn test_state_over(cli: bool, log: Log) -> Control {
    let (shared, _turns) = crate::daemon::Session::new(std::env::current_dir().unwrap(), log);
    let mut handler = (
        trillium::Init::new(move |info: trillium::Info| {
            let shared = Arc::clone(&shared);
            async move { info.with_shared_state(shared) }
        }),
        crate::daemon::handler(
            crate::daemon::Transport::Socket { auth: None },
            crate::daemon::App::session(std::env::current_dir().unwrap()),
        ),
    );
    let context = trillium_testing::init(&mut handler).await;
    Control::with_client(
        Client::with_connector(
            trillium_testing::ServerConnector::new(handler).with_context(context),
        ),
        cli,
    )
}

/// One `tools/call` end to end: the JSON-RPC a mounted harness sends, through
/// mcplease's dispatch, through `execute`, through a real `GET /state`, and back
/// out as both content halves.
#[tokio::test]
async fn tools_call_state_reaches_the_daemon() {
    let state = test_state(false).await;
    let request: JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "state", "arguments": {} }
    }))
    .unwrap();
    let config = mcplease::ServerConfig::new(mcplease::server_info!());

    let context = RequestContext::from_request(&request);
    let response = mcplease::handle_request::<Tools, Control>(request, state, context, &config)
        .await
        .expect("nothing here cancels");

    let JsonRpcResponse { result, error, .. } = response;
    assert!(error.is_none(), "{error:?}");
    let result = result.expect("a result");
    assert_ne!(
        result.get("isError"),
        Some(&serde_json::Value::Bool(true)),
        "the tool call failed: {result}"
    );
    // The program-facing half is the typed `StateResult`: no `settled` (nothing
    // waited), and a `state` object that is the daemon's own `GET /state` body.
    let structured = result
        .get("structuredContent")
        .expect("state is a structured_output! tool");
    assert!(structured.get("settled").is_none());
    let daemon_state = structured.get("state").expect("the /state body");
    assert!(
        daemon_state.get("review_level").is_some() && daemon_state.get("pace").is_some(),
        "not a /state body: {daemon_state}"
    );
    // The model-facing half is the same value as text.
    let text = result["content"][0]["text"].as_str().expect("a text block");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text).unwrap(),
        *structured
    );
}

/// `transcript` and `entries` over the same router: a turn submitted through the
/// client shows up as one `user` line carrying its seq, and that seq fetches the
/// whole entry.
#[tokio::test]
async fn transcript_lines_carry_the_seq_that_entries_resolves() {
    let mut log = Log::in_memory();
    log.append(
        1,
        crate::log::Payload::User(vec![crate::log::ContentBlock::Text(
            "hello transcript".into(),
        )]),
    )
    .unwrap();
    let state = test_state_over(false, log).await;

    let text = Transcript {
        since: None,
        tail: None,
        session: None,
    }
    .execute(state.clone(), RequestContext::default())
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
        session: None,
    }
    .execute(state, RequestContext::default())
    .await
    .unwrap();
    let entry = &fetched.entries[0];
    assert_eq!(entry["seq"], seq);
    assert_eq!(entry["payload"]["User"][0]["Text"], "hello transcript");
}

/// A `Proposed` config edit as a `Payload` — the spotter's prefix-promotion shape,
/// inert until an orchestrator resolves it (docs/permissions.md).
fn proposed_edit(command: &str) -> crate::log::Payload {
    crate::log::Payload::ConfigEdit(crate::log::ConfigEdit {
        state: crate::log::EditState::Proposed,
        resolves: None,
        change: crate::log::ConfigChange::Command {
            command: command.into(),
        },
        note: String::new(),
    })
}

/// `proposal` over the real router: committing folds the queued change into the
/// effective config and drains it from `state.proposals`; cancelling drains it
/// without applying anything; an unknown `seq` is an error naming it.
///
/// Unlike [`test_state_over`], the proposals here are filed with `Session::append`
/// *after* construction — the pending-proposals queue is populated by the live
/// append path, not folded from a log a `Session` is merely opened over.
#[tokio::test]
async fn proposal_commits_and_cancels_through_the_client_executor() {
    let (shared, _turns) =
        crate::daemon::Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let commit_seq = shared.append(0, proposed_edit("npm")).unwrap();
    let cancel_seq = shared.append(0, proposed_edit("cargo")).unwrap();

    let mut handler = (
        trillium::Init::new({
            let shared = Arc::clone(&shared);
            move |info: trillium::Info| {
                let shared = Arc::clone(&shared);
                async move { info.with_shared_state(shared) }
            }
        }),
        crate::daemon::handler(
            crate::daemon::Transport::Socket { auth: None },
            crate::daemon::App::session(std::env::current_dir().unwrap()),
        ),
    );
    let context = trillium_testing::init(&mut handler).await;
    let state = Control::with_client(
        Client::with_connector(
            trillium_testing::ServerConnector::new(handler).with_context(context),
        ),
        false,
    );

    let result = Proposal {
        seq: commit_seq,
        cancel: false,
        note: "looks safe".into(),
        session: None,
    }
    .execute(state.clone(), RequestContext::default())
    .await;
    assert_eq!(result.unwrap(), format!("committed {commit_seq}"));

    let result = Proposal {
        seq: cancel_seq,
        cancel: true,
        note: "not now".into(),
        session: None,
    }
    .execute(state.clone(), RequestContext::default())
    .await;
    assert_eq!(result.unwrap(), format!("cancelled {cancel_seq}"));

    let remaining = State { session: None }
        .execute(state.clone(), RequestContext::default())
        .await
        .unwrap();
    let proposals = remaining.state.get("proposals").cloned();
    assert_eq!(
        proposals,
        Some(serde_json::json!([])),
        "both proposals drained: {:?}",
        remaining.state
    );

    let err = Proposal {
        seq: 999,
        cancel: false,
        note: "?".into(),
        session: None,
    }
    .execute(state, RequestContext::default())
    .await
    .unwrap_err();
    assert!(err.to_string().contains("999"), "{err}");
}

/// The advertised list is what a mounting harness caches, so both its **order** and
/// its behaviour hints are pinned here. Order is macro-expansion order; a reorder
/// moves every client's cached list. The hints are what an approval UX shows, and
/// the spec's defaults are pessimistic, so an unset one reads as a destructive
/// open-world mutation.
#[test]
fn tools_list_is_ordered_and_annotated() {
    let tools = Tools::tools_list();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "turn",
            "approve",
            "settle",
            "interrupt",
            "background",
            "pace",
            "review_level",
            "notify",
            "dismiss",
            "proposal",
            "image",
            "state",
            "transcript",
            "entries",
            "title",
            "sessions",
            "create",
            "hand_off",
            "archive",
            "subscribe",
            "unsubscribe",
        ]
    );

    let hints = |name: &str| {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} declares no annotations"));
        (
            annotations.read_only_hint,
            annotations.destructive_hint,
            annotations.idempotent_hint,
        )
    };

    for name in [
        "state",
        "transcript",
        "entries",
        "settle",
        "image",
        "sessions",
    ] {
        assert_eq!(hints(name).0, Some(true), "{name} is a read");
    }
    // Interrupt kills a process group; approving runs a call the rungs did not
    // clear; ending a session tears it down; committing a proposal folds a
    // change into the effective config.
    for name in ["interrupt", "approve", "archive", "proposal"] {
        assert_eq!(
            hints(name),
            (Some(false), Some(true), Some(false)),
            "{name}"
        );
    }
    for name in ["turn", "background", "pace", "notify", "create"] {
        assert_eq!(
            hints(name),
            (Some(false), Some(false), Some(false)),
            "{name} is a non-idempotent write"
        );
    }
    // Every tool declares a closed world: they reach one local daemon, nothing else.
    for tool in &tools {
        assert_eq!(
            tool.annotations.as_ref().unwrap().open_world_hint,
            Some(false),
            "{}",
            tool.name
        );
    }
}

/// Mounted against the host with no `--name`/`--socket`, a call that names no
/// session is an error naming the argument, not a discovery of whatever is live:
/// the mount's session selection is explicit per call (docs/operating.md). The
/// host path here has no listener, so the error also says the host is not up.
#[tokio::test]
async fn an_unbound_mount_requires_session() {
    let executor = Executor::client(
        std::path::PathBuf::from("/nonexistent/efference/host.sock"),
        Fallback::None,
    );
    let state = Control::new(executor, false);
    let err = match &state.executor {
        Executor::Client(executor) => match executor.client(None).await {
            Ok(_) => panic!("an unbound mount resolved a session"),
            Err(e) => e.to_string(),
        },
        Executor::Host(_) => panic!("this test bound the HTTP executor"),
    };
    assert!(err.contains("pass `session`"), "{err}");
    assert!(err.contains("no host at"), "{err}");
}

/// Every session tool but the host's own takes `session`, and those take none or
/// a required one — `hand_off` none, since a session hands off only its own work — the mount's whole addressing surface, pinned
/// against a tool that forgets the field.
#[test]
fn every_session_tool_takes_session() {
    for tool in Tools::tools_list() {
        let properties = tool.input_schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        match tool.name.as_str() {
            "sessions" | "create" | "hand_off" => {
                assert!(!properties.contains_key("session"), "{}", tool.name)
            }
            "archive" => assert_eq!(
                tool.input_schema["required"],
                serde_json::json!(["session"])
            ),
            _ => assert!(
                properties.contains_key("session"),
                "{} takes no `session`",
                tool.name
            ),
        }
    }
}

/// The documented temperature range (0–100 percent) must be what the model
/// sees, not just what the runtime accepts: 255 would otherwise serialize as
/// a 5.1 native temperature. Pinned through the advertised tool list, which is
/// exactly what a mounting harness caches.
#[test]
fn create_temperature_schema_matches_documented_range() {
    let temperature = Tools::tools_list()
        .into_iter()
        .find(|tool| tool.name == "create")
        .expect("create is mounted")
        .input_schema["properties"]["temperature"]
        .clone();
    assert_eq!(temperature["type"], "integer");
    assert_eq!(temperature["minimum"], serde_json::json!(0));
    assert_eq!(temperature["maximum"], serde_json::json!(100));
}

/// `worktree` is advertised on `create` and defaults to off, so an existing
/// caller's request body still parses.
#[test]
fn create_advertises_worktree_defaulting_off() {
    let tool = Tools::tools_list()
        .into_iter()
        .find(|tool| tool.name == "create")
        .expect("create is mounted");
    assert_eq!(
        tool.input_schema["properties"]["worktree"]["type"],
        "boolean"
    );
    let create: super::create::Create = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(!create.worktree);
    let create: super::create::Create =
        serde_json::from_value(serde_json::json!({"worktree": true})).unwrap();
    assert!(create.worktree);
}
