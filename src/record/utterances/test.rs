use super::*;
use crate::record::log::{
    AssistantOutput, GateDecision, ReferenceDefinition, Seq, ToolCall, UserSegment, WireState,
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

    /// The session's reply, prose and calls.
    fn reply(&mut self, prose: &str, calls: Vec<ToolCall>) -> &mut Self {
        let text = match prose {
            "" => vec![],
            prose => vec![ContentBlock::Text(prose.into())],
        };
        self.push(Payload::assistant(AssistantOutput::from_legacy_messages(
            vec![],
            text,
            calls,
        )))
    }

    fn entries(&self) -> &[LogEntry] {
        &self.0
    }
}

fn operator(text: &str, answers: Option<&str>) -> Utterance {
    Utterance::new(Speaker::Operator, text, answers)
}

/// No source, a web client and an MCP client are the operator; a `session[…]` source is
/// another session, by id, whichever project it names.
#[test]
fn a_source_names_its_speaker() {
    let mut log = Log::default();
    log.submitted("from the TUI", None)
        .submitted("from the browser", Some("web"))
        .submitted("from an MCP client", Some("mcp[claude-code]"))
        .submitted("from the lead", Some("session[lead]"))
        .submitted("from afar", Some("session[far in /work/other]"));
    let speakers: Vec<Speaker> = utterances(log.entries())
        .into_iter()
        .map(|u| u.speaker)
        .collect();
    assert_eq!(
        speakers,
        vec![
            Speaker::Operator,
            Speaker::Operator,
            Speaker::Operator,
            Speaker::Session("lead".into()),
            Speaker::Session("far".into()),
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
        utterances(log.entries()),
        vec![operator("ask @helper to review the gallery", None)]
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
    let texts: Vec<String> = utterances(log.entries())
        .into_iter()
        .map(|u| u.text)
        .collect();
    assert_eq!(texts, vec!["load the gallery", "use the docs tool instead"]);
}

/// A turn carries the prose of the session's last reply since the previous turn, and
/// none when the session wrote nothing in between.
#[test]
fn a_turn_answers_the_sessions_last_prose() {
    let mut log = Log::default();
    log.submitted("look at the gallery", None)
        .reply("Reading it now.", vec![])
        .reply("Shall I open it in Playwright?", vec![])
        .submitted("yes", None)
        .submitted("and search the docs too", None);
    assert_eq!(
        utterances(log.entries()),
        vec![
            operator("look at the gallery", None),
            operator("yes", Some("Shall I open it in Playwright?")),
            operator("and search the docs too", None),
        ]
    );
}

/// A reply to an `ask` is the replier's utterance, and answers the question.
#[test]
fn an_ask_reply_answers_its_question() {
    let ask = ToolCall::new(
        ASK,
        "toolu_ask",
        serde_json::json!({ "question": "Which model for the helper?" }),
    );
    let mut log = Log::default();
    log.reply("I'll need a helper.", vec![ask]).push_submitted(
        Payload::tool_result(
            "toolu_ask",
            RawResult::text("deepseek-flash"),
            WireState::Raw,
        ),
        Some("session[lead]"),
    );
    assert_eq!(
        utterances(log.entries()),
        vec![Utterance::new(
            Speaker::Session("lead".into()),
            "deepseek-flash",
            Some("Which model for the helper?"),
        )]
    );
}

/// The text keeps its head and the session's prose its tail, each cut on a character
/// boundary and marked.
#[test]
fn clips_fall_on_character_boundaries() {
    let long = "é".repeat(TEXT_CLIP + 10);
    let utterance = Utterance::new(
        Speaker::Operator,
        &long,
        Some(&"ü".repeat(ANSWERS_CLIP + 3)),
    );
    assert_eq!(utterance.text, format!("{} …", "é".repeat(TEXT_CLIP)));
    assert_eq!(
        utterance.answers.as_deref(),
        Some(format!("… {}", "ü".repeat(ANSWERS_CLIP)).as_str())
    );
    let short = Utterance::new(Speaker::Operator, "  ok  ", Some(" fine "));
    assert_eq!(short.text, "ok");
    assert_eq!(short.answers.as_deref(), Some("fine"));
}
