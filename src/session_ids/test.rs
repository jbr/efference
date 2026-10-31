//! The register's contract: atomic reservation, the resume-side claim, release,
//! and a backfill that reports a collision instead of failing — against a real
//! database in a temporary directory.

use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("efference-session-ids-{nonce:016x}"));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn ids(&self) -> SessionIds {
        SessionIds::at(self.0.join("state.db"))
    }

    /// A root holding a session directory with a log for each id.
    fn root(&self, name: &str, ids: &[&str]) -> PathBuf {
        let root = self.0.join(name);
        for id in ids {
            let dir = root.join(".efference").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(crate::session_dir::LOG_NAME), "{}\n").unwrap();
        }
        root
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_id_held_under_another_root_is_refused() {
    let dir = Dir::new();
    let ids = dir.ids();
    ids.reserve("calm-heron", Path::new("/a")).unwrap();
    let held = ids.reserve("calm-heron", Path::new("/b")).unwrap_err();
    assert_eq!(held.root, Path::new("/a"));
    // Even the holder cannot reserve it again: a creation names a new session.
    assert!(ids.reserve("calm-heron", Path::new("/a")).is_err());
}

#[test]
fn a_claim_accepts_the_holder_and_registers_an_unseen_id() {
    let dir = Dir::new();
    let ids = dir.ids();
    ids.claim("calm-heron", Path::new("/a")).unwrap();
    assert_eq!(ids.holder("calm-heron"), Some(PathBuf::from("/a")));
    ids.claim("calm-heron", Path::new("/a")).unwrap();
    let held = ids.claim("calm-heron", Path::new("/b")).unwrap_err();
    assert_eq!(held.root, Path::new("/a"));
}

#[test]
fn release_frees_the_id_only_for_its_holder() {
    let dir = Dir::new();
    let ids = dir.ids();
    ids.reserve("calm-heron", Path::new("/a")).unwrap();
    ids.release("calm-heron", Path::new("/b"));
    assert!(ids.holder("calm-heron").is_some());
    ids.release("calm-heron", Path::new("/a"));
    ids.reserve("calm-heron", Path::new("/b")).unwrap();
}

#[test]
fn concurrent_reservations_of_one_id_have_one_winner() {
    let dir = Dir::new();
    // Create the table before the race, as a running host has.
    dir.ids().reserve("seed", Path::new("/seed")).unwrap();
    let winners: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let ids = dir.ids();
                scope.spawn(move || {
                    ids.reserve("calm-heron", Path::new(&format!("/root-{n}")))
                        .is_ok()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum()
    });
    assert_eq!(winners, 1);
}

#[test]
fn a_disabled_register_reserves_and_refuses_nothing() {
    let ids = SessionIds::disabled();
    ids.reserve("calm-heron", Path::new("/a")).unwrap();
    ids.reserve("calm-heron", Path::new("/b")).unwrap();
    assert_eq!(ids.holder("calm-heron"), None);
}

#[test]
fn a_backfill_registers_each_root_and_reports_a_shared_id() {
    let dir = Dir::new();
    let ids = dir.ids();
    let one = dir.root("one", &["calm-heron", "amber-lark"]);
    let two = dir.root("two", &["calm-heron", "wild-otter"]);
    // A directory without a log is not a session.
    std::fs::create_dir_all(two.join(".efference/worktrees")).unwrap();

    let collisions = ids.backfill(&[one.clone(), two.clone()]);

    assert_eq!(
        collisions,
        [Collision {
            id: "calm-heron".into(),
            holder: one.clone(),
            other: two.clone(),
        }]
    );
    assert_eq!(ids.holder("calm-heron"), Some(one.clone()));
    assert_eq!(ids.holder("amber-lark"), Some(one.clone()));
    assert_eq!(ids.holder("wild-otter"), Some(two));
    assert_eq!(ids.holder("worktrees"), None);
    // A second pass finds the same collision and changes nothing else.
    let again = ids.backfill(&[one]);
    assert!(again.is_empty(), "the holder's own root is not a collision");
}
