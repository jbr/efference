use super::*;

fn scratch(name: &str) -> PathBuf {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let n = RandomState::new().build_hasher().finish();
    let dir = std::env::temp_dir().join(format!("efference-fswatch-{name}-{n:016x}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.canonicalize().expect("canonical scratch")
}

async fn poke_for(
    rx: &mut broadcast::Receiver<FsChange>,
    path: &Path,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(change) if change.path == path => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => panic!("channel closed"),
            }
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_in_a_watched_directory_pokes_subscribers() {
    let dir = scratch("poke");
    let watch = FsWatch::new().expect("watcher");
    let _guard = watch.watch_dir(&dir).expect("watch");
    let mut rx = watch.subscribe();

    let file = dir.join("a.txt");
    std::fs::write(&file, "hello").expect("write");

    poke_for(&mut rx, &file)
        .await
        .expect("a poke for the write");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_and_restored_file_still_pokes() {
    // The parent-directory watch is what keeps a subscription alive across its
    // file not existing — the viewer's mv-away-and-back case.
    let dir = scratch("restore");
    let file = dir.join("b.txt");
    std::fs::write(&file, "first").expect("write");
    let watch = FsWatch::new().expect("watcher");
    let _guard = watch.watch_dir(&dir).expect("watch");
    let mut rx = watch.subscribe();

    std::fs::remove_file(&file).expect("remove");
    poke_for(&mut rx, &file)
        .await
        .expect("a poke for the removal");

    std::fs::write(&file, "second").expect("rewrite");
    poke_for(&mut rx, &file)
        .await
        .expect("a poke for the restore");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_last_guard_unwatches() {
    let dir = scratch("unwatch");
    let watch = FsWatch::new().expect("watcher");
    let first = watch.watch_dir(&dir).expect("watch");
    let second = watch.watch_dir(&dir).expect("watch again");
    drop(first);
    // Still watched: the second guard holds it.
    let mut rx = watch.subscribe();
    let file = dir.join("c.txt");
    std::fs::write(&file, "still watched").expect("write");
    poke_for(&mut rx, &file)
        .await
        .expect("a poke while a guard remains");

    drop(second);
    assert!(watch.state.lock().expect("lock").watched.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
