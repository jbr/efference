//! The in-session `sessions` tools: a session driving another through the host
//! directly, with the caller's identity enforced (docs/tools.md).

use super::*;
use crate::control_tools::{Control, Executor, Interrupt, Notify, Sessions, State, Turn};
use crate::daemon::{Host, SessionRuntime};
use mcplease::traits::Tool as _;
use mcplease::traits::ToolOutput as _;
use mcplease::types::RequestContext;
use std::path::Path;

struct Fixture {
    root: PathBuf,
    host: Arc<Host>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A host with four sessions over two projects. `parent`, `peer`, and `child`
    /// share one `.efference/` base (`child` commissioned by `parent`);
    /// `stranger` sits in another project.
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-in-session-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let projects = [root.join("a"), root.join("b")];
        let ids = ["parent", "peer", "child", "stranger"];
        for project in &projects {
            for id in ids {
                std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
            }
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty());
        let adopt = |id: &str, cwd: &Path, created_by: Option<String>| {
            let boot = runtime::boot_in(cwd, &cwd.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                SessionRuntime::start(boot, host.swansong()),
                created_by,
            );
        };
        adopt("parent", &projects[0], None);
        adopt("peer", &projects[0], None);
        adopt("child", &projects[0], Some("parent".into()));
        adopt("stranger", &projects[1], None);
        Self { root, host }
    }

    fn control(&self, caller: &str) -> Control {
        Control::new(
            Executor::host(
                tokio::runtime::Handle::current(),
                Arc::clone(&self.host),
                caller.to_string(),
            ),
            false,
        )
    }
}

/// Rule 1: a session is never a target of itself, on any verb.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_target_is_refused() {
    let fixture = Fixture::new();
    let mut control = fixture.control("parent");
    let err = Interrupt {
        session: Some("parent".into()),
    }
    .execute(&mut control, &RequestContext::default())
    .unwrap_err()
    .to_string();
    assert!(err.contains("cannot target itself"), "{err}");

    // A call naming no session defaults to the caller, which the same rule
    // refuses rather than silently reading the caller's own state.
    // `StateResult` has no `Debug`, so take the error rather than `unwrap_err`.
    let err = State { session: None }
        .execute(&mut control, &RequestContext::default())
        .err()
        .expect("a self-target is refused")
        .to_string();
    assert!(err.contains("cannot target itself"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// Rule 2: a mutating verb reaches descendants only. A peer in the same project
/// is refused a `turn`, while `notify` — which is not a mutation — reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_descendant_mutation_is_refused_but_a_notify_reaches_it() {
    let fixture = Fixture::new();
    let mut control = fixture.control("parent");
    let err = Turn {
        text: vec!["steer".into()],
        wait: false,
        timeout_s: None,
        source: None,
        session: Some("peer".into()),
    }
    .execute(&mut control, &RequestContext::default())
    .unwrap_err()
    .to_string();
    assert!(err.contains("not an ancestor"), "{err}");

    Notify {
        source: "parent".into(),
        body: vec!["hello peer".into()],
        slot: None,
        replaces: None,
        session: Some("peer".into()),
    }
    .execute(&mut control, &RequestContext::default())
    .expect("notify reaches anything in the project");

    fixture.host.swansong().shut_down().await;
}

/// A descendant `turn` lands on the child's log carrying the caller's
/// `session[<id>]` provenance (docs/tools.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_descendant_turn_lands_with_the_session_source() {
    let fixture = Fixture::new();
    let mut control = fixture.control("parent");
    Turn {
        text: vec!["do the thing".into()],
        wait: false,
        timeout_s: None,
        source: None,
        session: Some("child".into()),
    }
    .execute(&mut control, &RequestContext::default())
    .expect("a descendant is a valid target");

    let child = fixture.host.session("child").unwrap();
    let landed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if child
                .snapshot()
                .iter()
                .any(|entry| entry.source.as_deref() == Some("session[parent]"))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(landed.is_ok(), "the child never logged the submitted turn");

    fixture.host.swansong().shut_down().await;
}

/// Project scope is a visibility boundary: a session sharing no `.efference/`
/// base with the caller is not found, not forbidden, and does not appear in the
/// caller's `sessions` listing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_in_another_project_is_not_found() {
    let fixture = Fixture::new();
    let mut control = fixture.control("parent");
    let err = State {
        session: Some("stranger".into()),
    }
    .execute(&mut control, &RequestContext::default())
    .err()
    .expect("a session in another project is not found")
    .to_string();
    assert!(err.contains("no session"), "{err}");
    assert!(err.contains("in this project"), "{err}");
    assert!(!err.contains("ancestor"), "not a permission refusal: {err}");

    let listing = Sessions {}
        .execute(&mut control, &RequestContext::default())
        .expect("the project lists")
        .to_text();
    for id in ["parent", "peer", "child", "\"created_by\": \"parent\""] {
        assert!(listing.contains(id), "{id} missing from {listing}");
    }
    assert!(!listing.contains("stranger"), "{listing}");

    fixture.host.swansong().shut_down().await;
}
