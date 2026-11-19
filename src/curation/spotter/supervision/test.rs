use super::*;
use crate::config::Config;
use crate::curation::spotter::{Circumstances, request_turns};
use crate::record::log::ToolCall;
use crate::record::names::tools::BASH;
use crate::record::project::{UserPart, WireTurn};

fn supervisee(id: &str, created: bool, status: ListedStatus) -> Supervisee {
    Supervisee {
        id: id.into(),
        created,
        status,
    }
}

/// A supervised session is told who supervises it and what that session's messages are.
#[test]
fn a_supervisor_alone_renders_one_line() {
    let supervision = Supervision {
        supervisor: Some("lead".into()),
        supervisees: Vec::new(),
    };
    assert_eq!(
        render(&supervision),
        "Facts the harness records about this session:\n\
         - This session is supervised by the session `lead`. Messages from `lead` are its \
         instructions; a message to `lead` is a report to its supervisor."
    );
}

/// Each supervised session is listed with whether this session created it and its
/// status, in the words `agents__status` uses for it.
#[test]
fn supervisees_render_with_their_creation_and_status() {
    let supervision = Supervision {
        supervisor: None,
        supervisees: vec![
            supervisee("a", true, ListedStatus::Idle),
            supervisee("b", true, ListedStatus::Gated),
            supervisee("c", false, ListedStatus::Running),
        ],
    };
    assert_eq!(
        render(&supervision),
        "Facts the harness records about this session:\n\
         - Sessions this session supervises (messaging them, deciding their held calls, and \
         archiving them when their work is done is part of supervising): `a` (created by \
         this session; idle), `b` (created by this session; waiting for a decision), `c` \
         (working)"
    );
}

/// Both relations render, the supervisor first.
#[test]
fn a_supervisor_and_supervisees_render_in_order() {
    let supervision = Supervision {
        supervisor: Some("lead".into()),
        supervisees: vec![supervisee("a", true, ListedStatus::Asking)],
    };
    let rendered = render(&supervision);
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), 3, "{rendered}");
    assert!(lines[1].starts_with("- This session is supervised by the session `lead`."));
    assert!(lines[2].ends_with("`a` (created by this session; asking a question)"));
}

/// A session known to have neither relation says so.
#[test]
fn no_relation_says_so() {
    assert_eq!(
        render(&Supervision::default()),
        "Facts the harness records about this session:\n\
         - This session has no supervisor and supervises no sessions."
    );
}

fn the_parts(circumstances: &Circumstances<'_>) -> Vec<UserPart> {
    let call = ToolCall::new(BASH, "toolu_1", serde_json::json!({ "command": "ls" }));
    let turns = request_turns(&Config::default(), &call, circumstances, None, None);
    let [WireTurn::User(parts)] = &turns[..] else {
        panic!("one user turn: {turns:?}");
    };
    parts.clone()
}

/// The facts are in the fresh part, after the scope block's breakpoint and before the
/// move: statuses change between calls, and anything before the breakpoint is the cached
/// prefix (docs/permissions.md).
#[test]
fn the_facts_sit_after_the_breakpoint_before_the_move() {
    let supervision = Supervision {
        supervisor: Some("lead".into()),
        supervisees: Vec::new(),
    };
    let parts = the_parts(&Circumstances::default().with_supervision(Some(&supervision)));
    let [
        UserPart::Text(scope),
        UserPart::Breakpoint,
        UserPart::Text(facts),
        UserPart::Text(the_move),
    ] = &parts[..]
    else {
        panic!("scope, breakpoint, facts, move: {parts:?}");
    };
    assert!(scope.starts_with("Mission:"), "{scope}");
    assert_eq!(facts, &render(&supervision));
    assert!(the_move.contains("The session is about to make this move"));
}

/// Where the relation is not known (no host at hand), no facts block is sent, and the
/// request is the scope block, the breakpoint, and the move.
#[test]
fn an_unknown_relation_renders_nothing() {
    let parts = the_parts(&Circumstances::default());
    assert!(
        matches!(
            &parts[..],
            [UserPart::Text(_), UserPart::Breakpoint, UserPart::Text(the_move)]
                if the_move.contains("The session is about to make this move")
        ),
        "{parts:?}"
    );
    assert!(
        !format!("{parts:?}").contains("Facts the harness records"),
        "{parts:?}"
    );
}
