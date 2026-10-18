use super::*;
use crate::api::{PromptCachePolicy, encode_responses};
use crate::log::{
    AssistantOutput, ContentBlock, Keep, LoggedInference, RawResult, Summary, Timestamp, ToolCall,
    Triaged, WireState,
};
use crate::project::{BreakpointPlacement, ImagePlacement, encode_messages};
use serde_json::json;

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(1_700_000_000_000 + seq),
        submission_id: None,
        source: None,
        payload,
    }
}

fn assistant(seq: u64, text: &str, calls: Vec<ToolCall>) -> LogEntry {
    let text = match text {
        "" => vec![],
        text => vec![ContentBlock::Text(text.into())],
    };
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(vec![], text, calls),
            excluded: None,
        },
    )
}

fn call(n: u64) -> ToolCall {
    ToolCall::new(
        "bash",
        format!("c{n}"),
        json!({"command": format!("step {n}")}),
    )
    .with_intent(format!("run step {n}"))
}

/// A session of `steps` steps, each a call whose result of `size` bytes was kept whole,
/// then the triage's own turn: the log, and the call the triage covers.
fn session(steps: u64, size: usize) -> (Vec<LogEntry>, ToolCall) {
    let mut log = vec![entry(
        1,
        Payload::User(vec![ContentBlock::Text("do the steps".into())]),
    )];
    let mut seq = 1;
    for n in 1..=steps {
        seq += 1;
        log.push(assistant(seq, &format!("Step {n}."), vec![call(n)]));
        seq += 1;
        let body = format!("output of step {n}\n").repeat(size / 16);
        log.push(entry(
            seq,
            Payload::ToolResult {
                call_id: call(n).id,
                raw: RawResult::text(body),
                wire: WireState::Triaged(
                    Triaged::new(Keep::All).with_note(format!("step {n} ran")),
                ),
            },
        ));
    }
    let triaged = call(steps + 1);
    log.push(assistant(seq + 1, "The last step.", vec![triaged.clone()]));
    (log, triaged)
}

fn request(log: &[LogEntry], triaged: &ToolCall, reader: Reader) -> Vec<WireTurn> {
    let raw = RawResult::text("final output\nline two");
    let pending = [Pending {
        call: triaged,
        raw: &raw,
    }];
    text_triage_messages(log, reader, &pending, &[], &[], &[])
}

