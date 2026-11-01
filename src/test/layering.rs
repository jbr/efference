//! Which way imports run between the crate's modules (docs/README.md, *Dependency
//! direction*).
//!
//! The rows of [`TABLE`] hold. `record::names` imports no other module. `record::log`
//! imports none but `record::names`, which is placed below it. Each group imports only the
//! groups before it in this order: `platform`, `record`, `config`, `inference`, `protocol`,
//! `tools`, `curation`, `session`. And a module outside [`OUTER`] imports nothing from
//! [`OUTER`], apart from the imports named in [`OPEN_EDGES`]: a new one fails here instead
//! of joining them, and a named one that goes away fails until its line is deleted.
//!
//! A row names a group (`platform`), a module (`agent`) or a module inside a group
//! (`record::log`), and matches by module-path prefix. A group named for one of its modules
//! (`config`) has that module's path. The scan keys a file by its module path from the
//! crate root (`record/log/summary.rs` is `record::log::summary`) and an import by the path
//! it spells.
//!
//! The scan is over text, as `test/statics.rs` is: it skips test files and
//! comments, and reads every spelling of a path that names a module of the crate
//! — `crate::a::b`, a `use crate::{a, b::c}` group (nested groups and `self`
//! included), and a `super::` chain that leaves its file's module, resolved from the
//! file's module path. A `super::` inside an inline `mod` names that module's parent, not
//! the file's, so the scan counts the inline modules a path sits in. A string literal is
//! read when the whole literal is a `crate::` path, because attribute paths such as
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

/// A set of modules, for the table: each is a module path, and contains the modules and
/// items below it.
enum Modules {
    /// Exactly these.
    These(&'static [&'static str]),
    /// Every path but these (`Except(&[])` is all of them).
    Except(&'static [&'static str]),
}

impl Modules {
    /// Whether `path` — a file's module path or an imported path — lies in the set.
    fn has(&self, path: &str) -> bool {
        let within = |name: &&str| {
            path == *name
                || path
                    .strip_prefix(*name)
                    .is_some_and(|rest| rest.starts_with("::"))
        };
        match self {
            Modules::These(names) => names.iter().any(within),
            Modules::Except(names) => !names.iter().any(within),
        }
    }
}

/// One row of the table: a module in `importers` may not import from a module in
/// `imports`, except the `open` edges, each a file under `src/` and the path it
/// names. A row's `imports` leaves out the importers' own group or module, which they
/// may always import.
struct Row {
    importers: Modules,
    imports: Modules,
    why: &'static str,
    open: &'static [(&'static str, &'static str)],
}

/// The first row that matches wins, so a row for a module comes before the row for its
/// group.
const TABLE: &[Row] = &[
    Row {
        importers: Modules::These(&["record::names"]),
        imports: Modules::Except(&["record::names"]),
        why: "`record::names` imports no other module",
        open: &[],
    },
    Row {
        importers: Modules::These(&["record::log"]),
        imports: Modules::Except(&["record::log", "record::names"]),
        why: "`record::log` imports no module but `record::names`, which is placed below it",
        open: &[],
    },
    Row {
        importers: Modules::These(&["platform"]),
        imports: Modules::Except(&["platform"]),
        why: "platform imports no other group",
        open: &[],
    },
    Row {
        importers: Modules::These(&["record"]),
        imports: Modules::Except(&["platform", "record"]),
        why: "record imports only platform",
        open: &[],
    },
    Row {
        importers: Modules::These(&["config"]),
        imports: Modules::Except(&["platform", "record", "config"]),
        why: "config imports only platform and record",
        open: &[],
    },
    Row {
        importers: Modules::These(&["inference"]),
        imports: Modules::Except(&["platform", "record", "config", "inference"]),
        why: "inference imports only platform, record and config",
        open: &[],
    },
    Row {
        importers: Modules::These(&["protocol"]),
        imports: Modules::Except(&["platform", "record", "config", "inference", "protocol"]),
        why: "protocol imports only platform, record, config and inference",
        open: &[],
    },
    Row {
        importers: Modules::These(&["tools"]),
        imports: Modules::Except(&[
            "platform",
            "record",
            "config",
            "inference",
            "protocol",
            "tools",
        ]),
        why: "tools imports only platform, record, config, inference and protocol",
        open: &[],
    },
    Row {
        importers: Modules::These(&["curation"]),
        imports: Modules::Except(&[
            "platform",
            "record",
            "config",
            "inference",
            "protocol",
            "tools",
            "curation",
        ]),
        why: "curation imports only platform, record, config, inference, protocol and tools",
        open: &[],
    },
    Row {
        importers: Modules::These(&["session"]),
        imports: Modules::Except(&[
            "platform",
            "record",
            "config",
            "inference",
            "protocol",
            "tools",
            "curation",
            "session",
        ]),
        why: "session imports only platform, record, config, inference, protocol, tools and curation",
        open: &[],
    },
    Row {
        importers: Modules::Except(OUTER),
        imports: Modules::These(OUTER),
        why: "a module outside the outer set imports from it",
        open: OPEN_EDGES,
    },
];

