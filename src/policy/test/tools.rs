//! Structured-tool classification: name grants and filesystem path perms.

use super::*;

#[test]
fn structured_tool_gates_without_a_name_grant() {
    // An empty role grants nothing — every structured tool gates (today's behavior).
    let c = call("read", serde_json::json!({ "paths": ["src/main.rs"] }));
    assert_eq!(structured(&Config::default(), &c), Verdict::Gate);
    // Even with a path grant, an un-granted *name* gates.
    let role = Config {
        paths: [(
            ".".to_string(),
            Perms {
                read: true,
                write: true,
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
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
    let role = Config {
        tools: ["read", "write"].iter().map(|s| s.to_string()).collect(),
        paths: [
            (
                ".".to_string(),
                Perms {
                    read: true,
                    write: true,
                },
            ),
            (".efference".to_string(), Perms::default()), // deny
            (
                ".efference/sess/config.toml".to_string(),
                Perms {
                    read: true,
                    write: false,
                }, // re-allow, read only
            ),
        ]
        .into_iter()
        .collect(),
        ..Config::default()
    };
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
