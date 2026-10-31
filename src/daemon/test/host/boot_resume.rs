//! The resume at boot (docs/resumption.md): a restarted host reopens the sessions that
//! were open, creators before the children that end with them, and leaves archived ones
//! and ones that cannot start dormant.
//!
//! Sessions are minted by `boot::prepare` on the seeded Codex catalog, so nothing here
//! reaches a provider; the state database and every root are under a temp directory.

use super::*;
use crate::boot::test::{Root, codex_registry, drop_boot, request};
use crate::log::Payload;

/// A session left on disk as a host that stopped would leave it: a turn said, and
/// `archived` when it was archived on purpose.
async fn left(
    root: &Root,
    id: &str,
    created_by: Option<&str>,
    archived: bool,
    extra: impl FnOnce(&mut Log),
) {
    let mut asked = request(root.path(), id, None);
    asked.lineage.created_by = created_by.map(String::from);
    let mut boot = crate::boot::prepare(asked)
        .await
        .expect("the session mints");
    boot.log.append(1, Payload::user("hi")).unwrap();
    extra(&mut boot.log);
    if archived {
        boot.log.append(1, Payload::archived(None)).unwrap();
    }
    drop_boot(boot);
}

/// A host over a state database remembering `root`, on the registry the sessions were
/// minted against.
fn host_over(root: &Root) -> Arc<Host> {
    let state = root.path().join("state.db");
    crate::remembered::Projects::at(state.clone()).record(&crate::remembered::Project {
        last_used_at: 1,
        cwd: root.path().to_path_buf(),
        config_key: "test".into(),
        config: None,
    });
    Host::new(
        codex_registry(root.path(), "boot").with_history(crate::history::History::at(state)),
        baseline_env(),
    )
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// Three logs — archived, open, and open with a child — and the host resumes the
/// second and third, child included, and not the first. The child's creator is hosted
/// by the time the child is, so the relation holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_host_resumes_what_was_open_and_not_what_was_archived() {
    let root = Root::new("boot-resume");
    left(&root, "closed", None, true, |_| {}).await;
    left(&root, "plain", None, false, |_| {}).await;
    left(&root, "parent", None, false, |_| {}).await;
    // Written last, so the scan reaches the child before its creator.
    tokio::time::sleep(Duration::from_millis(20)).await;
    left(&root, "kid", Some("parent"), false, |_| {}).await;
    let host = host_over(&root);

    let started = sorted(host.resume_open_sessions().await);

    assert_eq!(started, ["kid", "parent", "plain"]);
    assert_eq!(sorted(host.ids()), ["kid", "parent", "plain"]);
    assert_eq!(
        host.created_by("kid").as_deref(),
        Some("parent"),
        "the child was adopted after its creator was hosted"
    );
    host.swansong().shut_down().await;
}

/// A session that cannot be reopened stays dormant with its reason and the pass goes on;
/// archiving a resumed session and restarting leaves it dormant, and a resume after that
/// makes it open again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_fails_to_resume_stays_dormant_and_the_pass_goes_on() {
    let root = Root::new("boot-resume-fail");
    left(&root, "good", None, false, |_| {}).await;
    left(&root, "broken", None, false, |_| {}).await;
    std::fs::remove_file(root.session_dir("broken").join("config.toml")).unwrap();
    let host = host_over(&root);

    assert_eq!(host.resume_open_sessions().await, ["good"]);
    assert_eq!(host.ids(), ["good"]);
    let server = host_server(&host).await;
    let rows = json(
        server
            .get(&format!(
                "/sessions?state=archived&cwd={}",
                root.path().to_string_lossy().replace('/', "%2F")
            ))
            .await
            .body(),
    );
    let broken = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "broken")
        .expect("the broken session is listed dormant");
    assert!(
        broken["resume_failure"]
            .as_str()
            .is_some_and(|reason| reason.contains("config.toml is missing")),
        "{broken}"
    );

    // Archived on purpose: the next host leaves it be.
    let ended = host.end("good").expect("hosted").await;
    assert!(!ended.discarded);
    host.swansong().shut_down().await;
    let next = host_over(&root);
    assert_eq!(
        next.resume_open_sessions().await,
        Vec::<String>::new(),
        "an archived session is not resumed, and a broken one fails again"
    );
    assert!(next.ids().is_empty());

    // A resume by hand opens it again, and it is open for the host after.
    host_server(&next)
        .await
        .patch("/sessions/good")
        .with_json_body(&serde_json::json!({
            "state": "active", "cwd": root.path(), "env": {"HOME": "/h"},
        }))
        .await
        .assert_status(200);
    for _ in 0..200 {
        if next.session("good").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(next.session("good").is_some());
    next.swansong().shut_down().await;
    let after = host_over(&root);
    assert_eq!(after.resume_open_sessions().await, ["good"]);
    after.swansong().shut_down().await;
}

