//! The verbs' answers, pinned as exact text.

use super::*;
use crate::session::status::Moves;

const NOW: u64 = 1_754_300_000_000;
const SECOND: u64 = 1000;

fn view(condition: Condition) -> StatusView {
    StatusView {
        id: "child".into(),
        condition,
        footer: Footer::default(),
    }
}

fn held() -> HeldCall {
    HeldCall {
        call_id: "toolu_1".into(),
        tool: "bash".into(),
        summary: Some("curl -sI https://example.com".into()),
        intent: None,
        handle: Some("8.2".into()),
        held_because: None,
        reviewer: Reviewer::Operator,
        escalations: Vec::new(),
    }
}

fn working(activity: Activity) -> StatusView {
    view(Condition::Working {
        activity: vec![activity],
    })
}

fn bash_for(seconds: u64) -> Activity {
    Activity::Tool {
        tool: "bash".into(),
        command: Some("cargo test".into()),
        since: NOW - seconds * SECOND,
    }
}

/// A driver's message says whether it landed now, waits behind a step or a held call,
/// or answered the question it named.
#[test]
fn what_a_message_did() {
    assert_eq!(
        sent("child", Landing::Now, None, NOW),
        "delivered: child is working on it"
    );
    assert_eq!(
        sent("child", Landing::Queued, Some(&working(bash_for(40))), NOW),
        "queued: child reads this after its current step (running bash `cargo test` for 40s)"
    );
    assert_eq!(
        sent(
            "child",
            Landing::Queued,
            Some(&view(Condition::Gated(held()))),
            NOW
        ),
        "queued: child is waiting for a decision on 8.2 (bash `curl -sI https://example.com`); \
         it reads this after the decision"
    );
    let asking = view(Condition::Asking {
        question: "Delete the old migrations?".into(),
        n: 1,
        of: 1,
        handle: Some("3.1".into()),
        returned: vec![],
    });
    assert_eq!(
        sent("child", Landing::Answered, Some(&asking), NOW),
        "answered: child has your answer to 3.1 \"Delete the old migrations?\" and goes on"
    );
    assert_eq!(
        left_in_inbox("child"),
        "left in child's inbox; it may set it aside (you don't drive it)"
    );
}

/// Each decision names what it did to the held call.
#[test]
fn what_a_decision_did() {
    assert_eq!(
        decided("child", &held(), true, None),
        "allowed: child runs bash `curl -sI https://example.com`"
    );
    assert_eq!(
        decided("child", &held(), true, Some("use read next time")),
        "allowed with your note: child runs bash `curl -sI https://example.com`; your note \
         arrives with its result"
    );
    assert_eq!(
        decided("child", &held(), false, Some("try the cached copy")),
        "denied: child continues under your message"
    );
    assert_eq!(
        decided("child", &held(), false, None),
        "denied: child's turn stopped"
    );
    let reader = Reader::read(Relation::Reviewer, &Moves::MCP, None);
    assert_eq!(
        gate_moved("7", &view(Condition::Gated(held())), &reader, NOW),
        "gate moved: 7 is not waiting for a decision; now child: waiting for your decision on \
         bash `curl -sI https://example.com` (seq 8.2)"
    );
}

/// An interrupt says what it stopped, and nothing is signalled at an idle session.
#[test]
fn what_an_interrupt_stopped() {
    assert_eq!(
        interrupted(&working(bash_for(180)), NOW),
        (
            "interrupted child: stopped bash `cargo test` after 3m; its turn ends, and its next \
             turn reads that you interrupted it"
                .to_string(),
            true
        )
    );
    assert_eq!(
        interrupted(
            &working(Activity::Thinking {
                since: NOW - SECOND
            }),
            NOW
        ),
        (
            "interrupted child: stopped its generation; it is idle".to_string(),
            true
        )
    );
    assert_eq!(
        interrupted(&view(Condition::Idle { closing: None }), NOW),
        ("nothing to interrupt: child is idle".to_string(), false)
    );
}

#[test]
fn what_an_archive_ended() {
    assert_eq!(
        archived("child", &[]),
        "archived child; its log stays on disk"
    );
    assert_eq!(
        archived("child", &["a".into(), "b".into()]),
        "archived child and the 2 sessions it created (a, b); logs stay on disk"
    );
}

/// A created session: its model, where it works, where its gates go, and its status.
#[test]
fn what_a_creation_made() {
    let reader = Reader::read(Relation::Driver, &Moves::IN_SESSION, Some("parent"));
    let then = fresh(&view(Condition::Idle { closing: None }), &reader, NOW);
    assert_eq!(then, "child: idle, waiting for its first message");
    assert_eq!(
        created(&Created {
            id: "child",
            model: Some("anthropic/claude-haiku-5.5".into()),
            working: "/repo/.efference/worktrees/child",
            project: Some("/repo"),
            branch: Some("efference/child".into()),
            gates: gates("you", false),
            watched: false,
            follow: None,
            then,
        }),
        "created child on anthropic/claude-haiku-5.5\n\
         cwd: /repo/.efference/worktrees/child (its own worktree, on branch efference/child)\n\
         gates: its gates come to you\n\
         child: idle, waiting for its first message"
    );
}

/// A creation over `efference mcp` says the mount watches the new agent, which call
/// reports it, and the stream that sends each of its decision points (docs/operating.md).
#[test]
fn a_watched_creation_names_wait() {
    assert_eq!(
        created(&Created {
            id: "child",
            model: None,
            working: "/repo",
            project: None,
            branch: None,
            gates: gates("the operator", false),
            watched: true,
            follow: Some(follow("ws://127.0.0.1:50123/token")),
            then: "delivered: child is working on it".into(),
        }),
        "created child\n\
         cwd: /repo\n\
         gates: its gates come to the operator\n\
         watched: wait() returns its statuses you have not seen\n\
         follow: ws://127.0.0.1:50123/token — in Claude Code, open a Monitor on it (ws) to be \
         woken by each decision point\n\
         delivered: child is working on it"
    );
}

/// A `wait` that ran out says so and gives each agent's line, not new.
#[test]
fn an_unchanged_wait_lists_each_agent() {
    assert_eq!(
        unchanged(
            100,
            vec!["a: idle, its turn is done".into(), "b: working".into()]
        ),
        "nothing new after 100s; where each agent is now (not new):\n\
         a: idle, its turn is done\n\
         b: working"
    );
}

/// An overview line marks the sessions the reader drives after their id.
#[test]
fn an_overview_marks_yours() {
    let reader = Reader::read(Relation::Driver, &Moves::IN_SESSION, Some("parent"));
    let idle = view(Condition::Idle { closing: None });
    assert_eq!(
        overview_line(&idle, &reader, true, NOW),
        "child (yours): idle, its turn is done"
    );
    let observer = Reader::read(Relation::Observer, &Moves::IN_SESSION, Some("parent"));
    assert_eq!(
        overview_line(&working(bash_for(5)), &observer, false, NOW),
        "child: working: running bash `cargo test` for 5s"
    );
}
