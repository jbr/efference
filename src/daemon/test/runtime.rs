//! Session runtimes stop independently inside one process (docs/control-channel.md).

use super::*;
use crate::api::{Endpoint, ModelCapabilities, Provider};
use crate::daemon::{SessionBoot, SessionRuntime};
use crate::log::Stance;
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
/// until shutdown, which is the state a hosted idle session sits in.
pub(super) fn boot(cwd: &Path) -> SessionBoot<StaticSpotter> {
    let scratch = cwd.join("scratch");
    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: Some("http://127.0.0.1:9/v1/messages".into()),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    SessionBoot {
        client: Client::new(endpoint, "m", "sys", None),
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

fn alive(pgid: u32) -> bool {
    // SAFETY: signal 0 checks existence and permission only; no memory effects.
    unsafe { libc::killpg(pgid as libc::pid_t, 0) == 0 }
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting: {what}"));
}

/// Two runtimes under one parent: ending one reaps its background children and
/// removes its scratch while the other keeps serving; ending the parent ends
/// the rest. Shutdown completion means teardown has run, not merely begun.
///
/// `alive` is `killpg(pgid, 0)`, which stays true for a killed process until it
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
    assert!(alive(child_one) && alive(child_two));

    one.swansong().shut_down().await;
    assert!(!alive(child_one), "the stopped session's child was reaped");
    assert!(alive(child_two), "the sibling's child kept running");
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
    wait_until("second child registered", || {
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
        !alive(child_two),
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
    wait_until("the kickoff is recorded", || {
        session.snapshot().iter().any(|entry| {
            matches!(entry.payload, crate::log::Payload::User(_))
                && entry.source.as_deref() == Some(crate::frontend::KICKOFF_SOURCE)
        })
    })
    .await;
    host.shut_down().await;
    runtime.finished().await.unwrap();
}

/// A log that already holds a user turn has had its opening: no kickoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_turn_is_not_repeated_on_a_log_that_has_begun() {
    let roots = Roots::new();
    let host = Swansong::new();
    let mut boot = boot(&roots.0.join("one"));
    boot.role.role.first_turn = Some("say hello".into());
    boot.log = Log::in_memory_with(vec![crate::log::LogEntry {
        seq: 0,
        turn: 1,
        at: crate::log::Timestamp(0),
        submission_id: None,
        source: None,
        payload: crate::log::Payload::User(vec![crate::log::ContentBlock::Text("earlier".into())]),
    }]);
    let runtime = SessionRuntime::start(boot, &host);
    assert!(runtime.session().queued_turns().is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(runtime.session().snapshot().len(), 1, "nothing was added");
    host.shut_down().await;
    runtime.finished().await.unwrap();
}
