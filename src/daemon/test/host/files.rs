//! A hosted session's `/files` (docs/web-bind.md), mounted under its prefix by
//! the host's routes rather than at a session bind's root.

use super::*;

/// The host routes alone over the in-memory transport, with the host as shared
/// state — what both tests here serve.
fn hosted_files(host: &Arc<Host>) -> impl trillium::Handler {
    let host = Arc::clone(host);
    (
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        crate::daemon::host::host_routes(true),
    )
}

/// Each session's `/files` under its own prefix, in both spellings of a
/// directory: the wildcard capture the router hands this surface is the path
/// below the mount, so a listing has to come back under the whole prefix
/// without the route reconstructing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosted_sessions_files_are_served_under_its_prefix() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let runtime = SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong());
    host.adopt("one".into(), runtime)
        .expect("the id is free in this test");
    let server = TestServer::new(hosted_files(&host)).await;

    let listing = server.get("/sessions/one/files/").await;
    listing.assert_status(200);
    let names: Vec<String> = json(listing.body())["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"scratch".to_string()), "{names:?}");

    for path in [
        "/sessions/one/files/scratch",
        "/sessions/one/files/scratch/",
    ] {
        let conn = server.get(path).await;
        conn.assert_status(200);
        assert_eq!(json(conn.body())["path"], "scratch", "{path}");
    }

    server
        .get("/sessions/one/files/nowhere.txt")
        .await
        .assert_status(404);
}

/// A change stream under the prefix is driven, not just answered: the `Sse`
/// stage hands its stream to the transport through the upgrade hook, which the
/// server looks up on the whole stack, so a `/files` handler reached through a
/// wrapper that forwards only `run` answers `200` and closes. A client then
/// hears nothing until its reconnect, and polls on the reconnect interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosted_sessions_directory_stream_delivers_its_pokes() {
    let roots = Roots::new();
    let cwd = roots.0.join("one");
    // The session's storage under `.efference/`, as a hosted session's is: the runtime
    // creates its attachment store there as it starts, which the watched directory
    // would otherwise report ahead of the poke.
    let session_dir = cwd.join(".efference").join("one");
    std::fs::create_dir_all(&session_dir).expect("the session dir exists");
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let runtime = SessionRuntime::start(runtime::boot_in(&cwd, &session_dir), host.swansong());
    host.adopt("one".into(), runtime)
        .expect("the id is free in this test");
    let mut handler = hosted_files(&host);
    let context = trillium_testing::init(&mut handler).await;
    let client = trillium_client::Client::new(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    )
    .with_base("http://localhost/");

    let mut events = client
        .get("/sessions/one/files/")
        .into_sse()
        .await
        .expect("open the directory stream");
    std::fs::write(cwd.join("poked.txt"), "arrived").expect("write");
    // Runtime startup can create `attachments` after the stream opens. Wait for
    // this write rather than assuming it is the first filesystem event.
    let kind = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, kind, data) = next_event(&mut events).await;
            if data == "poked.txt" {
                break kind;
            }
        }
    })
    .await
    .expect("the written file's change arrives");
    assert_eq!(kind.as_deref(), Some("change"));

    host.swansong().shut_down().await;
}
