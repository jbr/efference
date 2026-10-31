use super::sources::*;
use super::tools::*;

/// A session's source names its project only when it is another than the
/// recipient's; every reader takes the id from either form, and the wire label
/// names the project when there is one.
#[test]
fn a_session_source_names_another_project() {
    let here = session_source("calm-finch", None);
    let there = session_source("calm-finch", Some("~/code/mcplease"));
    assert_eq!(here, "session[calm-finch]");
    assert_eq!(there, "session[calm-finch in ~/code/mcplease]");

    for source in [&here, &there] {
        assert_eq!(source_session(source), Some("calm-finch"));
        assert_eq!(source_name(source), "calm-finch");
        assert!(agent_source(Some(source)));
    }
    assert_eq!(wire_label(&here), "calm-finch (agent)");
    assert_eq!(wire_label(&there), "calm-finch (agent in ~/code/mcplease)");

    assert_eq!(source_name("mcp[claude-code]"), "claude-code");
    assert_eq!(source_name("agent[parent]"), "parent");
    assert_eq!(source_name("calm-finch"), "calm-finch");
    assert_eq!(wire_label("calm-finch"), "calm-finch");
}

/// The tool names are on the wire and in logs already written, and the namespace
/// spellings are keys in config files: changing a value here changes all of them.
#[test]
fn the_tool_names_keep_their_spellings() {
    assert_eq!(
        [
            INTENT,
            ASK,
            BASH,
            CONSOLIDATE,
            LIST_PROCESSES,
            READ_PROCESS,
            KILL_PROCESS,
            WATCH_PROCESS,
            CHECK_INBOX,
            WAIT,
            RETAIN,
            TRIAGE,
            NAMESPACE_SEPARATOR,
            BUILTIN_NAMESPACE,
            AGENTS_NAMESPACE,
            LSP_NAMESPACE,
        ],
        [
            "intent",
            "ask",
            "bash",
            "consolidate",
            "list_processes",
            "read_process",
            "kill_process",
            "watch_process",
            "check_inbox",
            "wait",
            "retain",
            "triage",
            "__",
            "efference",
            "agents",
            "lsp",
        ]
    );
    assert!(is_triage_tool("retain") && is_triage_tool("triage"));
    assert!(!is_triage_tool("read") && !is_triage_tool("consolidate"));
}

#[test]
fn display_name_dots_the_namespace_boundary_only() {
    // The common case: what the operator reads instead of `ferritin__get`.
    assert_eq!(display_name("ferritin__get"), "ferritin.get");
    // An embedded tool name has no boundary to find, so it passes through
    // unchanged — the same invariant that makes namespacing collision-free.
    assert_eq!(display_name("edit"), "edit");
    assert_eq!(display_name("undo_edit"), "undo_edit");
    // Only the *first* `__` is the boundary. A server whose own tool name
    // contains `__` keeps it: `get__raw` is that server's name for the tool,
    // not a second namespace.
    assert_eq!(display_name("ferritin__get__raw"), "ferritin.get__raw");
}

/// The boundary is the first `__` for the namespace as it is for the display name; a
/// name without one is an embedded tool's, in the built-in namespace.
#[test]
fn a_namespace_is_the_part_before_the_first_separator() {
    assert_eq!(namespace_of("ferritin__get"), "ferritin");
    assert_eq!(namespace_of("ferritin__get__raw"), "ferritin");
    assert_eq!(namespace_of("read"), BUILTIN_NAMESPACE);
}

/// A whole-namespace grant is `<namespace>__*`; anything else, a star inside the
/// namespace or a second separator included, names a tool or nothing.
#[test]
fn a_namespace_glob_round_trips_only_for_a_plain_namespace() {
    assert_eq!(namespace_glob("ferritin"), "ferritin__*");
    assert!(is_namespace_glob("ferritin__*"));
    assert!(is_namespace_glob(&namespace_glob("agents")));
    assert!(!is_namespace_glob("__*"));
    assert!(!is_namespace_glob("ferritin__get"));
    assert!(!is_namespace_glob("fer*tin__*"));
    assert!(!is_namespace_glob("a__b__*"));
}
