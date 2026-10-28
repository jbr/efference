//! The island host's lifetime, against the real binary (docs/operating.md): the
//! ownership pipe is the only thing that ends it, and ending it leaves nothing
//! behind. The test holds the pipe's write end as a launcher would; the kernel
//! closing it on a launcher's SIGKILL is the same EOF as the drop here.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use efference::daemon::island::{ISLAND_LOG, ISLAND_SOCKET};

/// A scratch HOME holding a user config whose one connection's credential
/// loads, and an island directory to launch into.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        // `/tmp` rather than `temp_dir()`: macOS's per-user temp path is long enough
        // to push the socket past the platform's path limit for a Unix socket.
        let root = PathBuf::from(format!("/tmp/eff-isl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".efference/standalone/1")).unwrap();
        std::fs::write(
            root.join(".efference/config.toml"),
            r#"
[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "EFFERENCE_ISLAND_TEST_KEY" }
"#,
        )
        .unwrap();
        Self { root }
    }

    fn dir(&self) -> PathBuf {
        self.root.join(".efference/standalone/1")
    }

    fn socket(&self) -> PathBuf {
        self.dir().join(ISLAND_SOCKET)
    }

    /// Start `efference --host --island` the way the launcher does, returning the
    /// child and the owner's end of its stdin.
    fn launch(&self) -> (Child, ChildStdin) {
        let log = std::fs::File::create(self.dir().join(ISLAND_LOG)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_efference"));
        command
            .args(["--host", "--island", "--socket"])
            .arg(self.socket())
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("EFFERENCE_ISLAND_TEST_KEY", "not-a-real-credential")
            .stdin(Stdio::piped())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let mut child = efference::spawn::std(&mut command).unwrap();
        let owner = child.stdin.take().unwrap();
        (child, owner)
    }

    fn log(&self) -> String {
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(self.dir().join(ISLAND_LOG)) {
            let _ = file.read_to_string(&mut text);
        }
        text
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn live(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

fn wait_until(what: &str, scratch: &Scratch, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}; host log:\n{}", scratch.log());
}

fn exited(child: &mut Child) -> bool {
    child.try_wait().unwrap().is_some()
}

#[test]
fn closing_the_owner_ends_the_host_and_removes_the_island() {
    let scratch = Scratch::new("close");
    let (mut child, owner) = scratch.launch();
    wait_until("the socket", &scratch, || live(&scratch.socket()));
    drop(owner);
    wait_until("the host to exit", &scratch, || exited(&mut child));
    assert!(child.wait().unwrap().success(), "{}", scratch.log());
    assert!(!scratch.dir().exists(), "the island directory is removed");
}

#[test]
fn an_owner_gone_during_startup_leaves_nothing_running() {
    let scratch = Scratch::new("startup");
    let (mut child, owner) = scratch.launch();
    drop(owner);
    wait_until("the host to exit", &scratch, || exited(&mut child));
    assert!(!live(&scratch.socket()));
    assert!(!scratch.dir().exists(), "the island directory is removed");
}

#[test]
fn another_client_coming_and_going_does_not_end_the_host() {
    let scratch = Scratch::new("observer");
    let (mut child, owner) = scratch.launch();
    wait_until("the socket", &scratch, || live(&scratch.socket()));
    for _ in 0..3 {
        let mut stream = std::os::unix::net::UnixStream::connect(scratch.socket()).unwrap();
        std::io::Write::write_all(
            &mut stream,
            b"GET /sessions HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(!exited(&mut child), "an observer leaving ends nothing");
    assert!(live(&scratch.socket()));
    drop(owner);
    wait_until("the host to exit", &scratch, || exited(&mut child));
}
