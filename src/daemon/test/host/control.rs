//! The agent verbs against a host, as `efference mcp` reaches it (docs/tools.md): the
//! overview of every session, an archive's cascade, and a creation the host cannot run.

use super::*;
use crate::control_tools::{Archive, ClientExecutor, Control, Create, Executor, Status};
use mcplease::traits::Tool as _;
use mcplease::types::RequestContext;

/// The host's socket stack at its root, as the MCP mount's host client.
async fn host_root(host: &Arc<Host>) -> ControlClient {
    let host = Arc::clone(host);
    let mut handler = (
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        handler(Transport::Socket { auth: None }, App::Host),
    );
    let context = trillium_testing::init(&mut handler).await;
    ControlClient::with_connector(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    )
}

/// The MCP mount's tools over `host`: every call names its agent.
async fn mcp_over(host: &Arc<Host>) -> Control {
    Control::new(
        Executor::Client(ClientExecutor::over(host_root(host).await)),
        false,
    )
}

/// A host running `parent`, and `child`, which `parent` created.
fn hosting_a_family(roots: &Roots) -> Arc<Host> {
    let host = Host::new(one_connection_registry(), baseline_env());
    for (id, created_by) in [("parent", None), ("child", Some("parent"))] {
        let session_dir = roots.0.join(".efference").join(id);
        std::fs::create_dir_all(&session_dir).expect("the session dir exists");
        host.adopt_with(
            id.into(),
            SessionRuntime::start(runtime::boot_in(&roots.0, &session_dir), host.swansong()),
            created_by.map(str::to_string),
            false,
        )
        .expect("the id is free in this test");
    }
    host
}

/// With no agent, the MCP mount reads one line per session the host holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_overview_reads_every_session_on_the_host() {
    let roots = Roots::new();
    let host = hosting_a_family(&roots);
    let overview = Status { agent: None }
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
    let host = hosting_a_family(&roots);
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
