use super::*;
use crate::record::log::{
    AssistantOutput, GateDecision, ReferenceDefinition, Seq, Timestamp, ToolCall, UserSegment,
    WireState,
};

/// A log under construction: each entry takes the next seq.
#[derive(Default)]
struct Log(Vec<LogEntry>);

impl Log {
    /// An entry the loop wrote: no submission behind it.
    fn push(&mut self, payload: Payload) -> &mut Self {
        let seq = self.0.len() as Seq;
        self.0.push(LogEntry::new(seq, payload));
        self
    }

    /// An entry a submission produced, under `source`.
    fn push_submitted(&mut self, payload: Payload, source: Option<&str>) -> &mut Self {
        let seq = self.0.len() as Seq;
        self.0.push(
            LogEntry::new(seq, payload)
                .with_submission_id(Some(format!("sub-{seq}")))
                .with_source(source.map(str::to_string)),
        );
        self
    }

    /// A turn a client submitted, under `source`.
    fn submitted(&mut self, text: &str, source: Option<&str>) -> &mut Self {
        self.push_submitted(Payload::user(text), source)
    }

    /// The agent's reply, prose blocks and calls.
    fn reply(&mut self, prose: &[&str], calls: Vec<ToolCall>) -> &mut Self {
        let text = prose
            .iter()
            .map(|prose| ContentBlock::Text((*prose).into()))
            .collect();
        self.push(Payload::assistant(AssistantOutput::from_legacy_messages(
            vec![],
            text,
            calls,
        )))
    }

    fn entries(&self) -> &[LogEntry] {
        &self.0
    }

    /// What was said, as the session `s` folds it.
    fn said(&self) -> Vec<Utterance> {
        utterances("s", &self.0).said
    }
}

fn operator(text: &str) -> Utterance {
    Utterance::new(Speaker::Operator, text)
}

fn agent(text: &str) -> Utterance {
    Utterance::new(Speaker::Agent, text)
}

/// No source, a web client and an MCP client are the operator; a `session[…]` source is
/// a supervising agent, by id, whichever project it names.
#[test]
fn a_source_names_its_speaker() {
    let mut log = Log::default();
    log.submitted("from the TUI", None)
        .submitted("from the browser", Some("web"))
        .submitted("from an MCP client", Some("mcp[claude-code]"))
        .submitted("from the lead", Some("session[lead]"))
        .submitted("from afar", Some("session[far in /work/other]"));
    let speakers: Vec<Speaker> = log.said().into_iter().map(|u| u.speaker).collect();
    assert_eq!(
        speakers,
        vec![
            Speaker::Operator,
            Speaker::Operator,
            Speaker::Operator,
            Speaker::Supervisor("lead".into()),
            Speaker::Supervisor("far".into()),
        ]
    );
}

/// A structured turn reads as its readable form: prose with each reference as the
/// submitter would read it.
#[test]
fn a_documents_text_is_its_readable_form() {
    let mut document = UserContent::text("ask ");
    document.content.push(UserSegment::Reference("r1".into()));
    document
        .content
        .push(UserSegment::Text(" to review the gallery".into()));
    document
        .references
        .insert("r1".into(), ReferenceDefinition::agent("r1", "helper"));
    let mut log = Log::default();
    log.push(Payload::User(vec![ContentBlock::Document(document)]));
    assert_eq!(
        log.said(),
        vec![operator("ask @helper to review the gallery")]
    );
}