fn texts(parts: &[UserPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            UserPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

/// The view is text only — no assistant turn, no tool block — so it encodes on any
/// dialect. The history turn ends on its breakpoint; the current turn carries the
/// triage's own prose, its call in full with the raw output, and the third-person ask.
#[test]
fn the_text_view_is_a_history_turn_then_the_current_step() {
    let (log, triaged) = session(2, 64);
    let turns = request(&log, &triaged, Reader::Triage);
    let [WireTurn::User(history), WireTurn::User(current)] = turns.as_slice() else {
        panic!("two user turns: {turns:?}");
    };
    assert!(matches!(history.last(), Some(UserPart::Breakpoint)));
    for part in history.iter().chain(current) {
        assert!(
            !matches!(part, UserPart::ToolResult { .. }),
            "no tool blocks"
        );
    }
    let history = texts(history);
    assert!(history.starts_with(HISTORY_HEAD));
    for expected in ["do the steps", "Step 1.", "step 1 ran", "output of step 2"] {
        assert!(
            history.contains(expected),
            "missing {expected:?}:\n{history}"
        );
    }
    assert!(
        !history.contains("The last step."),
        "the triage's own turn is the current step, not history"
    );
    let current = texts(current);
    assert!(current.starts_with(CURRENT_HEAD));
    for expected in [
        "The last step.",
        "• bash {\"command\":\"step 3\",\"intent\":\"run step 3\"}",
        "     1\tfinal output",
        "the one call the agent just made",
        "the agent's task continues on its NEXT turn",
    ] {
        assert!(
            current.contains(expected),
            "missing {expected:?}:\n{current}"
        );
    }
    assert!(!current.contains("you just made"), "{current}");
}

/// A live triage's log already holds the batch's captured results and the inbox it
/// opened; they are the current step, so the history is the same as at the batch's
/// start.
#[test]
fn entries_after_the_triages_own_turn_stay_out_of_the_history() {
    let (mut log, triaged) = session(2, 64);
    let before = request(&log, &triaged, Reader::Triage);
    log.push(entry(
        99,
        Payload::ToolResult {
            call_id: triaged.id.clone(),
            raw: RawResult::text("final output"),
            wire: WireState::Pending,
        },
    ));
    assert_eq!(request(&log, &triaged, Reader::Triage), before);
}

/// The same log yields the same bytes, on every wire.
#[test]
fn the_same_log_yields_identical_bytes() {
    let (log, triaged) = session(30, 4000);
    let encode = |turns: &[WireTurn]| {
        let messages = encode_messages(
            turns,
            ImagePlacement::default(),
            BreakpointPlacement::default(),
        );
        let responses = encode_responses(turns, "", PromptCachePolicy::Explicit);
        (
            serde_json::to_string(&messages).unwrap(),
            serde_json::to_string(&responses).unwrap(),
        )
    };
    let first = encode(&request(&log, &triaged, Reader::Triage));
    let second = encode(&request(&log.clone(), &triaged, Reader::Triage));
    assert_eq!(first, second);
}

/// The histories at every triage of a long session: each, laid out, beside the next.
fn histories(steps: u64, size: usize) -> Vec<History> {
    let (log, _) = session(steps, size);
    (1..=steps)
        .map(|n| history(&log, history_end(&log, Some(&call(n))), Reader::Triage))
        .collect()
}

/// Between cuts a later triage's history extends an earlier one's: the earlier history,
/// up to its closing breakpoint, is a prefix of the later one's parts, so the earlier
/// request's cache point is a block boundary in the later request. Across a cut, the
/// earlier digest blocks are a prefix of the later ones.
#[test]
fn a_longer_logs_history_extends_a_shorter_ones_between_cuts() {
    let histories = histories(60, 6000);
    let mut cuts = 0;
    for pair in histories.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        let open = &before.parts[..before.parts.len() - 1];
        if before.cuts == after.cuts {
            assert!(
                after.parts.starts_with(open),
                "between cuts the history only grows"
            );
            assert_eq!(
                after.parts.len(),
                before.parts.len() + 1,
                "by one block, the step that landed"
            );
        } else {
            cuts += 1;
            let digest = |h: &History| {
                h.parts
                    .iter()
                    .take_while(|p| match p {
                        UserPart::Text(t) => !t.starts_with(TAIL_HEAD),
                        _ => false,
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert!(
                digest(after).starts_with(&digest(before)),
                "a cut appends digest blocks and rewrites none"
            );
        }
    }
    assert!(
        cuts >= 2,
        "the session is long enough to cut more than once"
    );
}

/// Where the cuts fall is a pure function of the log: a later triage's cuts begin with
/// an earlier one's, the triaged tail never passes the high-water mark, and a cut leaves
/// at most the low-water mark behind it.
#[test]
fn cuts_are_a_fold_over_the_log() {
    let histories = histories(60, 6000);
    for pair in histories.windows(2) {
        assert!(pair[1].cuts.starts_with(&pair[0].cuts));
    }
    for (i, h) in histories.iter().enumerate() {
        assert!(
            h.tail_bytes <= TRIAGED_TAIL_HIGH_WATER,
            "{i}: {}",
            h.tail_bytes
        );
        if i > 0 && h.cuts.len() > histories[i - 1].cuts.len() {
            assert!(
                h.tail_bytes <= TRIAGED_TAIL_LOW_WATER,
                "{i}: {}",
                h.tail_bytes
            );
        }
    }
    let last = histories.last().unwrap();
    assert!(last.digest_bytes > 0);
    // The digest keeps one line per call, not its output.
    let digest: String = texts(&last.parts);
    assert!(digest.contains("• [bash] run step 1\n    → step 1 ran"));
}

/// With a cut, the history carries two breakpoints: the end of the digest, which a
/// later cut leaves in place, and the end of the history.
#[test]
fn a_cut_history_marks_the_digest_end_and_the_history_end() {
    let histories = histories(30, 6000);
    let last = histories.last().unwrap();
    assert!(!last.cuts.is_empty());
    let marks = last
        .parts
        .iter()
        .filter(|p| matches!(p, UserPart::Breakpoint))
        .count();
    assert_eq!(marks, 2);
    let messages = encode_messages(
        &[WireTurn::User(last.parts.clone())],
        ImagePlacement::default(),
        BreakpointPlacement::LastTextBlock,
    );
    let json = serde_json::to_string(&messages).unwrap();
    assert_eq!(json.matches("cache_control").count(), 2);
}

/// A head summary written for the triage reader leads the history and hides what it
/// covers; read for the main model, the same log ignores it.
#[test]
fn a_triage_summary_leads_and_replaces_its_range() {
    let (mut log, triaged) = session(4, 64);
    let anchor = log.pop().unwrap();
    let model = LoggedInference::new("c", crate::api::Provider::Anthropic, "m");
    log.push(entry(
        50,
        Payload::Summary(
            Summary::compaction(5, "The first two steps ran.", model)
                .for_audience(crate::log::Audience::Triage),
        ),
    ));
    log.push(anchor);
    let triage = request(&log, &triaged, Reader::Triage);
    let WireTurn::User(parts) = &triage[0] else {
        panic!()
    };
    let text = texts(parts);
    assert!(text.contains(&format!("{SUMMARY_HEAD}\nThe first two steps ran.")));
    assert!(!text.contains("step 1 ran") && !text.contains("do the steps"));
    assert!(text.contains("output of step 3"));
    let main = request(&log, &triaged, Reader::Main);
    let WireTurn::User(parts) = &main[0] else {
        panic!()
    };
    let text = texts(parts);
    assert!(!text.contains("The first two steps ran.") && text.contains("step 1 ran"));
}

/// On Responses the history is its own input message, so a primer that sends the
/// history alone ends exactly where the triage's request continues — the point the
/// Codex backend resumes from (docs/transport.md).
#[test]
fn the_primer_ends_where_the_triage_request_continues() {
    let (log, triaged) = session(5, 64);
    let triage = request(&log, &triaged, Reader::Triage);
    let primer = vec![history(&log, history_end(&log, Some(&triaged)), Reader::Triage).turn()];
    let primer = encode_responses(&primer, "", PromptCachePolicy::Implicit);
    let triage = encode_responses(&triage, "", PromptCachePolicy::Implicit);
    assert_eq!(primer.len(), 1);
    assert_eq!(triage.len(), 2);
    assert_eq!(
        serde_json::to_string(&primer[0]).unwrap(),
        serde_json::to_string(&triage[0]).unwrap()
    );
}
