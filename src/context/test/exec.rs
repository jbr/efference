//! Tests for `exec.rs`.

use super::*;

fn exec_entry(label: &str, command: &str) -> ContextEntry {
    ContextEntry {
        heading: Some(label.into()),
        source: ContextSource::Exec(command.into()),
        grants: ContextGrants::default(),
        sort: None,
        exclude: Vec::new(),
        description: None,
    }
}

/// An exec entry is just another row: its stdout becomes a section at its position in
/// document order — a dynamic section sitting *between* static files — collapsed like a
/// labeled single file (the label is the tag, no wrapper).
#[test]
fn resolve_exec_section_lands_in_document_order() {
    let dir = tmpdir("exec-order");
    std::fs::write(dir.join("a.md"), "first\n").unwrap();
    std::fs::write(dir.join("b.md"), "last\n").unwrap();
    let resolved = resolve(
        &[
            bare("a.md"),
            exec_entry("Date", "echo from a command"),
            bare("b.md"),
        ],
        &dir,
    )
    .unwrap();
    assert_eq!(resolved.sections.len(), 3);
    assert_eq!(
        resolved.sections[1],
        Section {
            heading: None,
            description: None,
            items: vec![Item::Exec {
                name: "Date".into(),
                command: "echo from a command".into(),
                body: "from a command\n".into(),
            }],
        }
    );
    let rendered = resolved.render().unwrap();
    let date = rendered.find("<Date>\nfrom a command\n</Date>").unwrap();
    assert!(rendered.find("first").unwrap() < date && date < rendered.find("last").unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The command runs with the session cwd, so relative commands mean what they mean in the
/// bash tool; empty stdout is an empty body, not an error (bytes are bytes).
#[test]
fn resolve_exec_runs_in_base_and_allows_empty_stdout() {
    let dir = tmpdir("exec-cwd");
    std::fs::write(dir.join("marker.txt"), "here\n").unwrap();
    let resolved = resolve(
        &[
            exec_entry("Files", "ls marker.txt"),
            exec_entry("Quiet", "true"),
        ],
        &dir,
    )
    .unwrap();
    assert!(matches!(
        &resolved.sections[0].items[0],
        Item::Exec { body, .. } if body == "marker.txt\n"
    ));
    assert!(matches!(
        &resolved.sections[1].items[0],
        Item::Exec { body, .. } if body.is_empty()
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Fail closed, loudly: a non-zero exit is a config error carrying the command's stderr —
/// never a silently dropped section.
#[test]
fn exec_nonzero_exit_fails_closed_with_stderr() {
    let dir = tmpdir("exec-fail");
    let err = resolve(&[exec_entry("Broken", "echo oops >&2; exit 3")], &dir).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("exit status: 3") && msg.contains("oops"),
        "{msg}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A hung command can't hang boot: past the timeout the process group is killed and the
/// load fails closed.
#[test]
fn exec_timeout_fails_closed() {
    let dir = tmpdir("exec-hang");
    let err = run_exec(
        "sleep 30",
        &dir,
        std::time::Duration::from_millis(100),
        &crate::env_table::StripList::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Binary stdout in a text preamble is a config mistake — fail closed, don't mangle.
#[test]
fn exec_non_utf8_stdout_fails_closed() {
    let dir = tmpdir("exec-binary");
    let err = resolve(&[exec_entry("Binary", r"printf '\xff\xfe'")], &dir).unwrap_err();
    assert!(err.to_string().contains("not valid UTF-8"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}
