//! Structured-tool classification: name grants and filesystem path perms.

use super::*;
use crate::config::testing::ConfigFixture;

#[test]
fn structured_tool_gates_without_a_name_grant() {
    // An empty role grants nothing — every structured tool gates (today's behavior).
    let c = call("read", serde_json::json!({ "paths": ["src/main.rs"] }));
    assert_eq!(structured(&Config::default(), &c), Verdict::Gate);
    // Even with a path grant, an un-granted *name* gates.
    let role = ConfigFixture::new().with_path(".", true, true).build();
    assert_eq!(structured(&role, &c), Verdict::Gate);
}

#[test]
fn read_clears_under_a_read_grant_and_gates_outside_it() {
    let role = fs_role();
    // Under cwd → granted.
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["src/main.rs"] }))
        ),
        Verdict::Clear
    );
    // Absolute path outside any grant → gated.
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["/etc/passwd"] }))
        ),
        Verdict::Gate
    );
    // `..` escaping the cwd grant → gated (lexical normalization catches it).
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["../../etc/passwd"] }))
        ),
        Verdict::Gate
    );
}

#[test]
fn read_with_multiple_paths_needs_all_granted() {
    let role = fs_role();
    // One in-tree, one out-of-tree → the whole call gates.
    let c = call(
        "read",
        serde_json::json!({ "paths": ["src/a.rs", "/etc/hosts"] }),
    );
    assert_eq!(structured(&role, &c), Verdict::Gate);
}

/// `search` names its targets in `paths` (an array), as fs-mcp's schema does: a search
/// outside every grant gates, and only an omitted `paths` defaults to the cwd. Keyed on
/// `path` before, every search read as omitted and cleared wherever `.` was readable.
#[test]
fn search_is_judged_by_its_paths() {
    let role = fs_role();
    let search = |input| structured(&role, &call("search", input));
    assert_eq!(
        search(serde_json::json!({ "pattern": "x", "paths": ["src"] })),
        Verdict::Clear
    );
    assert_eq!(
        search(serde_json::json!({ "pattern": "x", "paths": ["/etc"] })),
        Verdict::Gate
    );
    assert_eq!(
        search(serde_json::json!({ "pattern": "x", "paths": ["src", "/etc"] })),
        Verdict::Gate
    );
    assert_eq!(
        search(serde_json::json!({ "pattern": "x" })),
        Verdict::Clear
    );
}

#[test]
fn list_defaults_to_cwd_when_path_omitted() {
    let role = fs_role();
    assert_eq!(
        structured(&role, &call("list", serde_json::json!({}))),
        Verdict::Clear
    );
}

#[test]
fn write_needs_write_permission_not_just_read() {
    let role = fs_role();
    // cwd is read+write → a write under it clears.
    assert_eq!(
        structured(
            &role,
            &call("write", serde_json::json!({ "path": "src/new.rs" }))
        ),
        Verdict::Clear
    );
    // `/refs` is read-only → a write there gates even though reads clear.
    assert_eq!(
        structured(
            &role,
            &call("write", serde_json::json!({ "path": "/refs/x.rs" }))
        ),
        Verdict::Gate
    );
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["/refs/x.rs"] }))
        ),
        Verdict::Clear
    );
}

