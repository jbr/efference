//! The managed-worktree lifecycle's races (docs/worktrees.md), forced at the seams
//! between a decision and the work that follows it (process/testing.md): two restores
//! of one removed tree, a removal racing a re-activation, a creation admitted during a
//! removal, an archive waiting to leave the map, and a boot cancelled while its
//! checkout is still running.

use super::worktree::{dormant, hosting_worktree, worktree_events};
use super::*;
use crate::record::log::Payload;
use Ordering;

/// Two resumes of one removed tree: at the seam before this one takes the root's
/// lock, the competing restore completes first — and this one finds the checkout in
/// place and records no re-creation (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_restore_of_one_tree_finds_the_first_in_place() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let env = crate::config::env_table::ChildEnv::default();
    let added = crate::host::worktree::add(&roots.0, "quiet-fox", &roots.0, &env).unwrap();
    let nested = added.path.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("lib.rs"), "pub fn f() {}").unwrap();
    crate::host::worktree::testing::git(&added.path, &["add", "src"]);
    crate::host::worktree::testing::commit(&added.path, "src");
    dormant(&roots, "tidy-moss", &nested, true).await;
    crate::host::worktree::retire(&roots.0, "quiet-fox", &env);
    assert!(!added.path.exists());

    let host = Host::new(boot::test::codex_registry(&roots.0, "host"), baseline_env());
    let (project, tree_id, competing_env) = (
        roots.0.clone(),
        "quiet-fox".to_string(),
        crate::config::env_table::ChildEnv::default(),
    );
    *host.before_worktree_restore.lock().unwrap() = Some(Box::new(move || {
        crate::host::worktree::restore(&project, &tree_id, &competing_env).unwrap();
    }));
    let server = host_server(&host).await;
    server
        .patch("/sessions/tidy-moss")
        .with_json_body(&serde_json::json!({"state": "active", "cwd": roots.0}))
        .await
        .assert_status(200);
    host.wait_until("tidy-moss hosted", || host.session("tidy-moss").is_some())
        .await;

    assert!(added.path.is_dir());
    let session = host.session("tidy-moss").unwrap();
    assert!(
        worktree_events(&session).is_empty(),
        "the checkout was the competing restore's: no re-creation recorded here, got {:?}",
        worktree_events(&session)
    );
    host.swansong().shut_down().await;
}

/// The root's lock is held from before the occupancy recount through the removal:
/// the moment an entrant would slip in between them is the moment it cannot
/// (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_root_lock_is_held_across_the_recount_and_the_removal() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let path = hosting_worktree(&roots, &host, "quiet-fox").await;
    host.session("quiet-fox")
        .unwrap()
        .append(1, Payload::User(vec![]))
        .unwrap();

    let held = Arc::new(AtomicBool::new(false));
    let (flag, locks, root) = (Arc::clone(&held), Arc::clone(&host.locks), path.clone());
    *host.before_worktree_removal.lock().unwrap() = Some(Box::new(move || {
        let lock = locks.for_root(&root);
        flag.store(lock.try_lock().is_err(), Ordering::SeqCst);
    }));
    server
        .delete("/sessions/quiet-fox")
        .await
        .assert_status(200);
    assert!(
        held.load(Ordering::SeqCst),
        "the lock is held between the recount and the removal"
    );
    host.swansong().shut_down().await;
}

/// A removal and a re-activation racing: under any interleaving the tree ends up at
/// its root and the re-activated session works in it (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removal_and_a_re_activation_leave_the_tree_restored() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(boot::test::codex_registry(&roots.0, "host"), baseline_env());
    let server = host_server(&host).await;
    let path = hosting_worktree(&roots, &host, "quiet-fox").await;
    host.session("quiet-fox")
        .unwrap()
        .append(1, Payload::User(vec![]))
        .unwrap();
    dormant(&roots, "tidy-moss", &path, true).await;

    let (deleted, resumed) = tokio::join!(
        server.delete("/sessions/quiet-fox"),
        server
            .patch("/sessions/tidy-moss")
            .with_json_body(&serde_json::json!({"state": "active", "cwd": roots.0})),
    );
    deleted.assert_status(200);
    resumed.assert_status(200);
    host.wait_until("tidy-moss hosted", || host.session("tidy-moss").is_some())
        .await;
    assert!(path.is_dir(), "the tree ends up at its root");
    host.swansong().shut_down().await;
}

