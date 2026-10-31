//! Session runtimes stop independently inside one process (docs/control-channel.md).

use super::*;
use crate::api::ModelCapabilities;
use crate::daemon::{SessionBoot, SessionRuntime};
use crate::log::{Provider, Stance};
use crate::spawn::group_alive;
use crate::spotter::StaticSpotter;
use crate::triage::Triager;
use std::path::Path;
use trillium_tokio::Swansong;

struct Roots(PathBuf);

impl Roots {
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-runtime-{nonce:016x}"));
        for label in ["one", "two"] {
            std::fs::create_dir_all(root.join(label).join("scratch")).unwrap();
        }
        Self(root.canonicalize().unwrap())
    }
}

impl Drop for Roots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A boot that never reaches a provider: the loop parks in `read_user_turn`
/// until shutdown, which is the state a hosted idle session sits in. A request it is
/// woken to send goes to a port nothing listens on, refused in process
/// (`crate::testing::refusing_endpoint`): it fails after the transport's retries, as it
/// would against a real closed port, and no socket is involved.
pub(super) fn boot(cwd: &Path) -> SessionBoot<StaticSpotter> {
    let scratch = cwd.join("scratch");
    let endpoint = crate::testing::refusing_endpoint(Provider::Anthropic, "/v1/messages");
    SessionBoot {
        client: Client::new(endpoint, "m", "sys"),
        toolbox: Toolbox::bash_and_mcps(cwd.to_path_buf(), Some(scratch)).unwrap(),
        triager: Triager::shared(),
        spotter: StaticSpotter {
            stance: Stance::Clear,
            reason: "test".into(),
        },
        log: Log::in_memory(),
        paths: SessionPaths::new(crate::policy::Directories::at(cwd), cwd.to_path_buf()).unwrap(),
        role: LoadedConfig::default(),
        autonomous: false,
        capabilities: ModelCapabilities {
            context_length: Some(200_000),
            ..ModelCapabilities::unknown()
        },
        model: "m".into(),
        inference: None,
        env_policy: crate::env_table::EnvPolicy::default(),
        keep_scratch: false,
        triage_prices: None,
        cache_lost_at: None,
    }
}

/// [`boot`] with explicit storage: the session's own `.efference/<id>/` dir, so
/// the runtime records the project's storage base the in-session mount scopes
/// to (docs/tools.md).
pub(super) fn boot_in(cwd: &Path, session_dir: &Path) -> SessionBoot<StaticSpotter> {
    let mut boot = boot(cwd);
    boot.paths = SessionPaths::new(
        crate::policy::Directories::at(cwd),
        session_dir.to_path_buf(),
    )
    .unwrap();
    boot
}

/// A working directory of a session's own, holding the `scratch` directory [`boot`]
/// names, and the guard that removes both when it drops (`crate::testing::temp_dir`).
/// The path is canonical, as a session's working directory is.
pub(super) fn workdir(label: &str) -> (PathBuf, tempfile::TempDir) {
    let dir = crate::testing::temp_dir(label);
    std::fs::create_dir_all(dir.path().join("scratch")).unwrap();
    (dir.path().canonicalize().unwrap(), dir)
}

/// [`SessionRuntime::start`], returned once the loop has gone idle. A test that stands in
/// for the loop afterwards — setting a phase, holding a gate — would otherwise race the
/// loop's first idle, which writes the phase over whatever the test set; a test that
/// lets the loop run against a scripted upstream submits its first turn to a loop
/// already waiting for one. This is how a fixture gets a runtime, whichever it does; a
/// test whose subject is the runtime's own start calls `start` itself.
pub(super) async fn started(boot: SessionBoot<StaticSpotter>, parent: &Swansong) -> SessionRuntime {
    let runtime = SessionRuntime::start(boot, parent);
    runtime
        .session()
        .wait_until("the loop went idle", || runtime.session().loop_idled())
        .await;
    runtime
}

/// Two runtimes under one parent: ending one reaps its background children and
/// removes its scratch while the other keeps serving; ending the parent ends
/// the rest. Shutdown completion means teardown has run, not merely begun.
///
/// `group_alive` is `killpg(pgid, 0)`, which stays true for a killed process until it
/// is waited — so asserting it directly after a shutdown is asserting that
/// teardown waited for the reap, not merely that it sent the signal. That is
/// the invariant (`reap_all_bg`, docs/process-monitor.md), and this test was
/// intermittently failing on it while teardown only signalled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_runtime_stops_without_touching_its_sibling() {
    let roots = Roots::new();
    let host = Swansong::new();
    let one = SessionRuntime::start(boot(&roots.0.join("one")), &host);
    let two = SessionRuntime::start(boot(&roots.0.join("two")), &host);

    let pgid_of = |runtime: &SessionRuntime| {
        let session = Arc::clone(runtime.session());
        session.spawn_background("sleep 30".into(), None).unwrap();
        session.process_monitor.lock().unwrap().running_pgids()[0]
    };
    let child_one = pgid_of(&one);
    let child_two = pgid_of(&two);
    assert!(group_alive(child_one) && group_alive(child_two));

    one.swansong().shut_down().await;
    assert!(
        !group_alive(child_one),
        "the stopped session's child was reaped"
    );
    assert!(group_alive(child_two), "the sibling's child kept running");
    assert!(
        !roots.0.join("one/scratch").exists(),
        "scratch removed at end"
    );
    assert!(roots.0.join("two/scratch").exists());
    one.finished().await.unwrap();

    // The sibling still works: a new background child starts under it.
    two.session()
        .spawn_background("sleep 30".into(), None)
        .unwrap();
    two.session()
        .wait_until("second child registered", || {
            two.session()
                .process_monitor
                .lock()
                .unwrap()
                .running_pgids()
                .len()
                == 2
        })
        .await;

    host.shut_down().await;
    assert!(
        !group_alive(child_two),
        "host shutdown reaps every session's children"
    );
    assert!(!roots.0.join("two/scratch").exists());
    two.finished().await.unwrap();
}

/// A `first-turn` config makes the harness submit the opening turn when the
/// log has none, carrying the reserved kickoff source (docs/context.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_turn_is_submitted_into_an_empty_log() {
    let roots = Roots::new();
    let host = Swansong::new();
    let mut boot = boot(&roots.0.join("one"));
    boot.role.role.first_turn = Some("say hello".into());
    let runtime = SessionRuntime::start(boot, &host);
    let session = Arc::clone(runtime.session());
    session
        .wait_until("the kickoff is recorded", || {
            session.snapshot().iter().any(|entry| {
                matches!(entry.payload, Payload::User(_))
                    && entry.source.as_deref() == Some(crate::names::sources::KICKOFF_SOURCE)
            })
        })
        .await;
    host.shut_down().await;
    runtime.finished().await.unwrap();
}

/// A log that already holds a user turn has had its opening: no kickoff.
#[tokio::test(start_paused = true)]
async fn a_first_turn_is_not_repeated_on_a_log_that_has_begun() {
    let roots = Roots::new();
    let host = Swansong::new();
    let mut boot = boot(&roots.0.join("one"));
    boot.role.role.first_turn = Some("say hello".into());
    boot.log = Log::in_memory_with(vec![
        LogEntry::new(0, Payload::user("earlier")).with_turn(1),
    ]);
    let runtime = SessionRuntime::start(boot, &host);
    assert!(runtime.session().queued_turns().is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(runtime.session().snapshot().len(), 1, "nothing was added");
    host.shut_down().await;
    runtime.finished().await.unwrap();
}
