//! The order and labelling of a triage request's trailing user turn (docs/triage.md): the
//! result blocks, then the ask that names them, then the riders under their own header.
use super::*;
use crate::record::log::context::Item;
use crate::tools::process_monitor::BgView;

fn listed(name: &str) -> Item {
    Item::Listed {
        name: name.into(),
        description: format!("what {name} does"),
        path: format!("skills/{name}.md"),
    }
}

/// The trailing user turn of a one-result triage carrying all three riders, as
/// `(tool_result contents, text blocks)`.
fn tail_with_riders() -> (Vec<String>, Vec<String>) {
    let call = ToolCall::new("read", "read_1", json!({}));
    let raw = RawResult::text("alpha\nbeta");
    let prefix = vec![
        entry(1, Payload::user("go")),
        entry(
            2,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![call.clone()],
            )),
        ),
    ];
    let pending = [Pending {
        call: &call,
        raw: &raw,
    }];
    let inbox = [ShownNotification::new(7, "ci", "42 passed")];
    let background = [BgView {
        handle: "p#1".into(),
        command: "cargo watch".into(),
        running_secs: 5,
        idle_secs: 1,
        tail: "ok".into(),
    }];
    let roster = [listed("ferritin")];
    let messages = wire(&triage_messages(
        &prefix,
        Reader::Main,
        &pending,
        &inbox,
        &background,
        &roster,
    ));
    let tail = messages.last().expect("a tail");
    let mut results = Vec::new();
    let mut texts = Vec::new();
    for b in &tail.content {
        match b {
            ApiBlock::ToolResult { content, .. } => {
                results.push(content.as_text().unwrap_or_default().to_string());
            }
            ApiBlock::Text { text, .. } => texts.push(text.clone()),
            _ => {}
        }
    }
    (results, texts)
}

/// The ask follows the result blocks directly and names them by their header; the riders
/// follow the ask, so the block above the ask is the result rather than the last rider.
#[test]
fn the_ask_follows_the_result_and_the_riders_follow_the_ask() {
    let (results, texts) = tail_with_riders();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].starts_with("═══ result r1 · read ═══"),
        "{results:?}"
    );
    assert_eq!(
        texts.len(),
        1,
        "ask and riders ride one text block: {texts:?}"
    );
    let text = &texts[0];
    assert!(
        text.starts_with("⟢ triage · the harness asks"),
        "the ask opens the text block: {text}"
    );
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing: {text}"))
    };
    assert!(
        at("headed `═══ result` above") < at("═══ also pending"),
        "the ask names the result blocks before the riders: {text}"
    );
    assert!(
        at("═══ also pending") < at("═══ inbox · 1 pending ═══"),
        "{text}"
    );
    assert!(
        at("═══ inbox · 1 pending ═══") < at("═══ background · 1 running ═══"),
        "{text}"
    );
    assert!(
        at("═══ background · 1 running ═══") < at("═══ context · 1 available ═══"),
        "{text}"
    );
    assert!(
        text.contains("not the output of the call above"),
        "the riders header says what they are: {text}"
    );
    assert!(
        !text[..at("═══ also pending")].contains("═══ inbox"),
        "no rider before the header: {text}"
    );
}

/// With no riders the ask is the whole text block and carries no riders header.
#[test]
fn no_riders_means_no_riders_header() {
    let call = ToolCall::new("read", "read_1", json!({}));
    let raw = RawResult::text("alpha");
    let pending = [Pending {
        call: &call,
        raw: &raw,
    }];
    let (_, ask) = triage_parts(&pending, &[], &[], &[], Reader::Main, Framing::FirstPerson);
    assert!(ask.starts_with("⟢ triage"), "{ask}");
    assert!(!ask.contains("also pending"), "{ask}");
}

/// The shape sentence names the result blocks by header in both answer shapes.
#[test]
fn both_shapes_name_the_result_blocks_by_header() {
    let one = retain_instruction(&["r1".to_string()], &[], Reader::Main, Framing::FirstPerson);
    assert!(
        one.contains("The block headed `═══ result` above is the FULL"),
        "{one}"
    );
    let many = retain_instruction(
        &["r1".to_string(), "r2".to_string()],
        &[],
        Reader::Main,
        Framing::FirstPerson,
    );
    assert!(
        many.contains("The blocks headed `═══ result` above are the FULL"),
        "{many}"
    );
}

/// A result carrying images says so in its header and the ask says they pass through; a
/// result without images gets neither (docs/triage.md).
#[test]
fn image_results_are_marked_and_the_ask_says_they_pass_through() {
    use crate::record::log::ResultImage;
    let call = ToolCall::new("read_image", "img_1", json!({}));
    let image = || ResultImage {
        media_type: "image/webp".into(),
        data: "UklGRg==".into(),
        width: 10,
        height: 10,
    };
    let with = RawResult::text("10×10 image/webp").with_images(vec![image()]);
    let (results, ask) = triage_parts(
        &[Pending {
            call: &call,
            raw: &with,
        }],
        &[],
        &[],
        &[],
        Reader::Main,
        Framing::FirstPerson,
    );
    assert!(
        results[0]
            .content
            .starts_with("═══ result r1 · read_image ═══\nimages: 1 image — not shown here; reaches the agent as it is\n"),
        "{}",
        results[0].content
    );
    assert!(
        ask.contains("  images      A result header that says `images:` is a result that carries images. They reach\n              the agent as they are, whatever you decide, and are not yours to triage:"),
        "{ask}"
    );
    assert!(
        ask.contains("write no note about the\n              image or whether it was retained."),
        "{ask}"
    );

    let plain = RawResult::text("alpha");
    let (results, ask) = triage_parts(
        &[Pending {
            call: &call,
            raw: &plain,
        }],
        &[],
        &[],
        &[],
        Reader::Main,
        Framing::FirstPerson,
    );
    assert!(
        !results[0].content.contains("images:"),
        "{}",
        results[0].content
    );
    assert!(!ask.contains("  images      "), "{ask}");
}

/// The idle ask and the per-result ask's inbox row define pin, leave, and dismiss with the
/// same words, say what leave costs, and the idle ask keeps the "nothing else will wake you"
/// point without the old "nothing changes" framing (docs/inbox.md).
#[test]
fn both_asks_define_the_dispositions_in_the_same_words() {
    let defs = "`pin` — it enters your working context and you resume work on it now. `leave` (list nothing for it) — it stays pending and is shown again at the next triage, no turn runs, and the harness may dismiss it if it keeps going unmentioned. `dismiss` — it is cleared for good, you will not see it again, and no turn runs.";
    assert!(TRIAGE_INSTRUCTION.contains(defs), "{TRIAGE_INSTRUCTION}");
    assert!(INBOX_FIELD.contains(defs), "{INBOX_FIELD}");
    assert!(
        TRIAGE_INSTRUCTION.contains("Nothing else will wake you: if a notification is something you said you would act on — a process you started, a timer you set, a reply you are waiting for — leaving or dismissing it means you wait until someone else sends a turn."),
        "{TRIAGE_INSTRUCTION}"
    );
    assert!(
        !TRIAGE_INSTRUCTION.contains("nothing changes"),
        "{TRIAGE_INSTRUCTION}"
    );
}
