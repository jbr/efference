use super::http::DecisionBody;
use super::*;
use crate::client::Client as ControlClient;
use crate::log::ContentBlock;
use futures_lite::StreamExt;
use std::collections::BTreeMap;
use std::time::Duration;
use trillium_testing::TestServer;

fn user(text: &str) -> Payload {
    Payload::User(vec![ContentBlock::Text(text.into())])
}

/// The launch baseline a test host is built with (docs/env.md): the environment a
/// session inherits when its creation omits one. Stated rather than captured, so
/// that path is reproducible instead of following whatever the test process was
/// launched with.
fn baseline_env() -> crate::env_table::EnvSnapshot {
    crate::env_table::EnvSnapshot::new(BTreeMap::new()).expect("an empty environment is valid")
}

/// One bash `ToolCall`, for exercising the effective-policy cache through `classify`.
fn bash_call(command: &str) -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        "toolu_x",
        serde_json::json!({ "command": command }),
    )
}

/// A role that resolved from `path`, so `Session` opens it as the live [`ConfigFile`] — what
/// makes the refresh/revert path reachable in a test without a real daemon boot. The base role
/// stays default: the file's contents reach effective policy through a refresh, like any edit.
fn file_role(path: &std::path::Path) -> LoadedConfig {
    LoadedConfig {
        role: crate::config::Config::default(),
        source: ConfigSource::Path(path.to_path_buf()),
    }
}

fn allow_grant(program: &str) -> Payload {
    committed_edit(crate::log::ConfigChange::Command {
        command: program.into(),
    })
}

/// A committed config edit (the synchronous/direct commit path) as a `Payload` — no prior
/// proposal to resolve.
fn committed_edit(change: crate::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::log::ConfigEdit {
        state: EditState::Committed,
        resolves: None,
        change,
        note: String::new(),
    })
}

/// A `Proposed` edit as a `Payload`, for driving the lifecycle in tests.
fn proposed_edit(change: crate::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::log::ConfigEdit {
        state: EditState::Proposed,
        resolves: None,
        change,
        note: String::new(),
    })
}

/// A [`ControlClient`] speaking to the real [`router`] over an in-memory transport.
///
/// `trillium_testing::connector` builds a client-side connector from a handler, so the
/// full request/response cycle runs — routing, extractors, SSE bodies — with no socket
/// bound. That matters beyond tidiness: a bound path is a shared name (two tests using
/// one collide, a panicking test leaks it) and Darwin caps a socket path at `SUN_LEN`
/// (104 bytes), which the harness's long per-session `$TMPDIR` exceeds.
///
/// The shared state the handlers read via `conn.shared_state::<Arc<Session>>()` is
/// server-global, so it arrives through [`Init`] rather than the `with_shared_state` a
/// real bind uses (`daemon/serve.rs`) — and `trillium_testing::init` is what *runs* that
/// init and hands back the resulting context. The bare `trillium_testing::connector`
/// skips startup, so a handler's init never fires and every route answers
/// `500 missing shared state`.
///
/// Streaming routes are why this returns a [`ControlClient`] rather than a
/// `TestServer`: that type reads the response body eagerly, which never returns on
/// `/events`.
async fn test_client(shared: &Arc<Session>) -> ControlClient {
    let shared = Arc::clone(shared);
    let mut handler = (
        trillium::Init::new(move |info: trillium::Info| {
            let shared = Arc::clone(&shared);
            async move { info.with_shared_state(shared) }
        }),
        handler(
            Transport::Socket { auth: None },
            App::session(std::env::current_dir().unwrap()),
        ),
    );
    let context = trillium_testing::init(&mut handler).await;
    ControlClient::with_connector(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    )
}

/// The real [`router`] as a [`TestServer`], for reads whose *response* is under test —
/// status, headers, JSON body — rather than the client's rendering of it.
/// [`test_client`] is the one to use for streams; this one reads bodies eagerly, so it
/// would never return on `/events`.
async fn test_server(shared: &Arc<Session>) -> TestServer<impl trillium::Handler> {
    let shared = Arc::clone(shared);
    TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let shared = Arc::clone(&shared);
            async move { info.with_shared_state(shared) }
        }),
        handler(
            Transport::Socket { auth: None },
            App::session(std::env::current_dir().unwrap()),
        ),
    ))
    .await
}

/// (id, event_type, data) of the next SSE event, with a timeout so a hung tail
/// fails loudly instead of blocking the suite.
async fn next_event(
    events: &mut trillium_client::sse::EventStream,
) -> (Option<String>, Option<String>, String) {
    let event = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("an event within the timeout")
        .expect("the stream has not ended")
        .expect("the event parsed");
    (
        event.id().map(str::to_string),
        event.event_type().map(str::to_string),
        event.data().to_string(),
    )
}

/// History enough that a fold has something to cover beyond its verbatim tail: two
/// operator messages, each about the tail's whole budget (backlog: cold-session.md).
fn append_foldable_history(shared: &Session) {
    let long = "x".repeat(crate::daemon::fold::FOLD_TAIL_TOKENS * 4);
    for _ in 0..2 {
        shared
            .append(1, Payload::User(vec![ContentBlock::Text(long.clone())]))
            .unwrap();
    }
}

mod approvals;
mod ask;
mod background;
mod capture;
mod cold;
mod config_edits;
mod continuation;
mod events;
mod fold;
mod http;
mod in_session;
mod interrupt;
mod keepalive;
mod log_reads;
mod metrics;
mod notifications;
mod pace;
mod processes;
mod push;
mod resumed_batch;
mod review;
mod status;
mod subscriptions;
mod supervise;
mod tools;
mod upstream_hold;
mod views;

mod host;
mod roots;
mod runtime;
