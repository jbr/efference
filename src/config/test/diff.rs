//! Tests for `diff.rs`.

use super::*;

/// New keys the file adds over the effective config surface as additive changes — one per slot,
/// in deterministic (sorted) order; a scope change replaces the scalar.
#[test]
fn diff_emits_additive_changes_for_what_the_file_adds() {
    let from = parse_str("scope = \"old\"\n[commands]\ncargo = true\n");
    let to = parse_str(
        "scope = \"new\"\ntools = [\"read\"]\n\
             [commands]\ncargo = true\n\"git diff\" = true\n[paths]\n\".\" = true\n",
    );
    let changes = diff_changes(&from, &to);
    assert_eq!(
        changes,
        vec![
            ConfigChange::Scope { text: "new".into() },
            ConfigChange::Command {
                command: "git diff".into()
            },
            ConfigChange::Tool {
                name: "read".into()
            },
            ConfigChange::Path {
                path: ".".into(),
                read: true,
                write: true
            },
        ],
    );
}

/// Diffing a config against itself is empty — the property that makes the harness's own
/// auto-persist write-back a harmless no-op echo (the file already matches effective).
#[test]
fn diff_against_self_is_empty() {
    let cfg = parse_str("scope = \"s\"\ntools = [\"read\"]\n[commands]\ncargo = true\n");
    assert!(diff_changes(&cfg, &cfg).is_empty());
}

/// A *changed* path perm surfaces (write added); a removed key does **not** (the additive fold
/// can't express removal — tightening waits for the next boot, which reloads the file as base).
#[test]
fn diff_emits_perm_changes_but_not_removals() {
    let from =
        parse_str("[commands]\ncargo = true\nnpm = true\n[paths]\n\".\" = { read = true }\n");
    let to = parse_str("[commands]\ncargo = true\n[paths]\n\".\" = true\n");
    let changes = diff_changes(&from, &to);
    // `.` gained write — emitted. `npm` was removed — not emitted (additive only).
    assert_eq!(
        changes,
        vec![ConfigChange::Path {
            path: ".".into(),
            read: true,
            write: true
        }],
    );
}
