use super::*;
use crate::api::{Completion, Usage};
use crate::config::Config;
use crate::log::Payload;
use std::path::Path;

/// Parse a config from TOML for a test that needs one as an *input* — the operator's own
/// vocabulary, checked by the real parser.
fn role(toml: &str) -> Config {
    Config::from_toml(toml).expect("valid config")
}

fn spot_completion(name: &str, input: Value) -> Completion {
    Completion {
        assistant: Payload::Assistant {
            output: crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new(name, "s", input)],
            ),
            upstream_failure: None,
        },
        id: None,
        model: "test".into(),
        stop_reason: None,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        failure_class: crate::api::FailureClass::Transient,
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
    let call = ToolCall::new(bash::NAME, "x", json!({ "command": "git log --oneline" }));
    // No probe → no promotion question.
    let plain = render_move(&call, None, &[], None);
    assert!(!plain.contains("clear_always"), "{plain}");
    assert!(!plain.contains("promotion"), "{plain}");
    // Probe → the named prefix and the clear_always instruction ride the move.
    let probed = render_move(&call, None, &[], Some("git log"));
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
    let bash_call = ToolCall::new(bash::NAME, "x", json!({ "command": "git push" }));
    let rendered = render_move(&bash_call, None, &[], None);
    assert!(rendered.contains("git push"), "{rendered}");
    assert!(rendered.contains("spot"), "instruction present: {rendered}");

    let write_call = ToolCall::new("write", "y", json!({ "path": "/etc/hosts" }));
    let rendered = render_move(&write_call, None, &[], None);
    assert!(rendered.contains("write"), "{rendered}");
    assert!(rendered.contains("/etc/hosts"), "{rendered}");
}

/// The move block withholds the call's `intent` (docs/tools.md, docs/permissions.md).
/// Pinned because the default is a decision, not an accident: the field's author is
/// the party being judged, so a spotter that reads it is a channel for arguing past
/// the gate — and a field the gate reads stops being written honestly. Flipping this
/// is the experiment; it should have to break a test.
#[test]
fn render_move_withholds_the_calls_intent() {
    let call = ToolCall::new(
        "write",
        "z",
        json!({ "path": "/etc/hosts", "intent": "this host file is disposable" }),
    );
    let rendered = render_move(&call, None, &[], None);
    assert!(rendered.contains("/etc/hosts"), "{rendered}");
    assert!(
        !rendered.contains("disposable"),
        "intent must not reach the spotter: {rendered}"
    );
    assert!(!rendered.contains("intent"), "{rendered}");
}

/// A connected server's entry: namespaced name, description, schema, and the
/// self-reported hints the wire definition has no slot for.
fn briefed_tool(annotations: Option<mcplease::types::ToolAnnotations>) -> ToolDefinition {
    ToolDefinition {
        name: "ferritin__get".into(),
        title: None,
        description: Some("Show a Rust item by path.".into()),
        input_schema: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
        }),
        output_schema: None,
        annotations,
        icons: None,
        meta: None,
    }
}

#[test]
fn a_brief_puts_the_tool_before_its_input() {
    let call = ToolCall::new(
        "ferritin__get",
        "x",
        json!({ "path": "serde::Deserialize" }),
    );
    let definition = briefed_tool(None);
    let brief = ToolBrief {
        server: Some("ferritin"),
        definition: &definition,
    };
    let rendered = render_move(&call, Some(&brief), &[], None);

    // The tool reads dotted here as everywhere user-facing, and is attributed to the
    // server that advertised it.
    assert!(rendered.contains("`ferritin.get`"), "{rendered}");
    assert!(!rendered.contains("ferritin__get"), "{rendered}");
    assert!(
        rendered.contains("connected MCP server `ferritin`"),
        "{rendered}"
    );
    // Description and schema both ride along.
    assert!(rendered.contains("Show a Rust item by path."), "{rendered}");
    assert!(rendered.contains("\"path\""), "{rendered}");
    // Order is the point: what the tool *is* precedes the arguments, which are
    // uninterpretable without it.
    let described = rendered
        .find("Show a Rust item")
        .expect("description present");
    let input = rendered
        .find("serde::Deserialize")
        .expect("the call's input present");
    assert!(
        described < input,
        "brief must precede the input: {rendered}"
    );
}