#[test]
fn a_nested_deny_carves_an_exclusion_out_of_an_allowed_parent() {
    // The self-surface stack the harness injects in `serve`: allow `.`, deny the whole
    // `.efference/` base, re-allow read-only on *this* session's `config.toml`. Resolution
    // is longest-prefix-wins, so each target is judged by its most specific grant.
    let role = ConfigFixture::new()
        .with_tool("read")
        .with_tool("write")
        .with_path(".", true, true)
        .with_path(".efference", false, false)
        .with_path(".efference/sess/config.toml", true, false)
        .build();
    let read = |p: &str| structured(&role, &call("read", serde_json::json!({ "paths": [p] })));
    let write = |p: &str| structured(&role, &call("write", serde_json::json!({ "path": p })));

    // `.` governs ordinary in-tree work.
    assert_eq!(read("src/main.rs"), Verdict::Clear);
    assert_eq!(write("src/main.rs"), Verdict::Clear);
    // The `.efference/` deny governs the log and the socket — the more specific grant wins
    // over the broader `.` allow, so writing the log or reading it is refused.
    assert_eq!(read(".efference/sess/log.jsonl"), Verdict::Gate);
    assert_eq!(write(".efference/sess/log.jsonl"), Verdict::Gate);
    // This session's own config: readable (the deepest grant re-allows read)…
    assert_eq!(read(".efference/sess/config.toml"), Verdict::Clear);
    // …but not directly writable — the write falls through to the gate, which is the
    // sanctioned scope-expansion path (the operator reviews the proposed edit).
    assert_eq!(write(".efference/sess/config.toml"), Verdict::Gate);
    // Another session's config is under the deny with no re-allow → opaque.
    assert_eq!(read(".efference/other/config.toml"), Verdict::Gate);
}

#[test]
fn edit_pipeline_enforces_write_at_edit_then_undo_clears() {
    let role = fs_role();
    // edit names the file → write enforced here.
    assert_eq!(
        structured(
            &role,
            &call(
                "edit",
                serde_json::json!({ "file_path": "src/x.rs", "operation": "replace", "anchor": "fn x" })
            )
        ),
        Verdict::Clear
    );
    // edit into the read-only zone gates (the write is refused up front).
    assert_eq!(
        structured(
            &role,
            &call(
                "edit",
                serde_json::json!({ "file_path": "/refs/x.rs", "operation": "replace", "anchor": "fn x" })
            )
        ),
        Verdict::Gate
    );
    // undo_edit carries no path; the name grant suffices (write was authorized at edit).
    assert_eq!(
        structured(&role, &call("undo_edit", serde_json::json!({}))),
        Verdict::Clear
    );
}

#[test]
fn daemon_tool_clears_on_name_grant_alone() {
    let role = fs_role();
    // kill_process has no path; being in the tool grants is enough.
    assert_eq!(
        structured(
            &role,
            &call("kill_process", serde_json::json!({ "handle": "bg-1" }))
        ),
        Verdict::Clear
    );
    // An un-granted daemon tool still gates.
    assert_eq!(
        structured(&role, &call("check_inbox", serde_json::json!({}))),
        Verdict::Gate
    );
}

#[test]
fn missing_or_malformed_required_path_gates() {
    let role = fs_role();
    // `read` requires `paths`; absent → gate.
    assert_eq!(
        structured(&role, &call("read", serde_json::json!({}))),
        Verdict::Gate
    );
    // Empty array → gate (a no-target read shouldn't auto-clear).
    assert_eq!(
        structured(&role, &call("read", serde_json::json!({ "paths": [] }))),
        Verdict::Gate
    );
    // Non-string path → gate.
    assert_eq!(
        structured(&role, &call("write", serde_json::json!({ "path": 42 }))),
        Verdict::Gate
    );
}

/// `read_image` follows the same read rule as `read`, keyed on its single
/// `path` input: clears under a granted read subtree, gates outside it, and
/// gates when the required path is absent.
#[test]
fn read_image_clears_like_a_read() {
    let mut role = fs_role();
    role.tools.grant("read_image");
    assert_eq!(
        structured(
            &role,
            &call("read_image", serde_json::json!({ "path": "shot.png" }))
        ),
        Verdict::Clear
    );
    assert_eq!(
        structured(
            &role,
            &call(
                "read_image",
                serde_json::json!({ "path": "/refs/mock.png" })
            )
        ),
        Verdict::Clear
    );
    assert_eq!(
        structured(
            &role,
            &call(
                "read_image",
                serde_json::json!({ "path": "/etc/secret.png" })
            )
        ),
        Verdict::Gate
    );
    assert_eq!(
        structured(&role, &call("read_image", serde_json::json!({}))),
        Verdict::Gate
    );
}