/// The kickoff, a hand-off brief, and a redirect the permission review wrote are the
/// harness's, not anyone's words. A redirect a person wrote is theirs.
#[test]
fn harness_written_user_turns_are_left_out() {
    let mut log = Log::default();
    log.submitted("Greet the operator.", Some(sources::KICKOFF_SOURCE));
    log.submitted("Carry on with step 3.", Some("handoff[old-one]"));
    log.submitted("load the gallery", None);
    // The review's redirect: its decision first, then the loop's own user turn.
    log.push(Payload::Telemetry(Telemetry::GateDecision(
        GateDecision::new(
            "toolu_1",
            Approval::Redirect("Gated, and no synchronous reviewer is available.".into()),
            Some("review[autonomous]".into()),
        ),
    )));
    log.push(Payload::user(
        "Gated, and no synchronous reviewer is available.",
    ));
    // A person's redirect, decided at the TUI.
    log.push(Payload::Telemetry(Telemetry::GateDecision(
        GateDecision::new(
            "toolu_2",
            Approval::Redirect("use the docs tool instead".into()),
            None,
        ),
    )));
    log.push(Payload::user("use the docs tool instead"));
    let texts: Vec<String> = log.said().into_iter().map(|u| u.text).collect();
    assert_eq!(texts, vec!["load the gallery", "use the docs tool instead"]);
}

/// Every reply with prose is the agent's utterance, in order with the turns around it:
/// its text blocks joined, a reply of calls alone left out.
#[test]
fn every_reply_with_prose_is_the_agents() {
    let read = ToolCall::new("read", "toolu_r", serde_json::json!({ "path": "a" }));
    let mut log = Log::default();
    log.submitted("look at the gallery", None)
        .reply(&["Reading it now."], vec![read])
        .reply(&[], vec![])
        .reply(
            &["It renders.", "  Shall I open it in Playwright?  "],
            vec![],
        )
        .submitted("yes", None);
    assert_eq!(
        log.said(),
        vec![
            operator("look at the gallery"),
            agent("Reading it now."),
            agent("It renders.\n\nShall I open it in Playwright?"),
            operator("yes"),
        ]
    );
}

/// An `ask`'s question is the agent's text, after the reply's prose; the reply to it is
/// the replier's utterance.
#[test]
fn an_ask_is_the_agents_and_its_reply_the_repliers() {
    let ask = ToolCall::new(
        ASK,
        "toolu_ask",
        serde_json::json!({ "question": "Which model for the helper?" }),
    );
    let mut log = Log::default();
    log.reply(&["I'll need a helper."], vec![ask])
        .push_submitted(
            Payload::tool_result(
                "toolu_ask",
                RawResult::text("deepseek-flash"),
                WireState::Raw,
            ),
            Some("session[lead]"),
        )
        // A result nobody submitted is the tool's, not a reply.
        .push(Payload::tool_result(
            "toolu_other",
            RawResult::text("file contents"),
            WireState::Raw,
        ));
    assert_eq!(
        log.said(),
        vec![
            agent("I'll need a helper.\n\nWhich model for the helper?"),
            Utterance::new(Speaker::Supervisor("lead".into()), "deepseek-flash"),
        ]
    );
}

/// Nothing is cut: a grant at the end of a long message is kept, and so is the start of
/// a long reply.
#[test]
fn nothing_is_clipped() {
    let long = format!("{} Also: you may push to main.", "é".repeat(10_000));
    let reply = format!("Option 1 is to vendor it. {}", "ü".repeat(10_000));
    let mut log = Log::default();
    log.reply(&[&reply], vec![]).submitted(&long, None);
    assert_eq!(log.said(), vec![agent(&reply), operator(&long)]);
}

/// The tag id is the same for every fold of a session's log, appended to or not, and
/// differs between sessions: by id, or by when their logs began.
#[test]
fn the_tag_id_is_one_value_per_session() {
    let mut log = Log::default();
    log.submitted("fix the flaky test", None);
    let id = utterances("s", log.entries()).id;
    assert_eq!(id.len(), 8, "{id}");
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    assert_eq!(utterances("s", log.entries()).id, id, "the same log");
    log.reply(&["Found it."], vec![]).submitted("go on", None);
    assert_eq!(utterances("s", log.entries()).id, id, "an appended log");
    assert_ne!(utterances("t", log.entries()).id, id, "another session");
    let later = [log.0[0].clone().with_at(Timestamp(1_794_620_552_183))];
    assert_ne!(
        utterances("s", &later).id,
        id,
        "a log begun at another time"
    );
}