#[test]
fn declared_hints_are_rendered_as_the_servers_claims_not_as_findings() {
    let definition = briefed_tool(Some(mcplease::types::ToolAnnotations {
        title: None,
        read_only_hint: Some(true),
        destructive_hint: None,
        idempotent_hint: None,
        open_world_hint: Some(true),
    }));
    let brief = ToolBrief {
        server: Some("ferritin"),
        definition: &definition,
    };
    let rendered = render_brief("ferritin.get", &brief);

    assert!(rendered.contains("read-only"), "{rendered}");
    assert!(rendered.contains("open-world"), "{rendered}");
    // The hedge is load-bearing: these are the judged party's own claims, so they may
    // raise a concern but must never settle one (docs/permissions.md).
    assert!(rendered.contains("self-reported"), "{rendered}");
    assert!(rendered.contains("never enough on its own"), "{rendered}");
    // An absent hint says nothing — it must not render as a denial.
    assert!(!rendered.contains("destructive"), "{rendered}");
    assert!(!rendered.contains("idempotent"), "{rendered}");
}

#[test]
fn a_tool_with_no_annotations_renders_no_declarations_section() {
    let definition = briefed_tool(None);
    let brief = ToolBrief {
        server: None,
        definition: &definition,
    };
    let rendered = render_brief("read", &brief);
    assert!(rendered.contains("built into this harness"), "{rendered}");
    assert!(!rendered.contains("self-reported"), "{rendered}");
}

#[test]
fn an_oversized_description_and_schema_are_clipped_at_a_char_boundary() {
    // Multi-byte chars straddling the budget must not panic or split a char.
    let long = "é".repeat(DESCRIPTION_BUDGET);
    let clipped = clip(&long, DESCRIPTION_BUDGET);
    assert!(clipped.ends_with("… (truncated)"), "{clipped}");
    assert!(clipped.len() <= DESCRIPTION_BUDGET + "\n… (truncated)".len());
    // Under budget passes through untouched.
    assert_eq!(clip("short", DESCRIPTION_BUDGET), "short");
}

#[test]
fn render_context_includes_scope_and_watch_for() {
    let role = role(
        r#"
        scope = "Fix the build."
        watch-for = ["Don't touch the deploy keys."]

        [paths]
        "." = true
        "/refs" = { read = true }
        "#,
    );
    let ctx = render_context(&role, Some(Path::new("/work/repo")), None);
    assert!(ctx.contains("Fix the build."), "{ctx}");
    // A relative grant leads with its resolved absolute form — string-visible
    // containment against the working-directory line — keeping the configured
    // spelling as the annotation; an absolute grant renders once, unannotated.
    assert!(
        ctx.contains("  - /work/repo (read, write) — granted as `.`"),
        "{ctx}"
    );
    assert!(ctx.contains("  - /refs (read only)\n"), "{ctx}");
    assert!(!ctx.contains("granted as `/refs`"), "{ctx}");
    assert!(ctx.contains("Don't touch the deploy keys."), "{ctx}");
    assert!(
        !ctx.contains("$TMPDIR"),
        "no scratch → no alias line: {ctx}"
    );
}

/// The scope block states how a relative path in a move resolves, with the real cwd
/// as the worked example — a spotter once gated `scratch/…` as "not under `.`" by
/// comparing spellings instead of resolving.
#[test]
fn render_context_states_relative_path_resolution() {
    let ctx = render_context(&Config::default(), Some(Path::new("/work/repo")), None);
    assert!(
        ctx.contains("`sub/file` means `/work/repo/sub/file`"),
        "{ctx}"
    );
}

