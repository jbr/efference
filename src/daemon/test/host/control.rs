//! The agent verbs against a host, as `efference mcp` reaches it (docs/tools.md): the
//! overview of every session, an archive's cascade, and a creation the host cannot run.

use super::*;
use crate::control_tools::{Archive, ClientExecutor, Control, Create, Executor, Status};
use crate::log::{Approval, ToolCall};
use mcplease::traits::Tool as _;
use mcplease::types::RequestContext;

mod follow;
mod stream;
mod wait;

/// The host's socket stack at its root, as the MCP mount's host client.
async fn host_root(host: &Arc<Host>) -> ControlClient {
    host_root_behind(host, ()).await.0
}

/// [`host_root`] with `front` run first, and the swansong of the server it stands in
/// for, whose shutdown ends the streams it serves: what a test uses to make the host stop
/// answering.
async fn host_root_behind(
    host: &Arc<Host>,
    front: impl trillium::Handler,
) -> (ControlClient, trillium_tokio::Swansong) {
    let host = Arc::clone(host);
    let mut handler = (
        front,
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        handler(Transport::Socket { auth: None }, App::Host),
    );
    let context = trillium_testing::init(&mut handler).await;
    let server = context.swansong().clone();
    let client = ControlClient::with_connector(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    );
    (client, server)
}

/// The MCP mount's tools over `host`: every call names its agent.
async fn mcp_over(host: &Arc<Host>) -> Control {
    Control::new(
        Executor::Client(ClientExecutor::over(host_root(host).await)),
        false,
    )
}

/// A host running `parent`, and `child`, which `parent` created, each loop idle.
async fn hosting_a_family(roots: &Roots) -> Arc<Host> {
    let host = Host::new(one_connection_registry(), baseline_env());
    for (id, created_by) in [("parent", None), ("child", Some("parent"))] {
        let session_dir = roots.0.join(".efference").join(id);
        std::fs::create_dir_all(&session_dir).expect("the session dir exists");
        host.adopt_with(
            id.into(),
            runtime::started(runtime::boot_in(&roots.0, &session_dir), host.swansong()).await,
            created_by.map(str::to_string),
            false,
        )
        .expect("the id is free in this test");
    }
    host
}

/// A host running `ids`, none created by another, each loop idle.
async fn hosting(roots: &Roots, ids: &[&str]) -> Arc<Host> {
    let host = Host::new(one_connection_registry(), baseline_env());
    for id in ids {
        let session_dir = roots.0.join(".efference").join(id);
        std::fs::create_dir_all(&session_dir).expect("the session dir exists");
        host.adopt(
            id.to_string(),
            runtime::started(runtime::boot_in(&roots.0, &session_dir), host.swansong()).await,
        )
        .expect("the id is free in this test");
    }
    host
}

/// The MCP mount's tools over `host`, and the executor they share, which a test watches
/// through as `create` does.
async fn mount(host: &Arc<Host>) -> (Control, ClientExecutor) {
    mount_over(host_root(host).await)
}

/// The MCP mount's tools over the host client `host`, and the executor they share.
fn mount_over(host: ControlClient) -> (Control, ClientExecutor) {
    let executor = ClientExecutor::over(host);
    let control = Control::new(Executor::Client(executor.clone()), false);
    (control, executor)
}

/// A bash call running `command`.
fn bash(call_id: &str, command: &str) -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        call_id,
        serde_json::json!({ "command": command }),
    )
}

/// Hold `call` on `id` for a decision, logged first so it has a handle; returns the
/// handle's seq and the receiver the decision arrives on.
fn hold(host: &Host, id: &str, call: &ToolCall) -> (u64, oneshot::Receiver<Approval>) {
    let session = host.session(id).expect(id);
    let seq = session
        .append(
            1,
            Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![call.clone()],
            )),
        )
        .unwrap();
    session.set_phase(LoopPhase::Executing);
    (seq, session.register_pending(call, Default::default()))
}

/// With no agent, the MCP mount reads one line per session the host holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_overview_reads_every_session_on_the_host() {
    let roots = Roots::new();
    let host = hosting_a_family(&roots).await;
    let overview = Status {
        agent: Default::default(),
    }
    .execute(mcp_over(&host).await, RequestContext::default())
    .await
    .unwrap();
    assert_eq!(
        overview,
        "child: idle, its turn is done\nparent: idle, its turn is done"
    );
    host.swansong().shut_down().await;
}

/// An archive over the host route names the sessions that ended with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_names_its_cascade() {
    let roots = Roots::new();
    let host = hosting_a_family(&roots).await;
    let said = Archive {
        agent: "parent".into(),
    }
    .execute(mcp_over(&host).await, RequestContext::default())
    .await
    .unwrap();
    assert_eq!(
        said,
        "archived parent and the session it created (child); logs stay on disk"
    );
    host.swansong().shut_down().await;
}

/// A creation naming a model the host cannot run is refused before anything is
/// minted, and the refusal says what the host offers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creation_the_host_cannot_run_names_what_it_offers() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let err = Create {
        cwd: Some(roots.0.join("one")),
        name: Some("fresh".into()),
        message: Some("go".into()),
        model: Some("nope/model-x".into()),
        spotter_model: None,
        config: None,
        effort: None,
        temperature: None,
        autonomous: false,
        upstream: Vec::new(),
        detached: false,
        worktree: false,
    }
    .execute(mcp_over(&host).await, RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("nope/model-x"), "{err}");
    assert!(err.contains("this host offers test/<model>"), "{err}");
    assert!(host.ids().is_empty(), "nothing minted");
    host.swansong().shut_down().await;
}
