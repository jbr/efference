//! The shared triage's request bytes (docs/triage.md): the main model triaging its own
//! results forks the main transcript, so its request must stay byte-identical to the
//! agent's prefix plus the fork tail. Pinned so a change to the separate triage's view
//! cannot move it.
use super::*;
use crate::log::{ContentBlock, Keep, Triaged};

fn assistant(seq: u64, text: &str, calls: Vec<ToolCall>) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text(text.into())],
                calls,
            ),
            upstream_failure: None,
        },
    )
}

#[test]
fn the_shared_triage_request_is_pinned() {
    let first = ToolCall::new("bash", "c1", json!({"command": "ls"})).with_intent("see the files");
    let triaged =
        ToolCall::new("bash", "c2", json!({"command": "cargo test"})).with_intent("run the tests");
    let prefix = vec![
        entry(1, Payload::User(vec![ContentBlock::Text("fix it".into())])),
        assistant(2, "Looking around first.", vec![first]),
        entry(
            3,
            Payload::ToolResult {
                call_id: "c1".into(),
                raw: RawResult::text("a.rs\nb.rs"),
                wire: WireState::Triaged(Triaged::new(Keep::All).with_note("Two files.")),
            },
        ),
        assistant(4, "Now the tests.", vec![triaged.clone()]),
    ];
    let raw = RawResult::text("running 3 tests\ntest result: ok. 3 passed");
    let pending = [Pending {
        call: &triaged,
        raw: &raw,
    }];
    let inbox = [ShownNotification {
        seq: 9,
        source: "timer".into(),
        body: "rang".into(),
    }];
    let turns = triage_request(
        &prefix,
        Reader::Main,
        TriageView::Transcript,
        &pending,
        &inbox,
        &[],
    );
    let got = serde_json::to_string(&wire(&turns)).unwrap();
    assert_eq!(got, EXPECTED);
}

/// Generated from the request as it stood before the separate triage moved to the text view.
const EXPECTED: &str = r##"[{"role":"user","content":[{"type":"text","text":"fix it"}]},{"role":"assistant","content":[{"type":"text","text":"Looking around first."},{"type":"tool_use","id":"c1","name":"bash","input":{"command":"ls","intent":"see the files"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"[3] a.rs\nb.rs\n⟢ Two files."}]},{"role":"assistant","content":[{"type":"text","text":"Now the tests."},{"type":"tool_use","id":"c2","name":"bash","input":{"command":"cargo test","intent":"run the tests"},"cache_control":{"type":"ephemeral"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c2","content":"═══ result r1 · bash ═══\nintent: run the tests\n     1\trunning 3 tests\n     2\ttest result: ok. 3 passed"},{"type":"text","text":"⟢ triage · the harness asks: answer, don't act\n\nCall `retain` now. This is a triage turn, not a work turn: the task continues on your NEXT turn, from whatever you keep here. No other tool will run until you have called `retain` — a call to anything else is rejected and this ask comes back. The verdict is the `retain` call's arguments and nothing else — prose is not read. Describing what to keep instead of calling `retain` leaves the result untriaged.\n\nThe block headed `═══ result` above is the FULL, line-numbered output of the one call you just made. Nothing earlier in this conversation is being triaged — those results are already decided, and their [n] tags are not result labels. Answer with a top-level `keep` and `note`: no `results` array and no label, since there is only one result in question.\n\n  keep        \"all\", \"none\", or comma-separated 1-based inclusive line ranges like\n              \"40-52,310-318\". Whatever you drop is gone from the working context, so keep\n              anything that might matter for the rest of the task, while staying as lean\n              as you can. Where the output enumerates — matches, a listing, a set of\n              candidates — the lines you keep read afterwards as the whole set, so keep\n              the enumeration whole or say in the `note` how many of how many survived.\n  note        One line, saying what this output showed — only what it showed. It is a\n              note to self: it is read on the next turn, in the working context, in\n              place of the lines that were dropped. Do not\n              generalize past it or explain what it means for the task. Ground it in\n              the lines you kept, not the ones you dropped: if the note turns on a\n              particular line, that line belongs in `keep`. Where the\n              result header carries an `intent`, answer it from what is shown. If the\n              output does not contain what the intent asked for, say that rather than\n              summarizing what it does contain; \"it didn't\" is often the answer\n              worth keeping.\n  intent_feedback\n              Omit almost always. Use only when the `intent` gave you nothing to\n              decide against — it restated the arguments, or named no question the\n              output could answer — and say so in one clause, so the next intent is\n              one you can act on. This is about the intent, never about the task: do\n              not suggest what to call next or what the output means.\n  inbox       The pending notifications under \"inbox\" in the sections after this ask, an\n              [id] each. `pin` — it enters your working context and you resume work on it now. `leave` (list nothing for it) — it stays pending and is shown again at the next triage, no turn runs, and the harness may dismiss it if it keeps going unmentioned. `dismiss` — it is cleared for good, you will not see it again, and no turn runs.\n\nCall `retain`.\n\n═══ also pending · not the output of the call above ═══\nThe sections below are separate from the result blocks. Answer them in the matching fields described above; the result's `keep` and `note` are about the result alone.\n\n═══ inbox · 1 pending ═══\n[9] timer · rang"}]}]"##;
