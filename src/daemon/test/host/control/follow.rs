//! `efference control status --follow`, `--output json`, and a named session's
//! resolution through the host (docs/operating.md): the blocks a script reads as an
//! agent changes, from any directory.

use super::*;
use crate::control_tools::{CliStatus, Output, follow_status, named};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// A writer a test reads while `follow_status` still holds it.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    /// The text once it contains `needle`, or a panic after ten seconds.
    async fn until(&self, needle: &str) -> String {
        let wait = async {
            loop {
                let text = self.text();
                if text.contains(needle) {
                    return text;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .unwrap_or_else(|_| panic!("never saw {needle:?} in {:?}", self.text()))
    }
}

fn agents(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

/// The current status, then a gate's block, then the end: blocks separated by a blank
/// line, a working status printing nothing, and the call returning once the agent has
/// ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_prints_the_status_then_each_decision_point_then_the_end() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]);
    let root = host_root(&host).await;
    let out = Shared::default();
    let following = tokio::spawn({
        let (root, mut out) = (root.clone(), out.clone());
        async move { follow_status(&root, &agents(&["child"]), None, &mut out).await }
    });
    out.until("child: idle, its turn is done").await;

    let (seq, _decision) = hold(&host, "child", &bash("toolu_1", "ls ~"));
    let text = out
        .until(&format!(
            "waiting for your decision on bash `ls ~` (seq {seq})"
        ))
        .await;
    assert!(
        text.starts_with("child: idle, its turn is done\n")
            && text.contains("\n\nchild: waiting for your decision"),
        "{text}"
    );

    Archive {
        agent: "child".into(),
    }
    .execute(mcp_over(&host).await, RequestContext::default())
    .await
    .unwrap();
    let text = out.until("child: ended; its log stays on disk").await;
    let result = tokio::time::timeout(Duration::from_secs(10), following)
        .await
        .expect("follow returns once the agent has ended")
        .unwrap();
    result.unwrap();
    assert_eq!(text.matches("child: ").count(), 3, "{text}");
    host.swansong().shut_down().await;
}

/// A host that stops answering ends the follow with an error, not a clean exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_fails_when_the_host_goes_away() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]);
    let down = Arc::new(AtomicBool::new(false));
    let front = {
        let down = Arc::clone(&down);
        move |conn: trillium::Conn| {
            let down = Arc::clone(&down);
            async move {
                match down.load(Ordering::SeqCst) {
                    true => conn.with_status(503).halt(),
                    false => conn,
                }
            }
        }
    };
    let (root, server) = host_root_behind(&host, front).await;
    let out = Shared::default();
    let following = tokio::spawn({
        let mut out = out.clone();
        async move { follow_status(&root, &agents(&["child"]), None, &mut out).await }
    });
    out.until("child: idle, its turn is done").await;
    down.store(true, Ordering::SeqCst);
    server.shut_down().await;
    let err = tokio::time::timeout(Duration::from_secs(10), following)
        .await
        .expect("follow gives up")
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("unreachable"), "{err}");
    host.swansong().shut_down().await;
}

/// An agent the host does not hold is refused up front, naming what it does hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_refuses_an_agent_the_host_lacks() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]);
    let err = follow_status(
        &host_root(&host).await,
        &agents(&["stranger"]),
        None,
        &mut Shared::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "no agent \"stranger\" on this host; it holds child"
    );
    host.swansong().shut_down().await;
}

/// `--output json` is the `StatusView`: one object for an agent, an array for the
/// overview.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_json_is_the_status_view() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child", "other"]);
    let (_, executor) = mount(&host).await;
    let control = Control::new(Executor::Client(executor), true);
    let read = |agent: &[&str]| CliStatus {
        agent: agents(agent),
        follow: false,
        output: Output::Json,
    };
    let one = read(&["child"])
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap();
    let view: crate::daemon::status::StatusView = serde_json::from_str(&one).unwrap();
    assert_eq!(view.id, "child");
    assert_eq!(
        view.condition,
        crate::daemon::status::Condition::Idle { closing: None }
    );
    let all = read(&[])
        .execute(control, RequestContext::default())
        .await
        .unwrap();
    let views: Vec<crate::daemon::status::StatusView> = serde_json::from_str(&all).unwrap();
    assert_eq!(
        views
            .iter()
            .map(|view| view.id.as_str())
            .collect::<Vec<_>>(),
        ["child", "other"]
    );
    host.swansong().shut_down().await;
}

/// A named session resolves through the host that holds it, from a directory that is
/// not the session's: no `.efference/<id>/control.sock` is looked for first. One the
/// host does not hold, and nothing else serves, says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_named_session_resolves_through_the_host() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]);
    let root = host_root(&host).await;
    let socket = Path::new("/elsewhere/x.sock");
    let client = named(Some(root.clone()), socket, "child").await.unwrap();
    assert_eq!(client.session_id(), Some("child"));
    assert!(client.state().await.unwrap().contains("loop_phase"));

    let err = match named(Some(root), socket, "stranger").await {
        Ok(_) => panic!("the host does not hold it"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("the host at /elsewhere/x.sock does not list it"),
        "{err}"
    );
    let err = match named(None, socket, "stranger").await {
        Ok(_) => panic!("no host"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("no host is running at /elsewhere/x.sock"),
        "{err}"
    );
    host.swansong().shut_down().await;
}
