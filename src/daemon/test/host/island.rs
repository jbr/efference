//! An island's exit (docs/resumption.md): when its owner watch fires, the host
//! archives each session it holds through the archive path `DELETE` takes, and only
//! then shuts down. The owner is a pipe the test holds, as the launcher holds the
//! host's stdin; sessions are idle runtimes over file logs, or one whose model
//! request is held open, so nothing reaches a provider.

use super::*;
use crate::daemon::LoopPhase;
use crate::daemon::island::OwnerWatch;
use crate::frontend::SubmittedTurn;
use crate::log::{Payload, WorktreeEventKind};
use crate::session_ids::SessionIds;

/// A host over a state database in `roots`, and the register it reserves in.
fn host_and_register(roots: &Roots) -> (Arc<Host>, SessionIds) {
    let state = roots.0.join("state.db");
    let host = Host::new(
        one_connection_registry().with_history(crate::history::History::at(state.clone())),
        baseline_env(),
    );
    (host, SessionIds::at(state))
}

/// Host `id` over a file log at `<roots>/.efference/<id>/`, working in `working`, made
/// by `created_by` (not detached) when named, and with one operator turn when
/// `talked`. Returns the log's path.
fn adopt_logged(
    host: &Arc<Host>,
    register: &SessionIds,
    roots: &Roots,
    id: &str,
    working: &Path,
    created_by: Option<&str>,
    talked: bool,
) -> PathBuf {
    let dir = roots.0.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(crate::session_dir::LOG_NAME);
    let mut boot = runtime::boot(&roots.0.join("one"));
    boot.paths = SessionPaths::new(
        crate::policy::Directories::new(roots.0.clone(), working.to_path_buf()),
        dir,
    )
    .unwrap();
    boot.log = Log::create(&path, None).expect("a fresh log");
    register.reserve(id, &roots.0).unwrap();
    host.adopt_with(
        id.into(),
        SessionRuntime::start(boot, host.swansong()),
        created_by.map(String::from),
        false,
    )
    .expect("the id is free in this test");
    if talked {
        host.session(id)
            .unwrap()
            .append(1, Payload::User(vec![]))
            .unwrap();
    }
    path
}

/// Adopt `host` as an island whose owner is a pipe; the returned writer is the
/// launcher's end, and dropping it is the owner going.
fn island(host: &Arc<Host>) -> io::PipeWriter {
    let (reader, writer) = io::pipe().unwrap();
    let watch = OwnerWatch::spawn(reader, || panic!("the host already existed"));
    watch.adopt(host);
    writer
}

/// Wait for `host` to start shutting down, failing the test after ten seconds.
async fn shuts_down(host: &Host) {
    tokio::time::timeout(Duration::from_secs(10), host.swansong().shutting_down())
        .await
        .expect("the island shut down");
}

fn entries(path: &Path) -> Vec<Payload> {
    crate::persist::load_recording(path)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.payload)
        .collect()
}

fn archived_count(entries: &[Payload]) -> usize {
    entries
        .iter()
        .filter(|payload| matches!(payload, Payload::Archived { .. }))
        .count()
}

/// The owner going archives every session before the shutdown begins: each log with
/// a turn ends in exactly one `Archived`, a child ending with its creator included; the
/// sessions the exit archives itself name it as the `source`, and a child archived by
/// its creator's cascade names none, as any child ending with its creator does; a
/// clean worktree is retired as an archive retires it; a session that never had a
/// turn is discarded. All of it is on disk by the time the host starts shutting down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_islands_exit_archives_each_session_before_it_shuts_down() {
    let roots = Roots::new();
    crate::worktree::testing::repository(&roots.0);
    let (host, register) = host_and_register(&roots);
    let one = roots.0.join("one");
    let talked = adopt_logged(&host, &register, &roots, "talked", &one, None, true);
    let parent = adopt_logged(&host, &register, &roots, "parent", &one, None, true);
    let child = adopt_logged(
        &host,
        &register,
        &roots,
        "child",
        &one,
        Some("parent"),
        true,
    );
    let empty = adopt_logged(&host, &register, &roots, "empty", &one, None, false);
    let env = crate::env_table::ChildEnv::default();
    let worktree = crate::worktree::add(&roots.0, "wt-fox", &roots.0, &env)
        .unwrap()
        .path;
    let in_worktree = adopt_logged(&host, &register, &roots, "wt-fox", &worktree, None, true);
    let owner = island(&host);

    drop(owner);
    shuts_down(&host).await;

    let standalone_exit = Some("standalone exit".to_string());
    for (path, source) in [
        (&talked, &standalone_exit),
        (&parent, &standalone_exit),
        (&child, &None),
        (&in_worktree, &standalone_exit),
    ] {
        let entries = entries(path);
        assert!(
            matches!(entries.last(), Some(Payload::Archived { source: recorded }) if recorded == source),
            "{} ends in Archived with source {source:?}: {entries:?}",
            path.display()
        );
        assert_eq!(archived_count(&entries), 1, "{}", path.display());
    }
    assert!(
        entries(&in_worktree).iter().any(|payload| matches!(
            payload,
            Payload::Worktree(event) if matches!(event.kind, WorktreeEventKind::Removed { .. })
        )),
        "the clean worktree was retired"
    );
    assert!(!worktree.exists());
    assert!(
        !empty.parent().unwrap().exists(),
        "a session that never had a turn is discarded"
    );
    assert_eq!(register.holder("empty"), None);
    assert!(
        host.ids().is_empty(),
        "every session left before the shutdown"
    );
    host.swansong().shut_down().await;
}

/// A session whose model request is in flight is archived as `DELETE` would archive
/// it, and the exit does not wait on the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_islands_exit_archives_a_session_mid_turn() {
    let roots = Roots::new();
    // An upstream that takes each request and never answers.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&accepted);
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            held.push(stream);
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut boot = runtime::boot(&roots.0.join("one"));
    boot.client = Client::new(
        crate::api::Endpoint {
            provider: crate::log::Provider::Anthropic,
            url: Some(format!("http://{addr}/v1/messages")),
            auth_header: "x-api-key".into(),
            auth_value: "k".into(),
            session_id: None,
            extra_headers: Vec::new(),
            request: Default::default(),
            base: None,
        },
        "m",
        "sys",
        None,
    );
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt("busy".into(), SessionRuntime::start(boot, host.swansong()))
        .expect("the id is free in this test");
    let session = host.session("busy").unwrap();
    let owner = island(&host);
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while session.loop_phase() != LoopPhase::Requesting || accepted.load(Ordering::SeqCst) == 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the turn's request is in flight");

    drop(owner);
    shuts_down(&host).await;

    let log = session.snapshot();
    assert!(
        matches!(
            log.last().map(|entry| &entry.payload),
            Some(Payload::Archived { .. })
        ),
        "the session mid-turn ends in Archived"
    );
    assert!(host.ids().is_empty());
    host.swansong().shut_down().await;
}
