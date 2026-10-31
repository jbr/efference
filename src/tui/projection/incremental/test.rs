//! The incremental projection's two obligations: it must agree with the
//! from-scratch [`project`](crate::tui::projection::project), and it must leave
//! untouched entries alone.

use super::*;
use crate::log::{ContentBlock, LogEntry, Payload, RawResult, ToolCall, Triaged, WireState};
use crate::tui::document::Document;
use crate::tui::projection::project;
use crate::tui::state::DocumentState;

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry::new(seq, payload).with_turn(1)
}

fn user(seq: u64, text: &str) -> LogEntry {
    entry(seq, Payload::user(text))
}

fn assistant_call(seq: u64, text: &str, call_id: &str, command: &str) -> LogEntry {
    entry(
        seq,
        Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
            vec![],
            vec![ContentBlock::Text(text.into())],
            vec![ToolCall::new(
                crate::bash::NAME,
                call_id,
                serde_json::json!({ "command": command }),
            )],
        )),
    )
}

fn result(seq: u64, call_id: &str, output: &str) -> LogEntry {
    entry(
        seq,
        Payload::tool_result(call_id, RawResult::text(output), WireState::Raw),
    )
}

fn empty_doc() -> DocumentState<'static> {
    DocumentState {
        document: Document::new(),
        layouts: Vec::new(),
        layout_width: 0,
        laid_out: 0,
        highlighted: 0,
    }
}

/// Feed `log` to a fresh transcript one entry at a time — the way `/events`
/// delivers it — and return the incrementally-built document.
fn stream(log: &[LogEntry], view: &mut ViewState) -> DocumentState<'static> {
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    for n in 1..=log.len() {
        transcript.update(
            &mut doc,
            &log[..n],
            &LiveTail::default(),
            view,
            Gate::default(),
            &[],
        );
    }
    doc
}

fn shape(nodes: &[crate::tui::document::DocumentNode<'_>]) -> String {
    format!("{nodes:#?}")
}

fn a_session() -> Vec<LogEntry> {
    vec![
        user(0, "check the tests"),
        assistant_call(1, "Running them.", "call-1", "cargo test"),
        result(2, "call-1", "test result: ok. 403 passed"),
        assistant_call(3, "All green. Now the lints.", "call-2", "cargo clippy"),
        result(4, "call-2", "no warnings"),
        user(5, "thanks"),
    ]
}

/// The invariant everything else rests on: streaming the log in produces the same
/// document as projecting it in one pass. If this fails, the cache is lying about
/// what changed.
#[test]
fn incremental_matches_a_from_scratch_projection() {
    let log = a_session();
    let mut view = ViewState::default();
    let doc = stream(&log, &mut view);
    let expected = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
    assert_eq!(shape(&doc.document.nodes), shape(&expected.nodes));
}

#[test]
fn an_artifact_url_reprojects_only_its_committed_assistant_entry() {
    let log = vec![
        user(6, "before"),
        entry(
            7,
            Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text(
                    "Chart:\n\n```html-artifact\n<svg>chart</svg>\n```".into(),
                )],
                vec![],
            )),
        ),
        user(8, "after"),
    ];
    let mut view = ViewState::default();
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    let tail = LiveTail::default();
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);
    let source_only = shape(&doc.document.nodes);
    assert!(source_only.contains("<svg>chart</svg>"));
    assert!(!source_only.contains("127.0.0.1"));

    // Stand in for a completed layout pass; only the target entry should lose its layout.
    doc.layouts = (0..doc.document.nodes.len())
        .map(|_| {
            Some(crate::tui::state::NodeLayout {
                height: 3,
                actions: Vec::new(),
                level: Default::default(),
            })
        })
        .collect();
    let before = transcript.counts[0];
    let target_end = before + transcript.counts[1];

    let coordinate = Coordinate {
        seq: 7,
        text_block: 0,
        fence: 0,
    };
    assert!(transcript.set_artifact_url(
        &log,
        coordinate,
        "http://127.0.0.1:4321/artifacts/token/".into(),
    ));
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    let with_link = shape(&doc.document.nodes);
    assert!(
        with_link.contains("<svg>chart</svg>"),
        "source remains visible"
    );
    assert!(with_link.contains("↗ artifact · http://127.0.0.1:4321/artifacts/token/"));
    assert!(doc.layouts[..before].iter().all(Option::is_some));
    assert!(doc.layouts[before..target_end].iter().all(Option::is_none));
    assert!(doc.layouts[target_end..].iter().all(Option::is_some));
    assert!(!transcript.set_artifact_url(
        &log,
        coordinate,
        "http://127.0.0.1:4321/artifacts/token/".into(),
    ));
}

/// A result arrives for a call several entries back: it must re-render *that*
/// entry's exchange, still matching a from-scratch projection.
#[test]
fn a_late_result_rebuilds_the_entry_that_made_the_call() {
    let mut log = vec![
        assistant_call(0, "one", "call-1", "sleep 10"),
        user(1, "still going?"),
    ];
    let mut view = ViewState::default();
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    let tail = LiveTail::default();
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    log.push(result(2, "call-1", "done"));
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    let expected = project(&log, &tail, &view, Gate::default(), &[]);
    assert_eq!(shape(&doc.document.nodes), shape(&expected.nodes));
}

