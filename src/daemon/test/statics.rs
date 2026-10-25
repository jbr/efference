//! Where the daemon keeps state (docs/control-channel.md, *Where state lives*): a
//! session's on its `Session`, the host's on its `Host`. A mutable `static` under the
//! daemon is state outside both, with no lifecycle of its own: the first artifact
//! build kept every artifact it ever served in one, never freed, and started its
//! listener on whichever runtime asked first.

use std::path::Path;

/// The types that make a `static` mutable state rather than a constant.
const INTERIOR: &[&str] = &[
    "Mutex", "RwLock", "OnceLock", "OnceCell", "LazyLock", "Cell<", "Atomic",
];

#[test]
fn the_daemon_keeps_no_state_in_statics() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    scan(&src.join("daemon.rs"), &mut found);
    visit(&src.join("daemon"), &mut found);
    assert!(
        found.is_empty(),
        "mutable statics in the daemon — keep per-session state on `Session` and \
         host-wide state on `Host` (docs/control-channel.md):\n{}",
        found.join("\n")
    );
}

fn visit(dir: &Path, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            // Tests may hold fixtures however they like.
            if name != "test" {
                visit(&path, found);
            }
        } else if name.ends_with(".rs") && name != "test.rs" {
            scan(&path, found);
        }
    }
}

fn scan(path: &Path, found: &mut Vec<String>) {
    let text = std::fs::read_to_string(path).unwrap();
    for (index, line) in text.lines().enumerate() {
        let code = line.trim_start();
        let code = ["pub(crate) ", "pub(super) ", "pub "]
            .iter()
            .find_map(|visibility| code.strip_prefix(visibility))
            .unwrap_or(code);
        let mutable_static =
            code.starts_with("static ") && INTERIOR.iter().any(|kind| code.contains(kind));
        if mutable_static || code.starts_with("thread_local!") {
            found.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
        }
    }
}
