//! Which way imports run between the crate's modules (docs/README.md, *Dependency
//! direction*).
//!
//! The rows of [`table`] hold. `record::names` imports no other module. `record::log`
//! imports none but `record::names`, which is placed below it. Each group imports only the
//! groups before it in [`ORDER`], and its own: `platform`, `record`, `config`, `inference`,
//! `protocol`, `tools`, `curation`, `session`, `agent`, `host`, `routes`, `listeners`,
//! `clients`, `setup`. The binary crate is above them all, and the library cannot name it.
//!
//! A row names a group (`platform`), a module (`agent`) or a module inside a group
//! (`record::log`), and matches by module-path prefix. A group named for one of its modules
//! (`config`, `agent`, `host`) has that module's path. The scan keys a file by its module
//! path from the crate root (`record/log/summary.rs` is `record::log::summary`) and an
//! import by the path it spells.
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

/// The groups, bottom to top: each imports only the groups before it and its own. A path
/// outside all of them is imported by no group.
const ORDER: &[&str] = &[
    "platform",
    "record",
    "config",
    "inference",
    "protocol",
    "tools",
    "curation",
    "session",
    "agent",
    "host",
    "routes",
    "listeners",
    "clients",
    "setup",
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
/// `imports`. A row's `imports` leaves out the importers' own group or module, which they
/// may always import.
struct Row {
    importers: Modules,
    imports: Modules,
    why: String,
}

/// The first row that matches wins, so the rows for a module come before the row for its
/// group: the two modules of `record` that are stricter than it, then one row per group in
/// [`ORDER`].
fn table() -> Vec<Row> {
    let mut rows = vec![
        Row {
            importers: Modules::These(&["record::names"]),
            imports: Modules::Except(&["record::names"]),
            why: "`record::names` imports no other module".to_string(),
        },
        Row {
            importers: Modules::These(&["record::log"]),
            imports: Modules::Except(&["record::log", "record::names"]),
            why: "`record::log` imports no module but `record::names`, which is placed below it"
                .to_string(),
        },
    ];
    for (index, group) in ORDER.iter().enumerate() {
        let below = &ORDER[..index];
        let why = match below {
            [] => format!("{group} imports no other group"),
            [only] => format!("{group} imports only {only}"),
            [rest @ .., last] => format!("{group} imports only {} and {last}", rest.join(", ")),
        };
        rows.push(Row {
            importers: Modules::These(&ORDER[index..=index]),
            imports: Modules::Except(&ORDER[..=index]),
            why,
        });
    }
    rows
}

/// The row that forbids `importer` importing from `imported`, if one does.
fn row_for<'a>(table: &'a [Row], importer: &str, imported: &str) -> Option<&'a Row> {
    table
        .iter()
        .find(|row| row.importers.has(importer) && row.imports.has(imported))
}

#[test]
fn imports_point_one_way() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut references = Vec::new();
    visit(&src, &src, &mut references);

    let table = table();
    let violations: Vec<String> = references
        .iter()
        .filter_map(|reference| {
            let row = row_for(&table, &reference.importer(), reference.imported())?;
            Some(format!(
                "src/{}:{}: imports `{}` — {}",
                reference.file, reference.line, reference.path, row.why
            ))
        })
        .collect();
    assert!(
        violations.is_empty(),
        "imports against the layering (docs/README.md, *Dependency direction*):\n{}",
        violations.join("\n")
    );
}

/// Every top-level module of the library is one of the groups, so the rows cover the whole
/// crate: a new one has to be placed in [`ORDER`] before anything can import it.
#[test]
fn the_crate_root_holds_only_groups() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut unplaced: Vec<String> = std::fs::read_dir(&src)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .map(|name| {
            name.strip_suffix(".rs")
                .map_or(name.clone(), str::to_string)
        })
        .filter(|name| {
            !ORDER.contains(&name.as_str()) && !["lib", "test", "bin"].contains(&name.as_str())
        })
        .collect();
    unplaced.sort();
    unplaced.dedup();
    assert!(
        unplaced.is_empty(),
        "src/ holds modules in no group of ORDER: {}",
        unplaced.join(", ")
    );
    for group in ORDER {
        assert!(
            src.join(format!("{group}.rs")).is_file(),
            "src/{group}.rs is the group's file"
        );
    }
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

/// Whether the table forbids `importer` importing from `imported`.
fn forbidden(importer: &str, imported: &str) -> bool {
    row_for(&table(), importer, imported).is_some()
}

/// The rows for `record::names`, `record::log`, `platform`, `record` and `config`.
#[test]
fn the_rows_for_the_lowest_groups_hold() {
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
    assert!(forbidden("platform::secrets", "host::Host"));
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
}

