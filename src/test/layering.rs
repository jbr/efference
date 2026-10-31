//! Which way imports run between the crate's top-level modules (docs/README.md,
//! *Dependency direction*).
//!
//! Three rows hold. `names` imports no other top-level module. `log` imports none but
//! [`BELOW_LOG`], the modules placed below it. And a module outside [`OUTER`] imports
//! nothing from [`OUTER`], apart from the imports named in [`OPEN_EDGES`]: a new one
//! fails here instead of joining them, and a named one that goes away fails until its
//! line is deleted.
//!
//! The scan is over text, as `daemon/test/statics.rs` is: it skips test files and
//! comments, and reads every spelling of a path that names a module of the crate
//! root — `crate::a::b`, a `use crate::{a, b::c}` group (nested groups and `self`
//! included), and a `super::` chain long enough to reach the root. A `super::`
//! inside an inline `mod` names that module's parent, not the root, so the scan
//! counts the inline modules a path sits in. A string literal is read when the whole
//! literal is a `crate::` path, because attribute paths such as
//! `schema_with = "crate::a::b"` live in strings; any other literal is blanked.
//! Test-only items in a non-test file are scanned like the rest.

use std::path::Path;

mod scan;

use scan::{Reference, references};

/// The modules the rest of the crate may not import from: the ones that host
/// sessions (`daemon`, `boot`), serve the session-control tools (`control_tools`), or
/// are clients of the control channel (`client`, `tui`, `setup`, and the `bin` crate,
/// which the library cannot name).
const OUTER: &[&str] = &[
    "daemon",
    "tui",
    "client",
    "bin",
    "control_tools",
    "setup",
    "boot",
];

/// The modules placed below `log`, which it may import: `names` holds the names a recorded
/// entry carries, and `log`'s own readers match on them.
const BELOW_LOG: &[&str] = &["names"];

/// A set of top-level modules, for the table.
enum Modules {
    /// Exactly these.
    These(&'static [&'static str]),
    /// Every module but these (`Except(&[])` is all of them).
    Except(&'static [&'static str]),
}

impl Modules {
    fn has(&self, module: &str) -> bool {
        match self {
            Modules::These(names) => names.contains(&module),
            Modules::Except(names) => !names.contains(&module),
        }
    }
}

/// One row of the table: a module in `importers` may not import from a module in
/// `imports`, except the `open` edges, each a file under `src/` and the path it
/// names.
struct Row {
    importers: Modules,
    imports: Modules,
    why: &'static str,
    open: &'static [(&'static str, &'static str)],
}

const TABLE: &[Row] = &[
    Row {
        importers: Modules::These(&["names"]),
        imports: Modules::Except(&[]),
        why: "`names` imports no other top-level module",
        open: &[],
    },
    Row {
        importers: Modules::These(&["log"]),
        imports: Modules::Except(BELOW_LOG),
        why: "`log` imports no top-level module but those placed below it",
        open: &[],
    },
    Row {
        importers: Modules::Except(OUTER),
        imports: Modules::These(OUTER),
        why: "a module outside the outer set imports from it",
        open: OPEN_EDGES,
    },
];

/// The imports of an outer module that a module outside the set still makes: the turn
/// loop reads the session from `daemon` and refreshes its config through it. They stay
/// until the session's state leaves `daemon`; each is deleted from here when its import
/// goes.
const OPEN_EDGES: &[(&str, &str)] = &[
    ("agent.rs", "daemon::Session"),
    ("agent/config_edit.rs", "daemon::refresh_and_emit"),
];

/// The row that forbids `importer` importing from `imported`, if one does.
fn row_for(importer: &str, imported: &str) -> Option<&'static Row> {
    if importer == imported {
        return None;
    }
    TABLE
        .iter()
        .find(|row| row.importers.has(importer) && row.imports.has(imported))
}

#[test]
fn imports_point_one_way() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut references = Vec::new();
    visit(&src, &src, &mut references);

    let mut violations = Vec::new();
    let mut named = Vec::new();
    for reference in &references {
        let Some(row) = row_for(reference.importer(), reference.imported()) else {
            continue;
        };
        match row
            .open
            .iter()
            .find(|(file, path)| *file == reference.file && *path == reference.path)
        {
            Some(edge) => named.push(edge),
            None => violations.push(format!(
                "src/{}:{}: imports `{}` — {}",
                reference.file, reference.line, reference.path, row.why
            )),
        }
    }
    assert!(
        violations.is_empty(),
        "imports against the layering (docs/README.md, *Dependency direction*):\n{}",
        violations.join("\n")
    );

    let stale: Vec<String> = OPEN_EDGES
        .iter()
        .filter(|edge| !named.contains(edge))
        .map(|(file, path)| format!("src/{file}: `{path}`"))
        .collect();
    assert!(
        stale.is_empty(),
        "named in OPEN_EDGES and no longer imported — delete the line:\n{}",
        stale.join("\n")
    );
}

/// Every reference in the library's non-test source. `bin` is left out: inside it
/// `crate::` names the binary, and it is the outermost module.
fn visit(root: &Path, dir: &Path, found: &mut Vec<Reference>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            let binary = dir == root && name == "bin";
            if !binary && !matches!(name.as_str(), "test" | "tests") {
                visit(root, &path, found);
            }
        } else if name.ends_with(".rs") && name != "test.rs" && name != "lib.rs" {
            let file = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            found.extend(references(&file, &std::fs::read_to_string(&path).unwrap()));
        }
    }
}

/// The table, row by row.
#[test]
fn the_table_forbids_what_it_names() {
    let forbidden = |importer: &str, imported: &str| row_for(importer, imported).is_some();
    // `names` imports nothing, and `log` imports nothing but `names`.
    assert!(forbidden("names", "log"));
    assert!(forbidden("names", "protocol"));
    assert!(!forbidden("names", "names"));
    assert!(forbidden("log", "api"));
    assert!(forbidden("log", "persist"));
    assert!(!forbidden("log", "log"));
    assert!(!forbidden("log", "names"));
    // Everything above `log` may import `names`, the outer modules included.
    assert!(!forbidden("agent", "names"));
    assert!(!forbidden("session_dir", "names"));
    assert!(!forbidden("daemon", "names"));
    // The inner modules do not import the outer ones; the outer ones may import anything.
    assert!(forbidden("agent", "daemon"));
    assert!(forbidden("session_dir", "boot"));
    assert!(forbidden("protocol", "control_tools"));
    assert!(!forbidden("agent", "log"));
    assert!(!forbidden("daemon", "agent"));
    assert!(!forbidden("tui", "daemon"));
    assert!(!forbidden("boot", "control_tools"));
}
