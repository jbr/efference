use super::*;

/// A corpus line as `extract.py` emits it. The runner reads only the fields it
/// renders; everything else on the line (`source`, `seq`, `recorded`, `note`, …)
/// rides for humans and must not break the parse.
const EXTRACTED: &str = r#"{"id":"1787377108597/40","source":"../trillium/.efference/1787377108597/log.jsonl","seq":40,"cwd":"/Users/jbr/code/trillium","scope":"Develop trillium.","watch_for":[],"paths":{".":{"read":true,"write":true}},"scratch":true,"call":{"name":"bash","input":{"command":"cat /Users/jbr/code/trillium/src/lib.rs"}},"recorded":{"stance":"clear","reason":"in-grant read"}}"#;

/// The session cwd rides the case, so the runner can render a case recorded in
/// another checkout against *that* checkout instead of its own working directory
/// (backlog/spotter-eval.md).
#[test]
fn a_case_carries_its_session_cwd() {
    let case: CaseIn = serde_json::from_str(EXTRACTED).unwrap();
    assert_eq!(case.cwd.as_deref(), Some("/Users/jbr/code/trillium"));
    assert_eq!(case.call.name, "bash");
    assert!(case.scratch);
}

/// Corpus files extracted before the field existed still parse; the missing cwd is
/// `None`, which the runner counts and warns about rather than treating as a cwd.
#[test]
fn a_case_without_a_cwd_parses_as_none() {
    let line =
        r#"{"id":"syn/x","call":{"name":"bash","input":{"command":"ls"}},"expected":"approve"}"#;
    let case: CaseIn = serde_json::from_str(line).unwrap();
    assert!(case.cwd.is_none());
    assert_eq!(case.expected.as_deref(), Some("approve"));
}