/// A creation admitted into a tree after the removal's recount, while the removal
/// holds the root's lock: its preparation waits for the removal and then refuses the
/// directory that is gone, instead of starting in a tree being deleted — and no
/// thread of the runtime waits on the lock meanwhile (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creation_admitted_during_a_removal_refuses_the_removed_tree() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(boot::test::codex_registry(&roots.0, "host"), baseline_env());
    let server = host_server(&host).await;
    let path = hosting_worktree(&roots, &host, "quiet-fox").await;
    host.session("quiet-fox")
        .unwrap()
        .append(1, Payload::User(vec![]))
        .unwrap();

    let admitted = Arc::new(Mutex::new(None));
    let (entrant, slot, cwd) = (Arc::clone(&host), Arc::clone(&admitted), path.clone());
    *host.before_worktree_removal.lock().unwrap() = Some(Box::new(move || {
        let body: CreateSession = serde_json::from_value(serde_json::json!({
            "name": "late-wren",
            "cwd": cwd,
            "model": "oai/gpt-5.6",
        }))
        .unwrap();
        let starting = entrant
            .create(body, crate::config::env::Lineage::default())
            .expect("admitted: the tree is still there");
        *slot.lock().unwrap() = Some(starting);
    }));
    server
        .delete("/sessions/quiet-fox")
        .await
        .assert_status(200);
    assert!(
        !path.exists(),
        "the removal went ahead: the entrant came after the count"
    );

    let starting = admitted.lock().unwrap().take().expect("the hook ran");
    let Err(refused) = starting.finished().await else {
        panic!("late-wren started in a removed tree");
    };
    assert!(refused.contains("is gone"), "{refused}");
    host.swansong().shut_down().await;
}

/// An archive that has recorded its session waits for the watcher to take the entry
/// off the map; in that window another occupant's recount must not count it, or two
/// archives in quick succession would each find the other still there and the tree
/// would outlive both (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_waiting_to_leave_the_map_occupies_nothing() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(one_connection_registry(), baseline_env());
    let path = hosting_worktree(&roots, &host, "quiet-fox").await;
    {
        let sessions = host.sessions.lock().unwrap();
        assert!(occupancy::remains(&sessions, &roots.0, &path));
        let Some(Entry::Hosted(hosted)) = sessions.entry("quiet-fox") else {
            panic!("quiet-fox is hosted");
        };
        hosted.departure.mark_archived();
        assert!(
            !occupancy::remains(&sessions, &roots.0, &path),
            "archived and still on the map, it occupies nothing"
        );
    }
    host.swansong().shut_down().await;
}

/// A boot cancelled while its checkout is still running leaves nothing minted: the
/// closure that finished the checkout rolls it back under the root's lock, and the
/// name is free again (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_boot_cancelled_mid_add_leaves_nothing_minted() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let (started, release) = (roots.0.join("hook-started"), roots.0.join("hook-release"));
    let hook = roots.0.join(".git/hooks/post-checkout");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch {}\nwhile [ ! -f {} ]; do sleep 0.05; done\nexit 0\n",
            started.display(),
            release.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let host = Host::new(boot::test::codex_registry(&roots.0, "host"), baseline_env());
    let server = host_server(&host).await;
    let body = serde_json::json!({
        "name": "hooked",
        "cwd": roots.0,
        "model": "oai/gpt-5.6",
        "worktree": true,
    });
    server
        .post("/sessions")
        .with_json_body(&body)
        .await
        .assert_status(201);
    host.poll_until("the checkout reached its hook", || started.exists())
        .await;

    server.delete("/sessions/hooked").await.assert_status(200);
    std::fs::write(&release, "go").unwrap();

    let path = roots.0.join(".efference/worktrees/hooked");
    let env = crate::config::env_table::ChildEnv::default();
    host.poll_until("the cancelled checkout is rolled back", || {
        !path.exists() && !crate::host::worktree::branch_exists(&roots.0, "hooked", &env)
    })
    .await;
    assert!(
        !roots.0.join(".efference/hooked").exists(),
        "its session dir is gone"
    );
    let created = server.post("/sessions").with_json_body(&body).await;
    created.assert_status(201);
    assert_eq!(
        json(created.body())["id"],
        "hooked",
        "the name is free again"
    );
    host.swansong().shut_down().await;
}
