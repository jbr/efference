use super::*;
use crate::api::{Completion, Usage};
use crate::config::{Config, Perms};
use crate::log::{CallId, Payload};

fn spot_completion(name: &str, input: Value) -> Completion {
    Completion {
        assistant: Payload::Assistant {
            thinking: vec![],
            text: vec![],
            calls: vec![ToolCall {
                id: CallId("s".into()),
                name: name.into(),
                input,
            }],
        },
        model: "test".into(),
        stop_reason: None,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    }
}

#[test]
fn parse_stance_is_lenient_and_fails_closed() {
    assert_eq!(parse_stance("clear"), Stance::Clear);
    assert_eq!(parse_stance("  WATCH "), Stance::Watch);
    assert_eq!(parse_stance("Gate"), Stance::Gate);
    // The promotion safe-vote, in its accepted spellings.
    assert_eq!(parse_stance("clear_always"), Stance::ClearAlways);
    assert_eq!(parse_stance("Clear Always"), Stance::ClearAlways);
    assert_eq!(parse_stance("always"), Stance::ClearAlways);
    // Anything unrecognized routes to the human, not to auto-clear.
    assert_eq!(parse_stance("yolo"), Stance::Gate);
    assert_eq!(parse_stance(""), Stance::Gate);
}

#[test]
fn parse_spot_reads_stance_and_reason() {
    let read = parse_spot(&spot_completion(
        SPOT,
        json!({ "stance": "clear", "reason": "in-scope build, reversible" }),
    ))
    .expect("a spot call parses");
    assert_eq!(read.stance, Stance::Clear);
    assert_eq!(read.reason, "in-scope build, reversible");
}

#[test]
fn parse_spot_reads_the_promotion_safe_vote() {
    let read = parse_spot(&spot_completion(
        SPOT,
        json!({ "stance": "clear_always", "reason": "every cargo subcommand is reversible" }),
    ))
    .expect("a spot call parses");
    assert_eq!(read.stance, Stance::ClearAlways);
}

#[test]
fn render_move_asks_the_promotion_question_only_when_a_prefix_is_probed() {
    let call = ToolCall {
        id: CallId("x".into()),
        name: bash::NAME.into(),
        input: json!({ "command": "git log --oneline" }),
    };
    // No probe → no promotion question.
    let plain = render_move(&call, None);
    assert!(!plain.contains("clear_always"), "{plain}");
    assert!(!plain.contains("promotion"), "{plain}");
    // Probe → the named prefix and the clear_always instruction ride the move.
    let probed = render_move(&call, Some("git log"));
    assert!(probed.contains("git log"), "{probed}");
    assert!(probed.contains("clear_always"), "{probed}");
}

#[test]
fn parse_spot_is_none_without_a_spot_call() {
    // A turn that called something else yields no verdict → caller fails closed.
    assert!(parse_spot(&spot_completion("bash", json!({ "command": "ls" }))).is_none());
    // Missing required field → no verdict.
    assert!(parse_spot(&spot_completion(SPOT, json!({ "stance": "clear" }))).is_none());
}

#[test]
fn render_move_shows_bash_command_and_structured_input() {
    let bash_call = ToolCall {
        id: CallId("x".into()),
        name: bash::NAME.into(),
        input: json!({ "command": "git push" }),
    };
    let rendered = render_move(&bash_call, None);
    assert!(rendered.contains("git push"), "{rendered}");
    assert!(rendered.contains("spot"), "instruction present: {rendered}");

    let write_call = ToolCall {
        id: CallId("y".into()),
        name: "write".into(),
        input: json!({ "path": "/etc/hosts" }),
    };
    let rendered = render_move(&write_call, None);
    assert!(rendered.contains("write"), "{rendered}");
    assert!(rendered.contains("/etc/hosts"), "{rendered}");
}

#[test]
fn render_context_includes_scope_and_watch_for() {
    let role = Config {
        scope: Some("Fix the build.".into()),
        watch_for: vec!["Don't touch the deploy keys.".into()],
        paths: [
            (
                ".".to_string(),
                Perms {
                    read: true,
                    write: true,
                },
            ),
            (
                "/refs".to_string(),
                Perms {
                    read: true,
                    write: false,
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let ctx = render_context(&role, None);
    assert!(ctx.contains("Fix the build."), "{ctx}");
    assert!(ctx.contains(". (read, write)"), "{ctx}");
    assert!(ctx.contains("/refs (read only)"), "{ctx}");
    assert!(ctx.contains("Don't touch the deploy keys."), "{ctx}");
    assert!(
        !ctx.contains("$TMPDIR"),
        "no scratch → no alias line: {ctx}"
    );
}

#[test]
fn render_context_handles_empty_role() {
    // No scope, no paths, no watch-for: the spotter still gets a usable frame.
    let ctx = render_context(&Config::default(), None);
    assert!(ctx.contains("no scope specified"), "{ctx}");
    assert!(ctx.contains("none configured"), "{ctx}");
    assert!(
        !ctx.contains("Things to watch for"),
        "no empty watch-for section: {ctx}"
    );
}

/// The scratch alias (docs/tools.md): with a scratch dir, the scope block
/// names `$TMPDIR` and the literal path it points at — what lets the spotter
/// connect a bash `$TMPDIR/…` write to the granted directory.
#[test]
fn render_context_names_the_scratch_alias() {
    let ctx = render_context(
        &Config::default(),
        Some(std::path::Path::new("/var/tmp/efference-ab12")),
    );
    assert!(ctx.contains("$TMPDIR"), "{ctx}");
    assert!(ctx.contains("/var/tmp/efference-ab12"), "{ctx}");
    assert!(ctx.contains("scratch"), "{ctx}");
}
