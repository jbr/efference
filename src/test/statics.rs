//! Where the daemon keeps state (docs/control-channel.md, *Where state lives*): a
//! session's on its `Session`, the host's on its `Host`. A mutable `static` under the
//! daemon is state outside both, with no lifecycle of its own: the first artifact
//! build kept every artifact it ever served in one, never freed, and started its
//! listener on whichever runtime asked first.
//!
//! The daemon's code is the groups that hold sessions and serve them, each a file
//! `<name>.rs` and a directory `<name>/` under `src/`; [`ROOTS`] names them. A task-local
//! (`tokio::task_local!`) is not read as a static: its value lives as long as the scope
//! that sets it.

use std::path::Path;

/// The groups whose code is the daemon's: a session's state and what drives it (`session`,
/// `agent`, which holds the loop's frontend over a session), the host and its boot (`host`),
/// the routes it answers (`routes`) and the listeners that mount them (`listeners`).
const ROOTS: &[&str] = &["session", "agent", "host", "routes", "listeners"];

/// The types that make a `static` mutable state rather than a constant.
const INTERIOR: &[&str] = &[
    "Mutex", "RwLock", "OnceLock", "OnceCell", "LazyLock", "Cell<", "Atomic",
];

#[test]
fn the_daemon_keeps_no_state_in_statics() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for root in ROOTS {
        scan(&src.join(format!("{root}.rs")), &mut found);
        visit(&src.join(root), &mut found);
    }
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
    for (line, code) in mutable_statics(&text) {
        found.push(format!("{}:{}: {}", path.display(), line, code));
    }
}

/// The lines of `text` that declare a mutable static: its number from 1, and the line.
fn mutable_statics(text: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut task_local = false;
    for (index, line) in text.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("tokio::task_local!") || code.starts_with("task_local!") {
            task_local = true;
        }
        if task_local {
            // The macro's body runs to the closing brace at its own indent.
            if code.starts_with('}') {
                task_local = false;
            }
            continue;
        }
        let declared = ["pub(crate) ", "pub(super) ", "pub "]
            .iter()
            .find_map(|visibility| code.strip_prefix(visibility))
            .unwrap_or(code);
        let mutable_static =
            declared.starts_with("static ") && INTERIOR.iter().any(|kind| declared.contains(kind));
        if mutable_static || declared.starts_with("thread_local!") {
            found.push((index + 1, line.trim()));
        }
    }
    found
}

#[test]
fn a_task_local_is_scoped_state_and_a_static_is_not() {
    let text = "\
static CACHE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
pub(crate) static ONCE: OnceLock<u8> = OnceLock::new();
thread_local! { static N: Cell<u8> = Cell::new(0); }
static LIMIT: usize = 4;
#[cfg(test)]
tokio::task_local! {
    /// Scratch directories one test made.
    pub(super) static MINTED: std::cell::RefCell<Vec<u8>>;
}
static AFTER: AtomicBool = AtomicBool::new(false);
";
    let lines: Vec<usize> = mutable_statics(text)
        .iter()
        .map(|(line, _)| *line)
        .collect();
    assert_eq!(lines, [1, 2, 3, 10]);
}
