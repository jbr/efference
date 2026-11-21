//! The labelled corpus for the idle-triage effort comparison, and the ask builders that
//! make each case reviewable without spending a request (docs/triage.md).
//!
//! The ask mirror here is not on the measured path: `idle_attempt` calls the production
//! [`Triager::triage`](crate::curation::triage::Triager::triage), so this copy can only
//! drift in a dry run. It exists so a reader can see the text a case would send.

use super::super::super::{ASK_BANNER, TRIAGE_INSTRUCTION, append_inbox_and_background};
use crate::curation::inbox::ShownNotification;
use crate::record::log::{AssistantOutput, ContentBlock, LogEntry, Payload, Seq};
use crate::record::project::{Reader, WireTurn, project_semantic_for};
use crate::tools::process_monitor::BgView;

/// The trailing user turn the live idle triage sends: the inbox and background blocks, the
/// ask banner, and the instruction. Mirrors the four lines in [`Triager::triage`](crate::curation::triage::Triager::triage) so a dry
/// run can review the ask without spending a request; the measured path calls production
/// itself, which is what keeps this copy from standing in for it.
fn idle_ask(inbox: &[ShownNotification], background: &[BgView]) -> String {
    let mut ask = String::new();
    append_inbox_and_background(&mut ask, inbox, background);
    ask.push_str("\n\n");
    ask.push_str(ASK_BANNER);
    ask.push_str("\n\n");
    ask.push_str(TRIAGE_INSTRUCTION);
    ask
}

/// The messages a case would send: the agent's projection, a breakpoint, then the ask.
/// Same shape as [`Triager::triage`](crate::curation::triage::Triager::triage), for a dry run only.
pub fn idle_request(
    prefix: &[LogEntry],
    inbox: &[ShownNotification],
    background: &[BgView],
) -> Vec<WireTurn> {
    let mut messages = project_semantic_for(prefix, Reader::Main);
    messages.push(WireTurn::Breakpoint);
    messages.push(WireTurn::text(idle_ask(inbox, background)));
    messages
}

/// One synthetic idle event and the decision a careful triage should reach.
///
/// `expects_wake` is the label the arms are scored against, decided from what the operator
/// asked for in `prefix` — not from what any model answered. A case whose label the
/// operator's own request leaves ambiguous does not belong here.
pub struct IdleCase {
    pub name: &'static str,
    pub expects_wake: bool,
    /// Why the label is what it is, in one line. Printed beside the result so a reader can
    /// disagree with the label rather than having to infer it.
    pub rationale: &'static str,
    pub prefix: Vec<LogEntry>,
    pub inbox: Vec<ShownNotification>,
    pub background: Vec<BgView>,
}

impl IdleCase {
    /// The ask text alone, for reviewing a case without spending a request.
    pub fn ask(&self) -> String {
        idle_ask(&self.inbox, &self.background)
    }

    /// The case as it would go on the wire, for the same dry-run review.
    pub fn request(&self) -> Vec<WireTurn> {
        idle_request(&self.prefix, &self.inbox, &self.background)
    }
}

fn worked_session(task: &str, answer: &str) -> Vec<LogEntry> {
    vec![
        LogEntry::new(0, Payload::user(task)),
        LogEntry::new(
            1,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                Vec::new(),
                vec![ContentBlock::Text(answer.to_string())],
                Vec::new(),
            )),
        ),
    ]
}

fn notice(seq: Seq, source: &str, body: &str) -> ShownNotification {
    ShownNotification::new(seq, source, body)
}

/// The labelled corpus: six events, balanced between wakes an operator asked for and events
/// that should wait. Ordinary work that happens to finish is the ambiguous middle — it is
/// deliberately absent, because a label there would be a guess.
///
/// Every case has `wake: false` on its notification, matching the live loop: must-wake
/// notifications are pinned before the triage runs and never reach it as a decision.
pub fn synthetic_cases() -> Vec<IdleCase> {
    vec![
        IdleCase {
            name: "self-scheduled timer exit",
            expects_wake: true,
            rationale: "the operator asked to be told when the timer finished; the exit is the answer",
            prefix: worked_session(
                "Start a 10-minute timer and tell me when it's done.",
                "Timer started as p#2 (`sleep 600`); nothing else is pending here.",
            ),
            inbox: vec![notice(2, "process", "p#2 exited 0 · sleep 600 finished")],
            background: Vec::new(),
        },
        IdleCase {
            name: "announced disinterest",
            expects_wake: false,
            rationale: "the operator said not to interrupt them with the build; finishing is not news",
            prefix: worked_session(
                "Kick off the release build, then I'll check back after lunch — don't interrupt me with it.",
                "Started p#3 (`cargo build --release`); I'll stay quiet until you ask.",
            ),
            inbox: vec![notice(
                2,
                "process",
                "p#3 exited 0 · cargo build --release finished",
            )],
            background: Vec::new(),
        },
        IdleCase {
            name: "operator question",
            expects_wake: true,
            rationale: "a person asked a direct question; it needs an answer now",
            prefix: worked_session(
                "Work on the log migration until it's done.",
                "Migration half applied; three tables remain.",
            ),
            inbox: vec![notice(
                2,
                "web",
                "jbr: are you still on the migration? what's left?",
            )],
            background: Vec::new(),
        },
        IdleCase {
            name: "ambient FYI",
            expects_wake: false,
            rationale: "a deploy notice with no action and no request attached",
            prefix: worked_session(
                "Land the docs fix, then stop for the day.",
                "Docs fix landed.",
            ),
            inbox: vec![notice(
                2,
                "mcp[notify]",
                "docs deploy finished on main (FYI, no action needed)",
            )],
            background: Vec::new(),
        },
        IdleCase {
            name: "watched CI result",
            expects_wake: true,
            rationale: "the operator asked to be told whether CI went green; this is that answer",
            prefix: worked_session(
                "Push the branch and tell me whether CI is green.",
                "Pushed; watching run 882.",
            ),
            inbox: vec![notice(
                2,
                "process",
                "p#4 exited 0 · gh run watch 882: all checks passed",
            )],
            background: Vec::new(),
        },
        IdleCase {
            name: "poll-shaped status",
            expects_wake: false,
            rationale: "a repeating poll says nothing new; mute the tail or leave it, but there is nothing to wake for",
            prefix: worked_session(
                "Wait for quota to free up, then start the batch.",
                "Waiting; p#5 polls every minute.",
            ),
            inbox: Vec::new(),
            background: vec![BgView {
                handle: "p#5".to_string(),
                command: "./scripts/wait-for-quota.sh".to_string(),
                tail: "quota still exhausted (4th retry), sleeping 60s".to_string(),
                running_secs: 240,
                idle_secs: 12,
            }],
        },
    ]
}
