//! The host's update check (docs/update-check.md): what it finds newer than the host
//! reaches every hosted session as the `newer_version` topic, and nothing else does.

use super::*;
use crate::daemon::frames::Topic;
use crate::update_check::Release;
use crate::update_check::fixtures::{NEWER, NEWER_SIG, check};

async fn hosted(label: &str) -> (Arc<Host>, Arc<Session>, PathBuf) {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let root = std::env::temp_dir().join(format!("efference-update-check-{label}-{nonce:016x}"));
    let root = std::fs::create_dir_all(root.join(".efference/one"))
        .and_then(|()| root.canonicalize())
        .unwrap();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let boot = runtime::boot_in(&root, &root.join(".efference/one"));
    host.adopt("one".into(), runtime::started(boot, host.swansong()).await)
        .unwrap();
    let session = host.session("one").unwrap();
    (host, session, root)
}

/// A check that finds a newer release publishes it on a hosted session's stream,
/// with the command that installs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_release_reaches_the_sessions_topic() {
    let (host, session, root) = hosted("newer").await;
    assert_eq!(session.topic_json(Topic::NewerVersion), Value::Null);
    let mut updates = session.subscribe_state();

    host.check_for_updates(
        check(&[
            ("/releases/latest.json", NEWER),
            ("/releases/latest.json.minisig", NEWER_SIG),
        ])
        .installed("0.1.0"),
    );
    let published = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let update = updates.recv().await.unwrap();
            if update.topic == Topic::NewerVersion && &*update.json != "null" {
                break update;
            }
        }
    })
    .await
    .expect("the topic moves");
    assert_eq!(
        serde_json::from_str::<Value>(&published.json).unwrap(),
        serde_json::json!({
            "version": "9.0.0",
            "tag": "v9.0.0",
            "url": "https://efference.net/releases/v9.0.0",
            "update_command": "curl -fsSL https://efference.net/install.sh | sh",
        })
    );
    let state: Value = serde_json::from_str(&session.state_json()).unwrap();
    assert_eq!(state["newer_version"]["version"], "9.0.0");
    host.swansong().shut_down().await;
    let _ = std::fs::remove_dir_all(root);
}

/// A failed check publishes nothing: the topic stays as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_check_publishes_nothing() {
    let (host, session, root) = hosted("failed").await;
    host.check_for_updates(check(&[("/releases/latest.json", NEWER)]).installed("0.1.0"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(session.topic_json(Topic::NewerVersion), Value::Null);
    host.swansong().shut_down().await;
    let _ = std::fs::remove_dir_all(root);
}

/// The value held is the host's, read by every session, and recording none clears
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_is_the_hosts_and_clears() {
    let (host, session, root) = hosted("clears").await;
    let release = Release {
        version: "9.0.0".into(),
        tag: "v9.0.0".into(),
        url: "https://efference.net/releases/v9.0.0".into(),
    };
    host.record_newer_version(Some(release.clone()));
    assert_eq!(session.topic_json(Topic::NewerVersion)["tag"], "v9.0.0");
    host.record_newer_version(None);
    assert_eq!(session.topic_json(Topic::NewerVersion), Value::Null);
    host.swansong().shut_down().await;
    let _ = std::fs::remove_dir_all(root);
}

/// Setting the cell answers whether it moved, which is what spares the sessions a
/// wake when a daily check finds what the last one did.
#[test]
fn the_cell_says_whether_it_moved() {
    let cell = newer_version::NewerVersion::default();
    let release = Release {
        version: "9.0.0".into(),
        tag: "v9.0.0".into(),
        url: String::new(),
    };
    assert!(cell.set(Some(release.clone())));
    assert!(!cell.set(Some(release)));
    assert!(cell.set(None));
    assert!(!cell.set(None));
}
