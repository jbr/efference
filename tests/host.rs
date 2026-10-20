//! `efference --host` against the real binary (docs/operating.md): a standalone
//! host on the socket it is given, taking the web-bind flags beside the mode
//! flag. Nothing here binds TCP: a bind reads the machine root from the secret
//! store, so the flags are shown reaching the host by the refusals it makes
//! before that read.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A scratch HOME holding a user config whose one connection's credential loads.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        // `/tmp` rather than `temp_dir()`: macOS's per-user temp path is long enough
        // to push the socket past the platform's path limit for a Unix socket.
        let root = PathBuf::from(format!("/tmp/eff-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".efference")).unwrap();
        std::fs::write(
            root.join(".efference/config.toml"),
            r#"
[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "EFFERENCE_HOST_TEST_KEY" }
"#,
        )
        .unwrap();
        Self { root }
    }

    fn socket(&self) -> PathBuf {
        self.root.join("host.sock")
    }

    fn command(&self, flags: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_efference"));
        command
            .arg("--host")
            .arg("--socket")
            .arg(self.socket())
            .args(flags)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("EFFERENCE_HOST_TEST_KEY", "not-a-real-credential")
            .stdin(Stdio::null());
        command
    }

    fn refused(&self, flags: &[&str]) -> String {
        let Output { status, stderr, .. } =
            efference::spawn::std_output(&mut self.command(flags)).unwrap();
        assert!(!status.success(), "{flags:?} started a host");
        String::from_utf8_lossy(&stderr).into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn live(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// A standalone host serves its socket with stdin closed — no ownership pipe,
/// unlike an island — and stays up until stopped.
#[test]
fn host_serves_the_socket_it_is_given() {
    let scratch = Scratch::new("serve");
    let mut command = scratch.command(&[]);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = efference::spawn::std(&mut command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !live(&scratch.socket()) {
        assert!(Instant::now() < deadline, "the host never bound its socket");
        assert!(child.try_wait().unwrap().is_none(), "the host exited");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut stream = UnixStream::connect(scratch.socket()).unwrap();
    stream
        .write_all(b"GET /sessions HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        child.try_wait().unwrap().is_none(),
        "a closed stdin ends nothing"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

/// The web-bind flags reach the host from beside `--host`: each is checked at
/// the flag boundary, before anything boots.
#[test]
fn the_web_bind_flags_reach_the_host() {
    let scratch = Scratch::new("bind");
    let open = scratch.refused(&["--open"]);
    assert!(open.contains("--open needs a bind"), "{open}");
    let half = scratch.refused(&["--bind", "127.0.0.1:0", "--tls-cert", "c.pem"]);
    assert!(half.contains("tls-cert and tls-key go together"), "{half}");
    let routable = scratch.refused(&["--bind", "192.0.2.1:7878"]);
    assert!(routable.contains("not loopback"), "{routable}");
    assert!(!live(&scratch.socket()));
}
