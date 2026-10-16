//! Tests for the ask's own text and shape.
use super::*;

#[test]
fn triage_view_numbers_every_line() {
    // The throwaway fork's view — per-line numbers are the `retain` coordinate
    // system. (Only here; never on the canonical wire.)
    let ls = lines("alpha\nbeta\ngamma");
    let view = LineView::Numbers.render(&ls);
    assert!(view.contains("     1\talpha"), "{view}");
    assert!(view.contains("     3\tgamma"), "{view}");
}

/// The triage tools' schemas are static (docs/triage.md): they ride every turn's
/// `tools[]`, so any variation would re-key the request envelope and split the cache
/// lineage. Byte-identical across calls, every rider field unconditional, and the
/// standing description defers use until the harness asks.
#[test]
fn triage_tool_schemas_are_static_and_permission_phrased() {
    assert_eq!(
        serde_json::to_string(&retain_tool()).unwrap(),
        serde_json::to_string(&retain_tool()).unwrap()
    );
    let retain = retain_tool();
    for field in ["results", "inbox", "background", "surfaced"] {
        assert!(
            retain["input_schema"]["properties"].get(field).is_some(),
            "`{field}` is unconditional"
        );
    }
    let triage = triage_tool();
    for tool in [&retain, &triage] {
        let description = tool["description"].as_str().unwrap();
        assert!(
            description.contains("unless asked to do so by the harness"),
            "permission-phrased, not identity-phrased: {description}"
        );
    }
    assert!(is_triage_tool("retain") && is_triage_tool("triage"));
    assert!(!is_triage_tool("bash"));
}

fn preamble_with(items: Vec<Item>) -> LogEntry {
    use crate::context::{Resolved, Section};
    entry(
        0,
        Payload::Preamble {
            system: "sys".into(),
            context: Resolved {
                sections: vec![Section {
                    heading: Some("Skills".into()),
                    description: None,
                    items,
                }],
            },
            inference: None,
        },
    )
}

fn listed(name: &str) -> Item {
    Item::Listed {
        name: name.into(),
        description: format!("what {name} does"),
        path: format!("skills/{name}.md"),
    }
}

#[test]
fn roster_lists_stub_items_and_excludes_inlined_and_exec() {
    // Only read-on-demand stubs (`Listed`) are surfaceable; inlined and exec bodies
    // are already whole on the wire, so there's nothing to load.
    let prefix = vec![preamble_with(vec![
        listed("ferritin"),
        Item::Inline {
            name: "CLAUDE".into(),
            path: "CLAUDE.md".into(),
            body: "the repo map".into(),
        },
        Item::Exec {
            name: "Env".into(),
            command: "env".into(),
            body: "PWD=/x".into(),
        },
        listed("jsonl-prose"),
    ])];
    assert_eq!(
        names(&surfaceable_roster(&prefix)),
        ["ferritin", "jsonl-prose"]
    );
}

/// The roster carries whole items now (so it can re-render them as the preamble did);
/// most assertions here are still about *which* items, by name.
fn names(roster: &[Item]) -> Vec<&str> {
    roster.iter().map(roster_name).collect()
}

#[test]
fn roster_excludes_already_surfaced_names() {
    // Surfaced-once dedup (docs/triage.md): a name any prior
    // `Triage.surfaced` carried drops out of the roster — folded from the log alone.
    let prefix = vec![
        preamble_with(vec![listed("ferritin"), listed("jsonl-prose")]),
        entry(
            1,
            Payload::Triage(Triage {
                for_result: Some(1),
                origin: Origin::Model,
                raw_keep: "all".into(),
                inbox: Vec::new(),
                surfaced: vec!["ferritin".into()],
                retries: Vec::new(),
                thinking: Vec::new(),
            }),
        ),
    ];
    assert_eq!(
        names(&surfaceable_roster(&prefix)),
        ["jsonl-prose"],
        "ferritin was surfaced already; only jsonl-prose remains offerable"
    );
}

#[test]
fn roster_is_empty_without_a_preamble() {
    // No listing → nothing to offer (the pre-snapshot / config-less case).
    assert!(surfaceable_roster(&[]).is_empty());
}

/// A single-result triage asks for the flat form and names no label at all: naming the
/// result when only one is in question is transcription with no decision in it
/// (docs/triage.md).
#[test]
fn a_single_result_gate_asks_for_the_flat_form_and_no_label() {
    let instruction =
        retain_instruction(&["r1".to_string()], &[], Reader::Main, Framing::FirstPerson);
    assert!(
        !instruction.contains("r1"),
        "nothing to copy: {instruction}"
    );
    assert!(instruction.contains("`keep` and `note`"));
    assert!(
        instruction.contains("no `results` array and no label"),
        "names the shape: {instruction}"
    );
}