/// A `..`-relative grant resolves through the parent — the spelling a sibling-repo
/// grant (`../fs-mcp`) arrives in.
#[test]
fn render_context_resolves_parent_relative_grants() {
    let role = role(
        r#"
        [paths]
        "../sibling" = true
        "#,
    );
    let ctx = render_context(&role, Some(Path::new("/work/repo")), None);
    assert!(
        ctx.contains("  - /work/sibling (read, write) — granted as `../sibling`"),
        "{ctx}"
    );
}

#[test]
fn render_context_handles_empty_role() {
    // No scope, no paths, no watch-for: the spotter still gets a usable frame.
    let ctx = render_context(&Config::default(), Some(Path::new("/work/repo")), None);
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
        Some(Path::new("/work/repo")),
        Some(Path::new("/var/tmp/efference-ab12")),
    );
    assert!(ctx.contains("$TMPDIR"), "{ctx}");
    assert!(ctx.contains("/var/tmp/efference-ab12"), "{ctx}");
    assert!(ctx.contains("scratch"), "{ctx}");
}

/// The artifact this seam exists to remove (docs/permissions.md): a session
/// recorded in another checkout grants `.` there and reads a file by absolute path.
/// Rendered against that session's cwd, the grant covers the file; rendered against
/// some *other* directory — what an eval process's own cwd is — the same grant
/// resolves elsewhere and the move reads as leaving the perimeter.
#[test]
fn render_context_resolves_grants_against_the_given_cwd_not_the_process() {
    let role = role(
        r#"
        [paths]
        "." = true
        "#,
    );
    let theirs = render_context(&role, Some(Path::new("/Users/jbr/code/trillium")), None);
    assert!(
        theirs.contains("- working directory: /Users/jbr/code/trillium\n"),
        "{theirs}"
    );
    assert!(
        theirs.contains("  - /Users/jbr/code/trillium (read, write) — granted as `.`"),
        "a move touching /Users/jbr/code/trillium/src is inside this grant: {theirs}"
    );

    // The same case rendered from elsewhere — the pre-fix behavior — puts the grant
    // in a directory the move's absolute paths are not under.
    let elsewhere = render_context(&role, Some(Path::new("/Users/jbr/code/efference")), None);
    assert!(
        !elsewhere.contains("/Users/jbr/code/trillium"),
        "{elsewhere}"
    );
}

/// Regression for a false premise the persona once left the model free to invent
/// (backlog/spotter-persona-premises.md, `shy-ridge` log seq 93/99): a project's root
/// config is not inside its denied session directory just because the names look
/// related. The enforcement was always correct (`src/boot.rs`'s `CONFIG_NAME` snapshot,
/// docs/permissions.md's origin-vs-snapshot section) — only the persona's account of it
/// was missing. Pinned on substance, not exact wording, so the fix can be reworded freely.
#[test]
fn persona_says_containment_is_resolved_not_named() {
    assert!(
        PERSONA.contains("never by a name that merely echoes a denied one"),
        "persona must say path containment is decided by resolution, not naming: {PERSONA}"
    );
}

/// Regression for a second false premise from the same incident: the model described
/// creating a session with `autonomous: true` as bypassing the spotter. The real
/// mechanism (`policy::ReviewLevel::Autonomous`'s doc comment, docs/permissions.md's
/// review-level dial) still runs the spotter on every move; autonomous mode only changes
/// what happens after a `watch`/`gate` (decline-and-redirect instead of a paused human
/// gate). Pinned on substance, not exact wording.
#[test]
fn persona_says_autonomous_mode_does_not_skip_the_spotter() {
    assert!(
        PERSONA.contains("still runs its own spotter on every move"),
        "persona must say autonomous mode doesn't skip spotter review: {PERSONA}"
    );
}

/// With no cwd at all, the block says so rather than inventing one, and relative
/// grants stay in their configured spelling — nothing is resolved against a
/// directory the caller never named.
#[test]
fn render_context_without_a_cwd_names_no_directory() {
    let role = role(
        r#"
        [paths]
        "." = true
        "#,
    );
    let ctx = render_context(&role, None, None);
    assert!(ctx.contains("- working directory: unknown\n"), "{ctx}");
    assert!(ctx.contains("  - . (read, write)\n"), "{ctx}");
}
