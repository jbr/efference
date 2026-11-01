use super::http::DecisionBody;
use super::*;
use crate::client::Client as ControlClient;
use crate::record::log::ContentBlock;
use futures_lite::StreamExt;
use std::collections::BTreeMap;
use std::time::Duration;
use trillium_testing::TestServer;

/// A path for a file of one test's, in a directory of its own that goes when the returned
/// guard drops: a name that is neither shared with another process running the suite (a
/// concurrent run in another worktree) nor with another test.
fn temp_file(name: &str) -> (PathBuf, tempfile::TempDir) {
    let dir = crate::inference::testing::temp_dir("daemon");
    (dir.path().join(name), dir)
}

fn user(text: &str) -> Payload {
    Payload::user(text)
}

/// The launch baseline a test host is built with (docs/env.md): the environment a
/// session inherits when its creation omits one. Stated rather than captured, so
/// that path is reproducible instead of following whatever the test process was
/// launched with.
fn baseline_env() -> crate::config::env_table::EnvSnapshot {
    crate::config::env_table::EnvSnapshot::new(BTreeMap::new())
        .expect("an empty environment is valid")
}

/// One bash `ToolCall`, for exercising the effective-policy cache through `classify`.
fn bash_call(command: &str) -> ToolCall {
    ToolCall::new(
        crate::record::names::tools::BASH,
        "toolu_x",
        serde_json::json!({ "command": command }),
    )
}

/// A role that resolved from `path`, so `Session` opens it as the live [`ConfigFile`] — what
/// makes the refresh/revert path reachable in a test without a real daemon boot. The base role
/// stays default: the file's contents reach effective policy through a refresh, like any edit.
fn file_role(path: &std::path::Path) -> LoadedConfig {
    LoadedConfig {
        source: ConfigSource::Path(path.to_path_buf()),
        ..LoadedConfig::default()
    }
}

fn allow_grant(program: &str) -> Payload {
    committed_edit(crate::record::log::ConfigChange::Command {
        command: program.into(),
    })
}

/// A committed config edit (the synchronous/direct commit path) as a `Payload` — no prior
/// proposal to resolve.
fn committed_edit(change: crate::record::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::record::log::ConfigEdit::new(
        EditState::Committed,
        change,
    ))
}

/// A `Proposed` edit as a `Payload`, for driving the lifecycle in tests.
fn proposed_edit(change: crate::record::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::record::log::ConfigEdit::new(
        EditState::Proposed,
        change,
    ))
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

/// [`test_client`] with the session served at `/sessions/<id>`, as a host serves it:
/// the client is at the root, and a client re-based on the session's prefix reaches
/// it.
async fn test_client_at(shared: &Arc<Session>, id: &str) -> ControlClient {
    let shared = Arc::clone(shared);
    let mut handler = (
        trillium::Init::new(move |info: trillium::Info| {
            let shared = Arc::clone(&shared);
            async move { info.with_shared_state(shared) }
        }),
        Router::new().all(
            format!("/sessions/{id}/*"),
            handler(
                Transport::Socket { auth: None },
                App::session(std::env::current_dir().unwrap()),
            ),
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

/// History enough that a compaction has something to cover beyond its verbatim tail: two
/// operator messages, each about the tail's whole budget (backlog: cold-session.md).
fn append_compactable_history(shared: &Session) {
    let long = "x".repeat(crate::daemon::compact::COMPACTION_TAIL_TOKENS * 4);
    for _ in 0..2 {
        shared.append(1, Payload::user(long.clone())).unwrap();
    }
}

/// Every notification `session` has been told, in arrival order (docs/inbox.md): the
/// ones in its log are `Ambient` entries, with the slot and the wake they arrived with,
/// and a slot notification still queued for the loop's next triage is held in memory
/// only. A session's live loop drains its queue into its log after the idle debounce, so
/// a test cannot count on catching one queued.
fn told(session: &Session) -> Vec<Notification> {
    let logged = session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Ambient(n) => Some(n),
            _ => None,
        });
    let queued = session
        .pending_notifications()
        .into_iter()
        .filter(|n| n.seq.is_none())
        .map(|n| {
            let note = Notification::new(n.source, n.body, n.slot);
            if n.wake { note.waking() } else { note }
        });
    logged.chain(queued).collect()
}

/// The latest notification `session` has been told ([`told`]) that `matches` accepts, or
/// `None` when it has heard none.
fn heard(session: &Session, matches: impl Fn(&Notification) -> bool) -> Option<Notification> {
    told(session).into_iter().rev().find(|n| matches(n))
}

mod approvals;
mod ask;
mod background;
mod capture;
mod cold;
mod compact;
mod config_edits;
mod continuation;
mod decide_authority;
mod durable_inbox;
mod empty_completion;
mod escalate;
mod events;
mod frame_bytes;
mod http;
mod in_session;
mod interrupt;
mod keepalive;
mod log_reads;
mod loop_phase;
mod mcp_verbs;
mod metrics;
mod notifications;
mod pace;
mod phase_and_queue;
mod processes;
mod push;
mod push_routing;
mod redirect;
mod resumed_batch;
mod review;
mod route_bytes;
mod schema;
mod statics;
mod status;
mod subscriptions;
mod supervise;
mod tools;
mod topic_bytes;
mod topic_values;
mod triage_client;
mod triage_keepalive;
mod triage_stream;
mod upstream_hold;
mod verbs;
mod wait;

mod host;
mod roots;
mod runtime;