/// The gate is ephemeral (`/state`, not the log), so its arrival and departure have
/// to be noticed by the cache itself.
#[test]
fn a_gate_arriving_and_resolving_rebuilds_its_call() {
    let log = vec![assistant_call(0, "", "call-1", "rm -rf /tmp/scratch")];
    let mut view = ViewState::default();
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    let tail = LiveTail::default();

    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);
    let ungated = shape(&doc.document.nodes);

    transcript.update(
        &mut doc,
        &log,
        &tail,
        &mut view,
        Gate {
            call: Some("call-1"),
            reason: Some("deletes outside the git tree"),
            acting_on: None,
            ..Default::default()
        },
        &[],
    );
    let gated = shape(&doc.document.nodes);
    assert_ne!(ungated, gated, "the gate must show up at the call");
    let expected = project(
        &log,
        &tail,
        &view,
        Gate {
            call: Some("call-1"),
            reason: Some("deletes outside the git tree"),
            acting_on: None,
            ..Default::default()
        },
        &[],
    );
    assert_eq!(gated, shape(&expected.nodes));

    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);
    assert_eq!(
        shape(&doc.document.nodes),
        ungated,
        "resolving the gate must take the affordance back off"
    );
}

/// Flipping an exchange to raw is a view toggle, not a log event — the entry that
/// owns the call has to be rebuilt on the strength of the toggle alone.
#[test]
fn a_raw_toggle_rebuilds_only_its_own_exchange() {
    let log = vec![
        assistant_call(0, "", "call-1", "ls"),
        entry(
            1,
            Payload::tool_result(
                "call-1",
                RawResult::text("a\nb\nc"),
                WireState::from(Triaged::new(crate::log::Keep::None).with_note("three files")),
            ),
        ),
        user(2, "ok"),
    ];
    let mut view = ViewState::default();
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    let tail = LiveTail::default();
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    view.toggle_raw("call-1");
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    let expected = project(&log, &tail, &view, Gate::default(), &[]);
    assert_eq!(shape(&doc.document.nodes), shape(&expected.nodes));
    assert!(
        shape(&doc.document.nodes).contains("a\\nb\\nc"),
        "the raw output should now be what the exchange shows"
    );
}

/// The point of the whole exercise: an entry that nothing has happened to keeps its
/// node — and therefore its cached layout — across an update. Asserted through
/// `splice_nodes`, which is the only thing that can drop a layout.
#[test]
fn untouched_entries_keep_their_cached_layout() {
    let mut log = a_session();
    let mut view = ViewState::default();
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    let tail = LiveTail::default();
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    // Stand in for a completed layout pass.
    let n = doc.document.nodes.len();
    doc.layouts = (0..n)
        .map(|_| {
            Some(crate::tui::state::NodeLayout {
                height: 3,
                actions: Vec::new(),
                level: Default::default(),
            })
        })
        .collect();

    // One more turn arrives, as it would mid-session.
    log.push(assistant_call(6, "one more", "call-3", "git status"));
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    let kept = doc.layouts[..n].iter().filter(|l| l.is_some()).count();
    assert_eq!(
        kept, n,
        "appending a turn must not invalidate the layout of anything before it"
    );
    assert!(
        doc.layouts[n..].iter().all(Option::is_none),
        "the new node has no layout yet"
    );
}

/// A log that was replaced rather than appended to (a reattach that re-delivers the
/// session from the start) has to fall back to a clean rebuild.
#[test]
fn a_shrinking_log_rebuilds_from_scratch() {
    let log = a_session();
    let mut view = ViewState::default();
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    let tail = LiveTail::default();
    transcript.update(&mut doc, &log, &tail, &mut view, Gate::default(), &[]);

    let shorter = &log[..2];
    transcript.update(&mut doc, shorter, &tail, &mut view, Gate::default(), &[]);
    let expected = project(shorter, &tail, &view, Gate::default(), &[]);
    assert_eq!(shape(&doc.document.nodes), shape(&expected.nodes));
}

/// The `ask` topic moving between the asks of one completion changes their marks with no
/// log entry, so the cache has to notice it as it does a gate.
#[test]
fn the_ask_topic_moving_rebuilds_the_marks() {
    let asks = entry(
        0,
        Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
            vec![],
            vec![],
            vec![
                ToolCall::new("ask", "q1", serde_json::json!({ "question": "First?" })),
                ToolCall::new("ask", "q2", serde_json::json!({ "question": "Second?" })),
            ],
        )),
    );
    let log = vec![asks];
    let mut view = ViewState::default();
    let mut doc = empty_doc();
    let mut transcript = Transcript::default();
    let tail = LiveTail::default();
    for live in [None, Some("q1"), Some("q2"), None] {
        let gate = Gate {
            ask: live,
            ..Default::default()
        };
        transcript.update(&mut doc, &log, &tail, &mut view, gate, &[]);
        let expected = project(&log, &tail, &view, gate, &[]);
        assert_eq!(
            shape(&doc.document.nodes),
            shape(&expected.nodes),
            "topic at {live:?}"
        );
    }
    let first = project(
        &log,
        &tail,
        &view,
        Gate {
            ask: Some("q1"),
            ..Default::default()
        },
        &[],
    );
    let second = project(
        &log,
        &tail,
        &view,
        Gate {
            ask: Some("q2"),
            ..Default::default()
        },
        &[],
    );
    assert_ne!(shape(&first.nodes), shape(&second.nodes));
}