/// The imports of an outer module that a module outside the set still makes: none now.
/// The turn loop's two (`agent` reading the `Session`, and refreshing its config) ran
/// through `daemon` until the session's state moved to `session`. A new one that must be
/// made against a row is named here by the file that makes it and the path it imports, and
/// is deleted when its import goes: a named one that no longer runs fails the test.
const OPEN_EDGES: &[(&str, &str)] = &[];

/// The row that forbids `importer` importing from `imported`, if one does.
fn row_for(importer: &str, imported: &str) -> Option<&'static Row> {
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
        let Some(row) = row_for(&reference.importer(), reference.imported()) else {
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

/// A name contains the paths below it, not the ones that only start with its letters.
#[test]
fn a_name_matches_whole_path_segments() {
    let record = Modules::These(&["record"]);
    assert!(record.has("record"));
    assert!(record.has("record::log::Payload"));
    assert!(!record.has("records"));
    assert!(!record.has("recording::log"));
    assert!(Modules::Except(&["record"]).has("recording::log"));
}

/// The table, row by row.
#[test]
fn the_table_forbids_what_it_names() {
    let forbidden = |importer: &str, imported: &str| row_for(importer, imported).is_some();
    // `record::names` imports nothing, and `record::log` imports nothing but `record::names`.
    assert!(forbidden("record::names", "record::log"));
    assert!(forbidden("record::names::sources", "record::log::Payload"));
    assert!(forbidden("record::names", "platform::state"));
    assert!(forbidden("record::names", "protocol"));
    assert!(!forbidden("record::names::sources", "record::names::tools"));
    assert!(forbidden("record::log", "inference::api"));
    assert!(forbidden("record::log", "record::persist"));
    assert!(forbidden(
        "record::log::summary",
        "record::project::project"
    ));
    assert!(forbidden("record::log", "platform::state"));
    assert!(!forbidden("record::log", "record::log"));
    assert!(!forbidden("record::log::summary", "record::log::Payload"));
    assert!(!forbidden("record::log", "record::names"));
    assert!(!forbidden("record::log", "record::names::tools::INTENT"));
    // A group imports nothing from a group above it, and may import within itself.
    assert!(forbidden("platform::secrets", "record::log"));
    assert!(forbidden("platform::push", "config::policy"));
    assert!(forbidden("platform::secrets", "daemon::Host"));
    assert!(!forbidden("platform::secrets", "platform::spawn"));
    assert!(!forbidden("platform::push::vapid", "platform::auth::Root"));
    assert!(forbidden("record::persist", "config::policy"));
    assert!(!forbidden("record::persist", "platform::state"));
    assert!(!forbidden("record::persist", "record::log::Payload"));
    assert!(!forbidden(
        "record::project::render",
        "record::names::tools"
    ));
    // `config` imports platform and record, and its own modules.
    assert!(forbidden("config::policy", "inference::registry"));
    assert!(forbidden("config::user_config", "inference::api::Provider"));
    assert!(forbidden("config", "tools::Toolbox"));
    assert!(!forbidden("config::policy", "platform::state"));
    assert!(!forbidden("config::policy", "record::log::Payload"));
    assert!(!forbidden("config::raw", "config::policy::Role"));
    assert!(!forbidden("config", "config::env_table::Table"));
    assert!(forbidden("platform::secrets", "config::user_config"));
    assert!(forbidden("record::persist", "config::env_table::Masker"));
    // `inference` imports platform, record and config, and its own modules; `protocol` those
    // and `inference`.
    assert!(forbidden("inference::api", "protocol::stream"));
    assert!(forbidden("inference::catalog", "tools::Toolbox"));
    assert!(!forbidden(
        "inference::api",
        "config::user_config::Defaults"
    ));
    assert!(!forbidden("inference::api", "record::log::Provider"));
    assert!(!forbidden(
        "inference::registry",
        "inference::api::Endpoint"
    ));
    assert!(!forbidden("inference", "inference::catalog::Catalog"));
    assert!(forbidden("protocol::state", "tools::Toolbox"));
    assert!(forbidden("protocol", "triage"));
    assert!(!forbidden("protocol::sessions", "inference::api::Error"));
    assert!(!forbidden("protocol::state", "record::log::Payload"));
    assert!(!forbidden("protocol", "protocol::turns"));
    // `tools` imports those and `protocol`, and its own modules.
    assert!(forbidden("tools::bash", "curation::triage"));
    assert!(forbidden("tools::mcp", "agent::Frontend"));
    assert!(forbidden("tools", "daemon::Host"));
    assert!(!forbidden("tools::bash", "protocol::state"));
    assert!(!forbidden("tools::mcp", "inference::api::Error"));
    assert!(!forbidden("tools::process_monitor", "config::policy"));
    assert!(!forbidden("tools", "tools::mcp::ConnectedProvider"));
    assert!(!forbidden("tools::toolbox", "tools::bash::Bash"));
    assert!(forbidden("protocol::state", "tools::process_monitor"));
    // `curation` imports those and `tools`, and its own modules.
    assert!(forbidden("curation::triage", "agent::Frontend"));
    assert!(forbidden("curation::compact", "daemon::Session"));
    assert!(forbidden("curation", "session::Session"));
    assert!(!forbidden("curation::triage", "tools::process_monitor"));
    assert!(!forbidden("curation::spotter", "config::policy::Verdict"));
    assert!(!forbidden("curation::compact", "curation::digest::digest"));
    assert!(!forbidden("curation::triage", "inference::api::Error"));
    assert!(forbidden("tools::mcp", "curation::triage::is_triage_tool"));
    // `session` imports those and `curation`, and its own modules.
    assert!(forbidden("session::pace", "agent::Frontend"));
    assert!(forbidden("session::topics", "daemon::Host"));
    assert!(forbidden("session", "frontend::Frontend"));
    assert!(!forbidden("session::pace", "tools::Interrupt"));
    assert!(!forbidden("session::push", "platform::push::send"));
    assert!(!forbidden("session::compact", "curation::digest::digest"));
    assert!(!forbidden(
        "session::topic_values",
        "session::status::render"
    ));
    assert!(forbidden("curation::triage", "session::Session"));
    assert!(forbidden("tools::bash", "session::phase"));
    // Modules outside both groups may import them, the outer modules included.
    assert!(!forbidden("agent", "session::Session"));
    assert!(!forbidden("agent", "record::log"));
    assert!(!forbidden("agent", "record::names"));
    assert!(!forbidden("agent", "platform::state"));
    assert!(!forbidden("daemon", "record::names"));
    assert!(!forbidden("daemon", "platform::state"));
    // The inner modules do not import the outer ones; the outer ones may import anything.
    assert!(forbidden("agent", "daemon"));
    assert!(forbidden("record::session_dir", "boot"));
    assert!(forbidden("protocol", "control_tools"));
    assert!(!forbidden("daemon", "agent"));
    assert!(!forbidden("tui", "daemon"));
    assert!(!forbidden("boot", "control_tools"));
}
