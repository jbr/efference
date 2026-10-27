//! `efference --host` against the real binary (docs/operating.md): a standalone
//! host on the socket it is given, taking the web-bind flags beside the mode
//! flag. A bind reads the machine root from the secret store, so the flags are
//! shown reaching the host by the refusals it makes before that read; the one
//! test that binds TCP keeps its root with `[secrets]` commands over the scratch
//! directory, so no test reaches the OS keychain.

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

    /// Keep this HOME's secrets with `[secrets]` commands: one file per item under
    /// `items/`, and a missing item exits 3, the declared absent status.
    fn with_secret_commands(self) -> Self {
        let items = self.root.join("items");
        std::fs::create_dir_all(&items).unwrap();
        std::fs::write(
            self.root.join("get.sh"),
            r#"f="$1/$2"; [ -f "$f" ] || exit 3; cat "$f""#,
        )
        .unwrap();
        std::fs::write(self.root.join("set.sh"), r#"cat > "$1/$2""#).unwrap();
        let argv = |script: &str| {
            format!(
                "[\"sh\", {:?}, {:?}, \"{{name}}\"]",
                self.root.join(script).display().to_string(),
                items.display().to_string()
            )
        };
        let mut config = std::fs::OpenOptions::new()
            .append(true)
            .open(self.root.join(".efference/config.toml"))
            .unwrap();
        writeln!(
            config,
            "[secrets]\nget = {}\nset = {}\nabsent_status = 3",
            argv("get.sh"),
            argv("set.sh")
        )
        .unwrap();
        self
    }

    /// `efference <args>` as a verb in this HOME, which must succeed; its stderr.
    fn verb(&self, args: &[&str]) -> String {
        let Output { status, stderr, .. } = efference::spawn::std_output(
            Command::new(env!("CARGO_BIN_EXE_efference"))
                .args(args)
                .current_dir(&self.root)
                .env("HOME", &self.root)
                .stdin(Stdio::null()),
        )
        .unwrap();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        assert!(status.success(), "efference {args:?}: {stderr}");
        stderr
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

/// Through `[secrets]` commands a host never mints the web root: with none stored the
/// bind is refused, naming the verb that makes one, and nothing is written. After
/// `efference auth rotate` has written one with `set`, the same host starts and serves.
#[test]
fn a_bind_through_secret_commands_starts_once_rotate_writes_the_root() {
    let scratch = Scratch::new("secret-commands").with_secret_commands();
    let root_item = scratch.root.join("items/root");

    let refused = scratch.refused(&["--bind", "127.0.0.1:0"]);
    assert!(
        refused.contains("no web root is stored in the `[secrets]` store"),
        "{refused}"
    );
    assert!(
        refused.contains("`efference auth rotate` creates one"),
        "{refused}"
    );
    assert!(!root_item.exists());
    assert!(!live(&scratch.socket()));

    let rotated = scratch.verb(&["auth", "rotate"]);
    assert!(
        rotated.contains("the `[secrets]` store holds a fresh root"),
        "{rotated}"
    );
    let root = std::fs::read_to_string(&root_item).unwrap();
    assert_eq!(root.len(), 43, "{root}");

    let mut command = scratch.command(&["--bind", "127.0.0.1:0"]);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = efference::spawn::std(&mut command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !live(&scratch.socket()) {
        assert!(Instant::now() < deadline, "the host never bound its socket");
        assert!(child.try_wait().unwrap().is_none(), "the host exited");
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(std::fs::read_to_string(&root_item).unwrap(), root);
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

/// A failed start prints the error as text under the program's name, the first thing
/// someone whose setup is wrong reads: its lines on lines of their own, not Rust's
/// quoted `Debug` form with `\n` escapes.
#[test]
fn a_failed_start_prints_its_error_readably() {
    let scratch = Scratch::new("readable");
    let mut command = scratch.command(&[]);
    command.env_remove("EFFERENCE_HOST_TEST_KEY");
    let Output { status, stderr, .. } = efference::spawn::std_output(&mut command).unwrap();
    assert_eq!(status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&stderr);
    // Log lines may precede it; the error is what the output ends with.
    assert!(
        stderr.ends_with(
            "efference: these configured connections cannot authenticate:\n  \
             work: host environment has no EFFERENCE_HOST_TEST_KEY\n\
             run `efference setup` to repair one, or to disable it and start without it\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("Error: \""), "{stderr}");
}
