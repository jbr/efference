//! The context rung: how a scope/context grant reaches the classifier.

use super::*;

/// A role whose `[context]` table has one entry granting `path` with the given write grants.
/// `create` is group-only in the TOML vocabulary (a bare path has nowhere to create into), so a
/// `create` grant forces a labeled group; otherwise a bare path is used, matching what an
/// operator would write for a plain listing.
fn ctx_role(path: &str, update: bool, create: bool) -> Config {
    let entry = match (update, create) {
        (false, false) => format!("\"{path}\" = true"),
        (true, false) => format!("\"{path}\" = {{ update = true }}"),
        (false, true) => format!("Entry = {{ path = \"{path}\", create = true }}"),
        (true, true) => format!("Entry = {{ path = \"{path}\", update = true, create = true }}"),
    };
    role(&format!("[context]\n{entry}\n"))
}

/// The headline behavior: a `[context]` listing is itself the read grant. An otherwise
/// empty role (no `tools`, no `paths`) still fast-path clears a read of the listed file
/// — that read is exactly what the preamble stub invited.
#[test]
fn listing_clears_a_read_with_no_other_grants() {
    let role = ctx_role("docs/verify.md", false, false);
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["docs/verify.md"] }))
        ),
        Verdict::Clear
    );
    // An unlisted path still gates…
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["src/main.rs"] }))
        ),
        Verdict::Gate
    );
    // …and one unlisted target poisons a batch (every path must be granted).
    assert_eq!(
        structured(
            &role,
            &call(
                "read",
                serde_json::json!({ "paths": ["docs/verify.md", "src/main.rs"] })
            )
        ),
        Verdict::Gate
    );
}

/// A location entry (dir) grants read on its whole subtree, by the same lexical rule as
/// `[paths]` — the caveat the backlog states loudly: the listing is a read grant on
/// everything under it, not just a mention.
#[test]
fn location_entry_clears_reads_under_its_subtree() {
    let role = ctx_role("memories", false, false);
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["memories/apis.md"] }))
        ),
        Verdict::Clear
    );
    assert_eq!(
        structured(
            &role,
            &call("list", serde_json::json!({ "path": "memories" }))
        ),
        Verdict::Clear
    );
    // An escape via `..` normalizes out of the subtree and gates.
    assert_eq!(
        structured(
            &role,
            &call(
                "read",
                serde_json::json!({ "paths": ["memories/../secrets.md"] })
            )
        ),
        Verdict::Gate
    );
}

/// The dogfood regression: a batched read spanning **both** clearance sources must clear.
/// One path is covered by a `[paths]` grant (needs the `read` name grant), the other only by
/// a `[context]` entry (standalone). Neither source covers the *whole* batch alone, so the
/// old all-or-nothing-per-source check gated it; per-path union clears it, because every path
/// is granted by *some* source.
#[test]
fn a_read_spanning_paths_and_context_clears_per_path() {
    let role = role(
        r#"
        tools = ["read"]

        [paths]
        "." = true

        [context]
        "/outside/memory" = true
        "#,
    );
    // `docs/x.md` is under the `.` [paths] grant; `/outside/memory/note.md` is under the
    // [context] entry. Each path clears via a *different* source; the batch clears.
    assert_eq!(
        structured(
            &role,
            &call(
                "read",
                serde_json::json!({ "paths": ["docs/x.md", "/outside/memory/note.md"] })
            )
        ),
        Verdict::Clear
    );
    // Order-independence: the same two paths, swapped, still clear.
    assert_eq!(
        structured(
            &role,
            &call(
                "read",
                serde_json::json!({ "paths": ["/outside/memory/note.md", "docs/x.md"] })
            )
        ),
        Verdict::Clear
    );
    // A third path covered by *neither* source still poisons the batch.
    assert_eq!(
        structured(
            &role,
            &call(
                "read",
                serde_json::json!({ "paths": ["docs/x.md", "/outside/memory/note.md", "/etc/passwd"] })
            )
        ),
        Verdict::Gate
    );
}

/// A read-only listing clears no writes; `update` clears a write to an **existing** file
/// under the entry; `create` clears a write to a **new** one. `create` without `update`
/// is the append-only store: add, never rewrite.
#[test]
fn write_grants_split_on_existence() {
    let dir = std::env::temp_dir().join(format!("efference-policy-ctx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("memories")).unwrap();
    std::fs::write(dir.join("memories/existing.md"), "a fact\n").unwrap();

    let existing = call(
        "write",
        serde_json::json!({ "path": "memories/existing.md" }),
    );
    let fresh = call("write", serde_json::json!({ "path": "memories/new.md" }));

    let read_only = ctx_role("memories", false, false);
    assert_eq!(classify_in(&read_only, &existing, &dir), Verdict::Gate);
    assert_eq!(classify_in(&read_only, &fresh, &dir), Verdict::Gate);

    let update_only = ctx_role("memories", true, false);
    assert_eq!(classify_in(&update_only, &existing, &dir), Verdict::Clear);
    assert_eq!(classify_in(&update_only, &fresh, &dir), Verdict::Gate);

    let append_only = ctx_role("memories", false, true);
    assert_eq!(classify_in(&append_only, &existing, &dir), Verdict::Gate);
    assert_eq!(classify_in(&append_only, &fresh, &dir), Verdict::Clear);

    // The edit pipeline: `edit` names the file (enforced like `write`);
    // `undo_edit` names nothing and clears on any context write grant.
    let edit = call(
        "edit",
        serde_json::json!({ "file_path": "memories/existing.md" }),
    );
    let undo = call("undo_edit", serde_json::json!({}));
    assert_eq!(classify_in(&update_only, &edit, &dir), Verdict::Clear);
    assert_eq!(classify_in(&update_only, &undo, &dir), Verdict::Clear);
    assert_eq!(classify_in(&read_only, &undo, &dir), Verdict::Gate);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// An `exec` entry has no files: it grants nothing, read or write.
#[test]
fn exec_entry_grants_nothing() {
    let role = role(
        r#"
        [context]
        Date = { exec = "date" }
        "#,
    );
    assert_eq!(
        structured(
            &role,
            &call("read", serde_json::json!({ "paths": ["date"] }))
        ),
        Verdict::Gate
    );
}

/// The rung never reaches beyond the known path-bearing tools: an unrelated structured
/// tool (or bash) is untouched by `[context]` and follows the ordinary rules.
#[test]
fn unrelated_tools_are_untouched() {
    let role = ctx_role("docs", true, true);
    assert_eq!(
        structured(&role, &call("kill_process", serde_json::json!({ "id": 1 }))),
        Verdict::Gate
    );
    assert_eq!(
        structured(
            &role,
            &call("bash", serde_json::json!({ "command": "cat docs/x.md" }))
        ),
        Verdict::Gate,
        "bash never clears via a context listing — only the structured read path"
    );
}
