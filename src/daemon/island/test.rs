use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A host with no connections and no state database: enough to adopt, and to
/// archive nothing on its way out.
fn bare_host() -> Arc<Host> {
    Host::new(
        crate::inference::InferenceRegistry::empty(),
        crate::env_table::EnvSnapshot::new(Default::default()).unwrap(),
    )
}

/// Wait for `host` to start shutting down, failing the test after five seconds.
async fn shuts_down(host: &Host) {
    tokio::time::timeout(Duration::from_secs(5), host.swansong().shutting_down())
        .await
        .expect("the host shut down");
}

/// A pipe whose write end the test holds, as the launcher holds the host's stdin.
fn pipe() -> (std::io::PipeReader, std::io::PipeWriter) {
    std::io::pipe().unwrap()
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn owner_closing_before_the_host_exists_runs_the_early_exit() {
    let (reader, writer) = pipe();
    let exited = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&exited);
    let _watch = OwnerWatch::spawn(reader, move || flag.store(true, Ordering::SeqCst));
    drop(writer);
    wait_for("the early exit", || exited.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_closing_after_adoption_shuts_the_host_down() {
    let (reader, writer) = pipe();
    let watch = OwnerWatch::spawn(reader, || panic!("the host already existed"));
    let host = bare_host();
    watch.adopt(&host);
    assert!(host.is_island(), "adoption makes the host an island");
    assert!(host.swansong().state().is_running());
    drop(writer);
    shuts_down(&host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bytes_from_the_owner_do_not_end_the_host() {
    let (reader, mut writer) = pipe();
    let watch = OwnerWatch::spawn(reader, || panic!("the host already existed"));
    let host = bare_host();
    watch.adopt(&host);
    std::io::Write::write_all(&mut writer, b"anything\n").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(host.swansong().state().is_running());
    drop(writer);
    shuts_down(&host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopting_after_the_owner_went_shuts_down_at_once() {
    let (reader, writer) = pipe();
    let returned = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&returned);
    let watch = OwnerWatch::spawn(reader, move || flag.store(true, Ordering::SeqCst));
    drop(writer);
    wait_for("the early exit", || returned.load(Ordering::SeqCst));
    let host = bare_host();
    watch.adopt(&host);
    shuts_down(&host).await;
}

#[test]
fn island_files_are_removed_by_name_only() {
    let dir = std::env::temp_dir().join(format!("eff-island-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let island = dir.join("7");
    std::fs::create_dir(&island).unwrap();
    let socket = island.join(ISLAND_SOCKET);
    std::fs::write(&socket, "").unwrap();
    std::fs::write(island.join(ISLAND_LOG), "boot").unwrap();
    remove_island_files(&socket);
    assert!(!island.exists());

    // Anything else in the directory keeps it, and is kept.
    std::fs::create_dir(&island).unwrap();
    std::fs::write(&socket, "").unwrap();
    std::fs::write(island.join("other"), "").unwrap();
    remove_island_files(&socket);
    assert!(island.join("other").exists());
    assert!(!socket.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
