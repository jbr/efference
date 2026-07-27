
use super::*;
use crate::log::CallId;
use serde_json::json;

fn tmpdir_expander() -> Expander {
    Expander::new(BTreeMap::from([(
        "TMPDIR".to_string(),
        "/scratch/efference-abc".to_string(),
    )]))
}

#[test]
fn expands_listed_names_in_both_forms() {
    let e = tmpdir_expander();
    assert_eq!(
        e.expand("$TMPDIR/notes.md").unwrap(),
        "/scratch/efference-abc/notes.md"
    );
    assert_eq!(
        e.expand("${TMPDIR}/notes.md").unwrap(),
        "/scratch/efference-abc/notes.md"
    );
    assert_eq!(e.expand("no vars here").unwrap(), "no vars here");
}

#[test]
fn unlisted_name_is_a_hard_error_never_empty() {
    let e = tmpdir_expander();
    let err = e.expand("$TYPO/important").unwrap_err();
    assert!(err.contains("$TYPO"), "names the variable: {err}");
    assert!(err.contains("TMPDIR"), "lists the allowed set: {err}");
    // The braced form errors identically.
    assert!(e.expand("${GOPATH}/src").is_err());
}

/// The pinned escapes: `$$` → literal `$`; a lone `$` not followed by a
/// valid name start stays literal (end-of-string, digits, punctuation).
#[test]
fn escapes_and_lone_dollars_stay_literal() {
    let e = tmpdir_expander();
    assert_eq!(e.expand("a$$b").unwrap(), "a$b");
    assert_eq!(e.expand("price$").unwrap(), "price$");
    assert_eq!(e.expand("a$1b").unwrap(), "a$1b");
    assert_eq!(e.expand("a$-b").unwrap(), "a$-b");
    // `~` is out of scope — literal.
    assert_eq!(e.expand("~/x").unwrap(), "~/x");
}

#[test]
fn shell_default_syntax_and_unclosed_braces_error() {
    let e = tmpdir_expander();
    assert!(e.expand("${UNSET:-42}").is_err(), "no `:-` defaults");
    assert!(e.expand("${TMPDIR").is_err(), "unclosed brace");
    assert!(e.expand("${}").is_err(), "empty name");
}

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: CallId("toolu_x".into()),
        name: name.into(),
        input,
    }
}

/// Only the path field is rewritten — content carrying `$TMPDIR` as *text*
/// lands byte-identical.
#[test]
fn expand_call_rewrites_paths_never_content() {
    let e = tmpdir_expander();
    let c = call(
        "write",
        json!({ "path": "$TMPDIR/notes.md", "content": "uses $TMPDIR and $TYPO" }),
    );
    let out = expand_call(&e, &c).unwrap().expect("rewritten");
    assert_eq!(out.input["path"], json!("/scratch/efference-abc/notes.md"));
    assert_eq!(out.input["content"], json!("uses $TMPDIR and $TYPO"));
}

#[test]
fn expand_call_handles_arrays_and_leaves_plain_calls_untouched() {
    let e = tmpdir_expander();
    let c = call("read", json!({ "paths": ["$TMPDIR/a.md", "src/lib.rs"] }));
    let out = expand_call(&e, &c).unwrap().expect("rewritten");
    assert_eq!(
        out.input["paths"],
        json!(["/scratch/efference-abc/a.md", "src/lib.rs"])
    );
    // No `$` anywhere → untouched (no clone).
    assert!(
        expand_call(&e, &call("read", json!({ "paths": ["src/lib.rs"] })))
            .unwrap()
            .is_none()
    );
    // Bash is exempt — the shell expands in-child.
    assert!(
        expand_call(&e, &call("bash", json!({ "command": "echo $TYPO" })))
            .unwrap()
            .is_none()
    );
}

#[test]
fn expand_call_surfaces_the_error_for_a_bad_path() {
    let e = tmpdir_expander();
    let c = call(
        "write",
        json!({ "path": "$HOME/.ssh/config", "content": "x" }),
    );
    let err = expand_call(&e, &c).unwrap_err();
    assert!(err.contains("$HOME"), "{err}");
}
