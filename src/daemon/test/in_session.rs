//! The in-session `agents__*` tools: a session driving another through the host
//! directly, with the caller's identity enforced (docs/tools.md).

use super::*;
use crate::control_tools::{Archive, Control, Create, Executor, Interrupt, Message, Status};
use crate::daemon::{Host, SessionRuntime};
use mcplease::traits::Tool as _;
use mcplease::types::RequestContext;
use std::path::Path;

mod binding;
mod decisions;
mod messages;
mod reads;

pub(super) struct Fixture {
    root: PathBuf,
    host: Arc<Host>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A host with five sessions over two projects. `parent`, `peer`, `child`, and
    /// `grandchild` share one `.efference/` base (`child` commissioned by `parent`,
    /// `grandchild` by `child`); `stranger` sits in another project.
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-in-session-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let projects = [root.join("a"), root.join("b")];
        let ids = ["parent", "peer", "child", "grandchild", "stranger"];
        for project in &projects {
            for id in ids {
                std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
            }
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
        let adopt = |id: &str, cwd: &Path, created_by: Option<String>| {
            let boot = runtime::boot_in(cwd, &cwd.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                SessionRuntime::start(boot, host.swansong()),
                created_by,
                false,
            )
            .expect("the id is free in this test");
        };
        adopt("parent", &projects[0], None);
        adopt("peer", &projects[0], None);
        adopt("child", &projects[0], Some("parent".into()));
        adopt("grandchild", &projects[0], Some("child".into()));
        adopt("stranger", &projects[1], None);
        Self { root, host }
    }

    fn control(&self, caller: &str) -> Control {
        Control::new(Executor::host(&self.host, caller.to_string()), false)
    }

    fn session(&self, id: &str) -> Arc<Session> {
        self.host.session(id).expect(id)
    }
}

/// Log `calls` as one assistant turn of `session`'s, so each has a transcript handle;
/// returns the turn's seq.
fn issue(session: &Session, calls: Vec<ToolCall>) -> Seq {
    session
        .append(
            1,
            Payload::Assistant {
                output: crate::log::AssistantOutput::from_legacy_messages(vec![], vec![], calls),
                excluded: None,
            },
        )
        .unwrap()
}

/// A bash call the child holds for a decision.
fn curl() -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        "toolu_curl",
        serde_json::json!({
            "command": "curl -sI https://example.com",
            "intent": "fetch only the response headers",
        }),
    )
}

/// A message from the caller to `agent`.
fn message(agent: &str, text: &str, reply_to: Option<&str>) -> Message {
    Message {
        agent: agent.into(),
        text: text.into(),
        words: Vec::new(),
        reply_to: reply_to.map(str::to_string),
    }
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..250 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Rule 1: a session is never a target of itself, on any verb.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_target_is_refused() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let err = Interrupt {
        agent: "parent".into(),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("a session cannot address itself"), "{err}");

    // A message to the caller itself is refused on the same rule.
    let err = message("parent", "hello", None)
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("a session cannot address itself"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// An id reaches its session in any project (docs/tools.md): a session in another
/// project is read and messaged by its id, and what it hears names the sender's
/// project, on a note and, once the sender supervises it, on a turn. The overview
/// stays the caller's project, and an id the host does not hold is not found.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_in_another_project_is_reached_by_its_id() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let read = Status {
        agent: Some("stranger".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .expect("stranger is on this host");
    assert!(read.starts_with("stranger"), "{read}");

    let overview = Status { agent: None }
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap();
    assert!(!overview.contains("stranger"), "{overview}");

    let said = message("stranger", "the brief is in NOTES.md", None)
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        "left in stranger's inbox; it may set it aside (you don't drive it)"
    );
    let place = crate::home::tilde(&fixture.root.join("a"));
    let source = format!("session[parent in {place}]");
    let stranger = fixture.session("stranger");
    wait_for("the note reached stranger", || {
        stranger
            .pending_notifications()
            .iter()
            .any(|n| n.source == source)
            || stranger
                .snapshot()
                .iter()
                .any(|e| matches!(&e.payload, Payload::Ambient(n) if n.source == source))
    })
    .await;

    fixture
        .host
        .set_supervisor("stranger", Some("parent".into()))
        .expect("a supervisor in another project is accepted");
    message("stranger", "pick it up", None)
        .execute(control.clone(), RequestContext::default())
        .await
        .expect("parent supervises stranger");
    wait_for("stranger logged the turn", || {
        stranger
            .snapshot()
            .iter()
            .any(|entry| entry.source.as_deref() == Some(source.as_str()))
    })
    .await;
    let wire = serde_json::to_string(&crate::project::project(&stranger.snapshot())).unwrap();
    assert!(
        wire.contains(&format!("parent (agent in {place}): pick it up")),
        "{wire}"
    );

    let err = Status {
        agent: Some("ghost".into()),
    }
    .execute(control, RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("no agent \"ghost\" on this host"), "{err}");
    assert!(!err.contains("drive"), "not a permission refusal: {err}");

    fixture.host.swansong().shut_down().await;
}

/// A session creates only inside its own project (docs/tools.md): a cwd whose
/// `.efference/` base is not the caller's is refused before anything is minted,
/// since the child would run under that project's config rather than the caller's. A cwd inside the project passes
/// this check, and fails later here on the empty inference registry, with an error
/// that says what the host offers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_create_outside_the_project_is_refused() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let create = |cwd: &str| Create {
        cwd: Some(cwd.into()),
        name: Some("offshoot".into()),
        message: None,
        model: Some("frontier/supervisor-9".into()),
        spotter_model: None,
        config: None,
        effort: None,
        temperature: None,
        autonomous: false,
        upstream: Vec::new(),
        detached: false,
        worktree: false,
    };
    let err = create("../b")
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("outside this project"), "{err}");
    assert!(
        err.contains(&fixture.root.join("b").display().to_string()),
        "{err}"
    );
    assert!(
        !fixture.root.join("b/.efference/offshoot").exists(),
        "nothing minted"
    );

    let err = create("./sub/..")
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("outside this project"), "{err}");
    assert!(
        err.contains("this host has no connections configured"),
        "a model the host cannot run names what it can: {err}"
    );

    // A cwd inside one of the project's worktrees belongs to the project, as it does
    // for a caller working in one (docs/worktrees.md).
    let err = create("./.efference/worktrees/w/src")
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("outside this project"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// A succession (docs/tools.md): the successor's first turn is the brief under the
/// reserved hand-off source, the predecessor names its successor for the clients to
/// follow, and the successor — which drives its predecessor — may archive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successor_gets_the_brief_and_may_archive_its_predecessor() {
    let fixture = Fixture::new();
    fixture
        .host
        .record_succession("parent", "peer", "carry on".into());

    let parent = fixture.session("parent");
    assert_eq!(
        parent.identity().and_then(|identity| identity.successor),
        Some("peer".to_string())
    );
    let peer = fixture.session("peer");
    wait_for("the brief", || {
        peer.snapshot().iter().any(|entry| {
            entry.source.as_deref() == Some("handoff[parent]")
                && matches!(&entry.payload, Payload::User(_))
        })
    })
    .await;

    // A session that is neither a driver nor the successor is refused.
    let err = Archive {
        agent: "parent".into(),
    }
    .execute(fixture.control("stranger"), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("no agent") || err.contains("don't drive"),
        "{err}"
    );

    // `child` was the predecessor's, so it ends with it.
    let said = Archive {
        agent: "parent".into(),
    }
    .execute(fixture.control("peer"), RequestContext::default())
    .await
    .expect("the successor archives its predecessor");
    assert_eq!(
        said,
        "archived parent and the 2 sessions it created (child, grandchild); logs stay on disk"
    );
    wait_for("the predecessor to leave", || {
        fixture.host.session("parent").is_none()
    })
    .await;

    fixture.host.swansong().shut_down().await;
}