/// A multi-result triage asks for the keyed form and interpolates exactly the labels this
/// triage offered — the tail is discarded with the fork, so nothing here is cache-keyed.
#[test]
fn a_multi_result_gate_lists_every_offered_label_and_the_count() {
    let offered = ["r1".to_string(), "r2".to_string()];
    let instruction = retain_instruction(&offered, &[], Reader::Main, Framing::FirstPerson);
    assert!(instruction.contains("no others: r1, r2."), "{instruction}");
    assert!(
        instruction.contains("exactly 2 `results` entries"),
        "{instruction}"
    );
}

/// Both shapes bound the ask to *this* triage. Every prior `tool_result` on the canonical
/// wire carries a `[seq]` block-addressing tag in the same bracket syntax the result
/// headers once used for call ids, and an unbounded "every [call id] above" was read by a live model
/// as "re-triage the whole transcript" — twenty-odd entries keyed by `[seq]` tags and
/// invented ids, with the triage's own id missing, so coverage failed and the triage
/// hard-errored (docs/triage.md).
#[test]
fn every_retain_ask_excludes_the_transcript_behind_it() {
    for offered in [
        vec!["r1".to_string()],
        vec!["r1".to_string(), "r2".to_string()],
    ] {
        let instruction = retain_instruction(&offered, &[], Reader::Main, Framing::FirstPerson);
        assert!(
            instruction.contains("Nothing earlier in this conversation is being triaged"),
            "the ask must say what is out of scope: {instruction}"
        );
        assert!(
            instruction.contains("[n] tags are not result labels"),
            "and must disown the block-addressing tags: {instruction}"
        );
    }
}

/// The verdict's two halves are asked to agree with each other, not merely with the raw.
/// A `note` can state a fact that is true of the raw and unsupported by anything that
/// survives onto the wire — a multi-match search kept down to one line, with the note
/// describing a different match. A reader who catches that once re-verifies by hand from
/// then on, so the cost of an unauditable summary is paid on every later result too
/// (docs/triage.md).
#[test]
fn the_ask_grounds_the_note_in_the_kept_lines_and_flags_a_partial_enumeration() {
    for offered in [
        vec!["only".to_string()],
        vec!["a".to_string(), "b".to_string()],
    ] {
        let instruction = retain_instruction(&offered, &[], Reader::Main, Framing::FirstPerson);
        assert!(
            instruction.contains("Ground it in\n              the lines you kept"),
            "the note must be answerable from what survives: {instruction}"
        );
        assert!(
            instruction.contains("that line belongs in `keep`"),
            "a claim about a line requires keeping that line: {instruction}"
        );
        assert!(
            instruction.contains("keep\n              the enumeration whole"),
            "a partial keep of an enumeration must be said, not implied: {instruction}"
        );
    }
}

/// The ask opens on a banner and the imperative, and closes on the imperative. Every other
/// section of a triage view carries a banner; the ask carrying none left the only
/// *instruction* in the view reading as a trailing note. A session answered 29% of its
/// triages with a work tool instead of a verdict (journal/2026-08-07-session-key-review.md).
#[test]
fn the_ask_is_bannered_and_opens_and_closes_on_the_imperative() {
    let instruction = retain_instruction(
        &["call_x".to_string()],
        &[],
        Reader::Main,
        Framing::FirstPerson,
    );
    let mut lines = instruction.lines();
    assert_eq!(lines.next(), Some(ASK_BANNER), "{instruction}");
    assert!(
        instruction
            .lines()
            .nth(2)
            .is_some_and(|l| l.starts_with("Call `retain` now.")),
        "the imperative leads, not a description: {instruction}"
    );
    assert!(instruction.ends_with("Call `retain`."), "{instruction}");
}

/// The turn is stated to be a triage turn, and the closed toolbox is stated as fact — a
/// non-`retain` call really is answered with an error and the ask really does come back
/// (`extend_with_retry`), so the sentence is true as written rather than a discouragement.
#[test]
fn the_ask_says_this_turn_is_not_the_task() {
    let instruction = retain_instruction(
        &["call_x".to_string()],
        &[],
        Reader::Main,
        Framing::FirstPerson,
    );
    assert!(
        instruction.contains("not a work turn"),
        "the ask must disown the task: {instruction}"
    );
    assert!(
        instruction.contains("No other tool will run until you have called `retain`"),
        "and state the closed toolbox non-optionally: {instruction}"
    );
}

/// A rider's field row rides inside the block, and only when its section is above. An
/// absent rider must leave no trace — an unconditional mention would ask about a section
/// the model can't see.
#[test]
fn rider_fields_appear_only_when_their_section_does() {
    let offered = ["call_x".to_string()];
    let bare = retain_instruction(&offered, &[], Reader::Main, Framing::FirstPerson);
    assert!(bare.contains("  keep "), "keep always rides: {bare}");
    for absent in ["surfaced", "inbox", "background"] {
        assert!(
            !bare.contains(absent),
            "{absent} named with no section: {bare}"
        );
    }
    let with_roster = retain_instruction(
        &offered,
        &[SURFACE_FIELD],
        Reader::Main,
        Framing::FirstPerson,
    );
    assert!(with_roster.contains("  surfaced "), "{with_roster}");
    assert!(!with_roster.contains("  inbox "), "{with_roster}");
}

