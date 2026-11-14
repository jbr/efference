use super::*;
use crate::config::policy::Directories;

#[test]
fn explicit_worktree_explanation_and_git_c_facts_agree() {
    let role = ConfigFixture::new()
        .with_path(".", true, true)
        .with_path(".efference", false, false)
        .with_path("private", false, false)
        .build();
    for working in ["/work/proj", "/work/proj/.efference/worktrees/own"] {
        let dirs = Directories::new("/work/proj".into(), working.into());
        let ctx = render_context(&role, Some(&dirs), None);
        assert!(ctx.contains(&worktree_context(&dirs)), "{ctx}");
        assert!(ctx.contains("including another session's"), "{ctx}");
        assert!(
            ctx.contains("are not mapped and retain their own grants"),
            "{ctx}"
        );

        let call = ToolCall::new(
            BASH,
            "worktree-read",
            json!({"command": "git -C /work/proj/.efference/worktrees/other status; wc -l /work/proj/.efference/worktrees/other/private/key; wc -l /work/proj/.efference/other/log.jsonl"}),
        );
        let rendered = render_move(&role, &call, None, &[], None, Some(&dirs), None);
        assert!(rendered.contains("`/work/proj/.efference/worktrees/other` → `/work/proj`: under the grant `/work/proj` (read, write)"), "{rendered}");
        assert!(
            rendered.contains(
                "`/work/proj/private/key`: under the grant `/work/proj/private` (no access"
            ),
            "{rendered}"
        );
        assert!(rendered.contains("`/work/proj/.efference/other/log.jsonl`: under the grant `/work/proj/.efference` (no access"), "{rendered}");
    }
}

#[test]
fn unknown_directories_do_not_assert_a_worktree_location() {
    let ctx = render_context(&Config::default(), None, None);
    assert!(!ctx.contains("Managed git worktrees"), "{ctx}");
}