/// An island reopens nothing: an interrupted session under a remembered root stays
/// dormant, its log untouched and its row still `interrupted`, and a host that is not
/// an island, started after, resumes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_island_leaves_an_interrupted_session_dormant_and_another_host_resumes_it() {
    let root = Root::new("boot-resume-island");
    left(&root, "open", None, false, |_| {}).await;
    let log = root.session_dir("open").join(crate::session_dir::LOG_NAME);
    let before = std::fs::read(&log).unwrap();
    let island = host_over(&root);
    let (reader, owner) = io::pipe().unwrap();
    let watch =
        crate::daemon::island::OwnerWatch::spawn(reader, || panic!("the host already existed"));
    watch.adopt(&island);

    assert_eq!(island.resume_open_sessions().await, Vec::<String>::new());
    assert!(island.ids().is_empty());
    assert_eq!(std::fs::read(&log).unwrap(), before, "the log is untouched");
    let rows = json(
        host_server(&island)
            .await
            .get(&format!(
                "/sessions?state=archived&cwd={}",
                root.path().to_string_lossy().replace('/', "%2F")
            ))
            .await
            .body(),
    );
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "open")
        .expect("the session is listed dormant");
    assert_eq!(row["interrupted"], true, "{row}");
    drop(owner);
    tokio::time::timeout(Duration::from_secs(10), island.swansong().shutting_down())
        .await
        .expect("the island shut down when its owner went");

    let host = host_over(&root);
    assert_eq!(host.resume_open_sessions().await, ["open"]);
    assert_eq!(host.ids(), ["open"]);
    host.swansong().shut_down().await;
}

/// A session that never had a turn, left by a host that stopped, is not removed at
/// start: a host that is not an island reopens it as interrupted, and archiving it
/// discards the directory and releases its id (docs/resumption.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_session_a_host_left_is_reopened_and_discarded_at_archive() {
    let root = Root::new("boot-resume-empty");
    let boot = crate::boot::prepare(request(root.path(), "stub", None))
        .await
        .expect("the session mints");
    drop_boot(boot);
    let dir = root.session_dir("stub");
    // The scaffold's `first-turn` kickoff would give the reopened session a turn of
    // its own; the operator deleting that line is the session this test is about.
    let config = dir.join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let waiting: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with("first-turn"))
        .collect();
    assert_ne!(
        waiting.len(),
        text.lines().count(),
        "the scaffold has a kickoff"
    );
    std::fs::write(&config, waiting.join("\n")).unwrap();
    // Older than any host that starts now; nothing may remove it on that account.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let host = host_over(&root);

    assert_eq!(host.resume_open_sessions().await, ["stub"]);
    assert!(dir.join(crate::session_dir::LOG_NAME).is_file());
    let register = crate::session_ids::SessionIds::at(root.path().join("state.db"));
    assert!(
        register.holder("stub").is_some(),
        "the backfill registered it"
    );

    let ended = host.end("stub").expect("hosted").await;
    assert!(ended.discarded);
    assert!(!dir.exists());
    assert_eq!(register.holder("stub"), None, "the id can be minted again");
    host.swansong().shut_down().await;
}

/// A resume starts nothing: a restored notification no triage showed leaves the log as
/// the resume wrote it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_over_an_unshown_notification_appends_nothing() {
    let root = Root::new("boot-resume-quiet");
    left(&root, "quiet", None, false, |log| {
        log.append(
            1,
            Payload::Ambient(crate::log::Notification::new("ci", "build failed", None)),
        )
        .unwrap();
    })
    .await;
    let host = host_over(&root);
    assert_eq!(host.resume_open_sessions().await, ["quiet"]);
    let session = host.session("quiet").unwrap();
    let before = session.snapshot().len();

    tokio::time::sleep(Duration::from_millis(600)).await;

    assert_eq!(
        session.snapshot().len(),
        before,
        "nothing ran after the resume"
    );
    assert!(
        session.has_pending_notifications(),
        "the notification is still unshown"
    );
    host.swansong().shut_down().await;
}
