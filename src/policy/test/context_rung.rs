//! The context rung: how a scope/context grant reaches the classifier.

use super::*;
use crate::config::{ContextEntry, ContextGrants, ContextSource};

fn ctx_entry(paths: &[&str], update: bool, create: bool) -> ContextEntry {
    ContextEntry {
        heading: None,
        source: ContextSource::Files(paths.iter().map(|s| s.to_string()).collect()),
        grants: ContextGrants { update, create },
        sort: None,
        exclude: Vec::new(),
        description: None,
    }
}

fn ctx_role(entries: Vec<ContextEntry>) -> Config {
    Config {
        context: entries,
        ..Default::default()
    }
}

/// The headline behavior: a `[context]` listing is itself the read grant. An otherwise
/// empty role (no `tools`, no `paths`) still fast-path clears a read of the listed file
/// — that read is exactly what the preamble stub invited.
#[test]
fn listing_clears_a_read_with_no_other_grants() {
    let role = ctx_role(vec![ctx_entry(&["docs/verify.md"], false, false)]);
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
    let role = ctx_role(vec![ctx_entry(&["memories"], false, false)]);
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
    let role = Config {
        tools: ["read"].iter().map(|s| s.to_string()).collect(),
        paths: [(
            ".".to_string(),
            Perms {
                read: true,
                write: true,
            },
        )]
        .into_iter()
        .collect(),
        context: vec![ctx_entry(&["/outside/memory"], false, false)],
        ..Default::default()
    };
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

    let read_only = ctx_role(vec![ctx_entry(&["memories"], false, false)]);
    assert_eq!(classify_in(&read_only, &existing, &dir), Verdict::Gate);
    assert_eq!(classify_in(&read_only, &fresh, &dir), Verdict::Gate);

    let update_only = ctx_role(vec![ctx_entry(&["memories"], true, false)]);
    assert_eq!(classify_in(&update_only, &existing, &dir), Verdict::Clear);
    assert_eq!(classify_in(&update_only, &fresh, &dir), Verdict::Gate);

    let append_only = ctx_role(vec![ctx_entry(&["memories"], false, true)]);
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
    let role = ctx_role(vec![ContextEntry {
        heading: Some("Date".into()),
        source: ContextSource::Exec("date".into()),
        grants: ContextGrants::default(),
        sort: None,
        exclude: Vec::new(),
        description: None,
    }]);
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
    let role = ctx_role(vec![ctx_entry(&["docs"], true, true)]);
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