/// The roster names each item **and its path**, so it reads as a file to open rather than
/// a capability to call — a session met a listed skill as a bare `[ferritin]` and built a
/// tool call out of the name (journal/2026-08-07-session-key-review.md).
///
/// It does *not* carry the summary. That is a measured call, not an aesthetic one: the
/// summaries are a quarter of a session's uncached fork tail and moved the first-attempt
/// failure rate not at all (journal/2026-08-07-triage-ask-measured.md).
#[test]
fn the_roster_gives_each_item_a_path_but_not_its_summary() {
    let roster = vec![listed("ferritin")];
    let mut view = String::new();
    append_roster(&mut view, &roster);
    assert!(view.contains("<ferritin"), "named: {view}");
    assert!(
        view.contains("path=\"skills/ferritin.md\""),
        "and located: {view}"
    );
    assert!(
        !view.contains("what ferritin does"),
        "the summary is the part that costs and doesn't pay: {view}"
    );
    assert!(!view.contains("[ferritin]"), "not the bare name: {view}");
}

/// `intent_feedback` is the one channel from triage back to the agent about the *intent*
/// rather than the output (docs/triage.md). It is optional in both answer shapes
/// and lands on the verdict beside the note, so it reaches the wire under the `⟢` sign.
#[test]
fn intent_feedback_is_carried_in_both_answer_shapes_and_absent_by_default() {
    let one = ["only".to_string()];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "keep": "all", "note": "3 warnings", "intent_feedback": "it said what the call does, not what you needed" }),
        ),
        &one,
    )
    .unwrap();
    assert_eq!(
        p.results[0].intent_feedback.as_deref(),
        Some("it said what the call does, not what you needed")
    );

    let p = parse_retain(&retain_completion(RETAIN, json!({ "keep": "all" })), &one).unwrap();
    assert_eq!(p.results[0].intent_feedback, None);

    let two = ["a".to_string(), "b".to_string()];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "result": "a", "keep": "none", "intent_feedback": "no question to keep against" },
                { "result": "b", "keep": "all" }
            ] }),
        ),
        &two,
    )
    .unwrap();
    assert_eq!(
        p.results[0].intent_feedback.as_deref(),
        Some("no question to keep against")
    );
    assert_eq!(p.results[1].intent_feedback, None);
}

/// The ask scopes `intent_feedback` to the channel and not the task, and asks the note to
/// say when the output lacks what the intent wanted rather than summarize around it.
#[test]
fn the_ask_scopes_intent_feedback_to_the_intent_and_prefers_it_didnt() {
    let instruction = retain_instruction(
        &["only".to_string()],
        &[],
        Reader::Main,
        Framing::FirstPerson,
    );
    assert!(instruction.contains("intent_feedback"), "{instruction}");
    assert!(
        instruction.contains("about the intent, never about the task"),
        "{instruction}"
    );
    assert!(
        instruction
            .contains("say that rather than\n              summarizing what it does contain"),
        "{instruction}"
    );
    let schema = retain_tool();
    let props = &schema["input_schema"]["properties"];
    assert!(props["intent_feedback"].is_object());
    assert!(props["results"]["items"]["properties"]["intent_feedback"].is_object());
}

/// The ask quotes the standing title and says to keep it unless the purpose
/// moved; with none, it says so — the two tails differ only there.
#[test]
fn title_instruction_quotes_the_standing_title() {
    let fresh = title_instruction(None);
    assert!(fresh.contains("no title yet"), "{fresh}");
    let kept = title_instruction(Some("Session names"));
    assert!(
        kept.contains("The current title is: Session names"),
        "{kept}"
    );
    assert!(kept.contains("Keep it verbatim"), "{kept}");
}

/// Who answers decides what the note is (docs/triage.md): the agent's own model
/// writes a note to self; a separate triage model writes a report that says what the
/// output shows and never what to do next. Both keep the grounding rule.
#[test]
fn a_separate_triage_model_is_asked_for_a_report() {
    let own = retain_instruction(&["r1".to_string()], &[], Reader::Main, Framing::FirstPerson);
    let separate = retain_instruction(
        &["r1".to_string()],
        &[],
        Reader::Triage,
        Framing::ThirdPerson,
    );
    assert!(own.contains("note to self"), "{own}");
    assert!(separate.contains("A report for the agent"), "{separate}");
    assert!(
        separate.contains("not what to do about it"),
        "a report names facts, not next steps: {separate}"
    );
    assert!(!separate.contains("note to self"), "{separate}");
    for ask in [&own, &separate] {
        assert!(ask.contains("puts that line in") || ask.contains("belongs in `keep`"));
    }
}