/// The hand-off's completion clears on the fast path (docs/permissions.md): only an
/// `archive` of the caller's own predecessor, by the host's record, counts. The same call
/// naming a sibling or a descendant, or from a session that is not the successor, does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_successors_archive_of_its_predecessor_completes_the_hand_off() {
    let fixture = Fixture::new();
    fixture
        .host
        .record_succession("parent", "peer", "carry on".into());
    let toolbox_for = |caller: &str| {
        let mut toolbox = Toolbox::bash_only();
        toolbox.mount_sessions(&fixture.host, caller.to_string());
        toolbox
    };
    let archive = |target: &str| {
        ToolCall::new(
            crate::protocol::agents::ARCHIVE_WIRE_NAME,
            "toolu_1",
            serde_json::json!({ "agent": target }),
        )
    };

    assert!(toolbox_for("peer").completes_hand_off(&archive("parent")));
    // The successor's other targets, and another session's archive of the predecessor.
    assert!(!toolbox_for("peer").completes_hand_off(&archive("stranger")));
    assert!(!toolbox_for("peer").completes_hand_off(&archive("child")));
    assert!(!toolbox_for("child").completes_hand_off(&archive("parent")));
    assert!(!toolbox_for("stranger").completes_hand_off(&archive("parent")));
    // The predecessor is not the successor of its successor.
    assert!(!toolbox_for("parent").completes_hand_off(&archive("peer")));
    // Another tool naming the predecessor is not the completion.
    let note = ToolCall::new(
        "agents__message",
        "toolu_2",
        serde_json::json!({ "agent": "parent", "text": "done?" }),
    );
    assert!(!toolbox_for("peer").completes_hand_off(&note));
    // No sessions provider mounted: nothing clears.
    assert!(!Toolbox::bash_only().completes_hand_off(&archive("parent")));

    fixture.host.swansong().shut_down().await;
}

/// A successor is created with its predecessor's inference options, not the
/// host's defaults (docs/tools.md): a supervisor on a large model must not hand
/// its place to a smaller one. The empty registry refuses the model, and the
/// refusal names the one the predecessor was created with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successor_is_created_with_its_predecessors_options() {
    let fixture = Fixture::new();
    fixture.host.record_options(
        "parent",
        crate::inference::SessionOptions {
            model: Some("frontier/supervisor-9".into()),
            effort: Some("max".into()),
            ..Default::default()
        },
    );
    let body = CreateSession {
        env: Default::default(),
        ..CreateSession::new(fixture.root.join("a"))
    };
    let err = fixture
        .host
        .hand_off("parent", body, "carry on".into())
        .await
        .expect_err("the empty registry refuses the carried model");
    assert!(err.contains("frontier"), "{err}");

    fixture.host.swansong().shut_down().await;
}