/// The rows for `inference`, `protocol`, `tools` and `curation`.
#[test]
fn the_rows_for_the_middle_groups_hold() {
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
    assert!(forbidden("protocol", "curation::triage"));
    assert!(!forbidden("protocol::sessions", "inference::api::Error"));
    assert!(!forbidden("protocol::state", "record::log::Payload"));
    assert!(!forbidden("protocol", "protocol::turns"));
    // `tools` imports those and `protocol`, and its own modules.
    assert!(forbidden("tools::bash", "curation::triage"));
    assert!(forbidden("tools::mcp", "agent::frontend::Frontend"));
    assert!(forbidden("tools", "host::Host"));
    assert!(forbidden("tools", "clients::control_tools"));
    assert!(!forbidden("tools::bash", "protocol::state"));
    assert!(!forbidden("tools::mcp", "inference::api::Error"));
    assert!(!forbidden("tools::process_monitor", "config::policy"));
    assert!(!forbidden("tools", "tools::mcp::ConnectedProvider"));
    assert!(!forbidden("tools::toolbox", "tools::bash::Bash"));
    assert!(forbidden("protocol::state", "tools::process_monitor"));
    // `curation` imports those and `tools`, and its own modules.
    assert!(forbidden("curation::triage", "agent::frontend::Frontend"));
    assert!(forbidden("curation::compact", "session::Session"));
    assert!(forbidden("curation", "session::Session"));
    assert!(!forbidden("curation::triage", "tools::process_monitor"));
    assert!(!forbidden("curation::spotter", "config::policy::Verdict"));
    assert!(!forbidden("curation::compact", "curation::digest::digest"));
    assert!(!forbidden("curation::triage", "inference::api::Error"));
    assert!(forbidden("tools::mcp", "curation::triage::is_triage_tool"));
}

/// The rows for `session`, `agent` and `host`.
#[test]
fn the_rows_for_the_session_and_the_loop_hold() {
    // `session` imports those and `curation`, and its own modules.
    assert!(forbidden("session::pace", "agent::frontend::Frontend"));
    assert!(forbidden("session::topics", "host::Host"));
    assert!(forbidden("session", "agent"));
    assert!(!forbidden("session::pace", "tools::Interrupt"));
    assert!(!forbidden("session::push", "platform::push::send"));
    assert!(!forbidden("session::compact", "curation::digest::digest"));
    assert!(!forbidden(
        "session::topic_values",
        "session::status::render"
    ));
    assert!(forbidden("curation::triage", "session::Session"));
    assert!(forbidden("tools::bash", "session::phase"));
    // `agent` imports those and `session`, and its own modules: the loop operates on the
    // session, and the host that starts the loop sits above it.
    assert!(forbidden("agent", "host::Host"));
    assert!(forbidden("agent::step", "routes::views::answer"));
    assert!(forbidden("agent::remote_frontend", "listeners::handler"));
    assert!(!forbidden("agent", "session::Session"));
    assert!(!forbidden("agent", "record::log"));
    assert!(!forbidden("agent", "record::names"));
    assert!(!forbidden("agent", "platform::state"));
    assert!(!forbidden("agent::remote_frontend", "session::Session"));
    assert!(!forbidden("agent", "agent::frontend::Frontend"));
    assert!(!forbidden("agent", "curation::spotter"));
    // `host` imports those and `agent`, and its own modules, `boot` included.
    assert!(forbidden("host::runtime", "routes::http::DecisionBody"));
    assert!(forbidden("host::create", "routes::errors::fail"));
    assert!(forbidden("host::boot", "listeners::tcp"));
    assert!(forbidden("host::boot", "clients::control_tools"));
    assert!(!forbidden("host::runtime", "agent::remote_frontend"));
    assert!(!forbidden("host::runtime", "session::SessionParts"));
    assert!(!forbidden("host::boot::resume", "host::worktree"));
    assert!(!forbidden("host", "host::island"));
}

/// The rows for `routes`, `listeners`, `clients` and `setup`.
#[test]
fn the_rows_for_the_serving_groups_hold() {
    // `routes` imports those and `host`, and its own modules.
    assert!(forbidden("routes::host_http", "listeners::handler"));
    assert!(forbidden("routes::stream", "clients::client"));
    assert!(!forbidden("routes::host_http", "host::Host"));
    assert!(!forbidden("routes::stream", "host::starting::Starting"));
    assert!(!forbidden("routes::views", "routes::entry_view::EntryView"));
    // `listeners` imports those and `routes`, and its own modules.
    assert!(forbidden("listeners::handler", "clients::client"));
    assert!(forbidden("listeners::serve", "setup"));
    assert!(!forbidden(
        "listeners::handler",
        "routes::http::control_routes"
    ));
    assert!(!forbidden("listeners::serve", "host::Host"));
    assert!(!forbidden("listeners::tcp::gate", "listeners::artifacts"));
    // `clients` imports those and `listeners`, and its own modules; `setup` is above them.
    assert!(forbidden("clients::tui", "setup"));
    assert!(forbidden("clients::client", "setup::flow"));
    assert!(!forbidden("clients::control_tools", "host::Host"));
    assert!(!forbidden("clients::client", "protocol::sessions"));
    assert!(!forbidden("clients::tui", "clients::client::Client"));
    assert!(!forbidden("clients::tui", "listeners::tcp"));
    // `setup` imports everything below it.
    assert!(!forbidden("setup", "clients::tui"));
    assert!(!forbidden("setup", "platform::state"));
    assert!(!forbidden("setup::flow", "setup::flow::variable"));
    // No group imports a module outside the groups, and none is named in the rows.
    assert!(forbidden("agent", "unplaced"));
    assert!(forbidden("setup", "bin"));
}