// --- the whole-namespace glob (docs/permissions.md) ---

/// Build a role holding the given grants as the log names them (wire names, a namespace as
/// `<namespace>__*`), applied the way a logged grant is.
fn tool_role(grants: &[&str]) -> Config {
    let mut role = Config::default();
    for name in grants {
        role.tools.grant(name);
    }
    role
}

/// A granted `ferritin` namespace clears every tool that server advertises — the one the mount table held when the
/// grant was written *and* one added later. Nothing enumerates the table, so the two cases are the
/// same lookup: the grant is over the namespace, not over a snapshot of its members.
#[test]
fn a_namespace_glob_clears_every_tool_in_that_namespace() {
    let role = tool_role(&["ferritin__*"]);
    for tool in ["ferritin__get", "ferritin__search", "ferritin__added_later"] {
        assert_eq!(
            structured(&role, &call(tool, serde_json::json!({}))),
            Verdict::Clear,
            "{tool}"
        );
    }
}

/// A namespace grant stops at its own namespace in both directions: a server's reaches neither
/// another server's tools nor a built-in, and `efference`'s reaches no connected tool.
#[test]
fn a_namespace_glob_does_not_cross_the_namespace_boundary() {
    let role = tool_role(&["ferritin__*"]);
    for tool in ["github__create_issue", "read", "list"] {
        assert_eq!(
            structured(&role, &call(tool, serde_json::json!({ "paths": ["a"] }))),
            Verdict::Gate,
            "{tool}"
        );
    }
    // The built-ins' glob is symmetric: it clears no connected tool.
    let builtins = tool_role(&["efference__*"]);
    assert_eq!(
        structured(&builtins, &call("ferritin__get", serde_json::json!({}))),
        Verdict::Gate
    );
}

/// A granted `efference` namespace clears the built-ins, whose wire names carry no `__` prefix
/// at all — the namespace is supplied by [`crate::record::names::tools::BUILTIN_NAMESPACE`]. The path-bearing
/// ones still need their `[paths]` grant: the namespace grant is a name grant, and a built-in's
/// blast radius stays bounded by the second check.
#[test]
fn the_builtin_glob_clears_embedded_tools_within_their_path_grants() {
    let mut role = fs_role();
    role.tools = crate::config::ToolsTable::default();
    role.tools.grant("efference__*");
    // A path-less built-in clears on the glob alone.
    for name in ["kill_process", "read_process", "wait"] {
        assert_eq!(
            structured(&role, &call(name, serde_json::json!({}))),
            Verdict::Clear,
            "{name}"
        );
    }
    // A path-bearing one clears inside a grant…
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["src/main.rs"] }))
        ),
        Verdict::Clear
    );
    // …and gates outside it, glob or no glob.
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["/etc/passwd"] }))
        ),
        Verdict::Gate
    );
}

/// Exact entries keep exact-match semantics beside a namespace grant, and a built-in grant
/// logged in the qualified spelling (`efference__read`) lands on the wire name `read`.
#[test]
fn exact_grants_are_unaffected_by_a_neighbouring_glob() {
    let role = tool_role(&["ferritin__*", "github__create_issue", "efference__read"]);
    assert_eq!(
        structured(&role, &call("github__create_issue", serde_json::json!({}))),
        Verdict::Clear
    );
    // A sibling of the exact entry is not swept in by it.
    assert_eq!(
        structured(&role, &call("github__delete_repo", serde_json::json!({}))),
        Verdict::Gate
    );
    // The qualified built-in spelling resolves to the unprefixed wire name (path grant still
    // required, so this one gates on the path, not the name).
    assert!(tool_granted(&role, "read"));
    assert!(!tool_granted(&role, "write"));
}
