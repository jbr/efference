//! A session leaves the host's map after its archive is recorded (`Departure`,
//! docs/control-channel.md): an end appends `Archived` and closes the log before the
//! session's watcher removes the entry, and resolves once the entry has gone. The watcher
//! runs once the session's loop has returned, so it can reach its removal while the end is
//! still recording; the tests here land it just before the record (`before_archived`).
//! A session that ends with no end under way leaves without waiting for one.

use super::resume::{adopt_logged, last_payload};
use super::*;

/// A host with `creator` and the child it made, `child`, each over a file log holding a
/// `User` entry, so an archive records them rather than discarding them
/// (docs/resumption.md).
async fn creator_and_child(roots: &Roots) -> (Arc<Host>, PathBuf, PathBuf) {
    let host = Host::new(one_connection_registry(), baseline_env());
    let creator = adopt_logged(&host, roots, "creator", None).await;
    let child = adopt_logged(&host, roots, "child", Some("creator")).await;
    for id in ["creator", "child"] {
        host.session(id)
            .unwrap()
            .append(1, Payload::user("hi"))
            .unwrap();
    }
    (host, creator, child)
}

/// Arm `id`'s next archive to run its watcher's removal step just before the `Archived`
/// entry is appended: the point a watcher reaches when nothing holds it back, since the
/// session's loop has returned by then. Once that point is reached, holds whether the
/// session was still listed after the step.
fn watcher_lands_before_the_record(host: &Arc<Host>, id: &str) -> Arc<Mutex<Option<bool>>> {
    let still_listed = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&still_listed);
    let reach = Arc::downgrade(host);
    let removing = id.to_string();
    let session = host.session(id).expect("hosted");
    *session.before_archived.lock().unwrap() = Some(Box::new(move |_: &Session| {
        let host = reach.upgrade().expect("the host outlives the end");
        host.sessions_mut().remove_hosted(&removing);
        *slot.lock().unwrap() = Some(host.listed(&removing).is_some());
    }));
    still_listed
}

/// **The cascade.** Archiving a creator ends its child through the creator's watcher, and
/// the child's own watcher runs once the child's loop has returned. Landing just before the
/// child's `Archived` entry is appended, it leaves the child listed; the archive resolves
/// once both have left the map, and each log ends in its record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cascaded_child_is_listed_until_its_archive_is_recorded() {
    let roots = Roots::new();
    let (host, creator, child) = creator_and_child(&roots).await;
    let at_the_record = watcher_lands_before_the_record(&host, "child");

    host.end("creator").expect("hosted").await;

    // Read first: the child's end has run by now only if the archive waited for it.
    assert_eq!(
        host.ids(),
        Vec::<String>::new(),
        "the archive resolves once the creator and its child have left"
    );
    let reached = at_the_record.lock().unwrap().take();
    let still_listed = reached.expect("the point before the child's record was reached");
    assert!(
        still_listed,
        "the child left the listing before its `Archived` entry was appended"
    );
    for path in [&creator, &child] {
        assert!(
            matches!(last_payload(path), Payload::Archived { .. }),
            "{} ends in its record",
            path.display()
        );
    }
    host.swansong().shut_down().await;
}

/// The archived session itself: its watcher runs once its loop has returned, while the end
/// that stopped the loop is still recording. Landing just before the `Archived` entry is
/// appended, it leaves the session listed, and the end resolves once the session has left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archived_session_is_listed_until_its_archive_is_recorded() {
    let roots = Roots::new();
    let (host, creator, _child) = creator_and_child(&roots).await;
    let at_the_record = watcher_lands_before_the_record(&host, "creator");

    host.end("creator").expect("hosted").await;

    assert!(
        host.listed("creator").is_none(),
        "the end resolves once the session has left"
    );
    let reached = at_the_record.lock().unwrap().take();
    let still_listed = reached.expect("the point before the creator's record was reached");
    assert!(
        still_listed,
        "the creator left the listing before its `Archived` entry was appended"
    );
    assert!(matches!(last_payload(&creator), Payload::Archived { .. }));
    host.swansong().shut_down().await;
}

/// A session whose loop fails leaves the map with no end under way: nothing holds its
/// removal back, and nothing records an archive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_whose_loop_fails_leaves_without_a_record() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let path = adopt_logged(&host, &roots, "one", None).await;
    let session = host.session("one").unwrap();
    session.append(1, Payload::user("hi")).unwrap();
    // A closed log refuses the loop's next append, the `User` entry of this turn.
    session.close_log();
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    host.wait_until("the session whose loop failed left the map", || {
        host.listed("one").is_none()
    })
    .await;
    assert!(matches!(last_payload(&path), Payload::User(_)));
    host.swansong().shut_down().await;
}
