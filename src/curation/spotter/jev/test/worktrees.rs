use super::*;
use crate::config::policy::Directories;
use crate::record::log::ToolCall;
use crate::record::names::tools::BASH;

#[test]
fn explicit_worktree_explanation_and_git_c_facts_agree() {
    let role = ConfigFixture::new()
        .with_path(".", true, true)
        .with_path(".efference", false, false)
        .with_path("private", false, false)
        .build();
    for working in ["/work/proj", "/work/proj/.efference/worktrees/own"] {
        let dirs = Directories::new("/work/proj".into(), working.into());
        let call = ToolCall::new(
            BASH,
            "worktree-read",
            json!({"command": "git -C /work/proj/.efference/worktrees/other status; wc -l /work/proj/.efference/worktrees/other/private/key; wc -l /work/proj/.efference/other/log.jsonl"}),
        );
        let rendered = state::state(&role, &call, &Circumstances::default(), Some(&dirs), None);
        assert_eq!(
            rendered["worktrees"],
            crate::curation::spotter::worktree_context(&dirs)
        );
        let paths = rendered["move"]["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 3, "{rendered:?}");
        assert_eq!(paths[0]["resolved"], "/work/proj");
        assert_eq!(paths[0]["under_grant"]["access"], "read, write");
        assert_eq!(paths[1]["resolved"], "/work/proj/private/key");
        assert_eq!(paths[1]["under_grant"]["access"], "denied");
        assert_eq!(
            paths[2]["resolved"],
            "/work/proj/.efference/other/log.jsonl"
        );
        assert_eq!(paths[2]["under_grant"]["access"], "denied");
    }
}

#[test]
fn unknown_directories_do_not_assert_a_worktree_location() {
    let call = ToolCall::new(BASH, "plain", json!({"command": "pwd"}));
    let rendered = state::state(
        &Config::default(),
        &call,
        &Circumstances::default(),
        None,
        None,
    );
    assert!(rendered.get("worktrees").is_none(), "{rendered:?}");
}
