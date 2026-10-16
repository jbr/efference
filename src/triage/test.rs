use super::title::{parse_title, title_instruction};
use super::*;
use crate::api::Usage;
use crate::log::Triage;
use crate::project::{
    ApiBlock, ApiMessage, BreakpointPlacement, ImagePlacement, Role, encode_messages, project_with,
};

/// The Messages-wire view of a fork tail, for assertions written against blocks.
fn wire(turns: &[WireTurn]) -> Vec<ApiMessage> {
    encode_messages(
        turns,
        ImagePlacement::default(),
        BreakpointPlacement::default(),
    )
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

/// The id of the result a single-result gate is triaging. Arbitrary — what matters is
/// that exactly one id is offered, which is the condition under which `parse_retain`
/// adopts the id-free shapes — the flat `keep`/`note`, or a lone mis-keyed entry.
const CALL_ID: &str = "toolu_x";

/// A `Completion` whose assistant turn made a single `retain` call carrying
/// `input` — the shape `Triager` asks for and `parse_retain` reads.
fn retain_completion(name: &str, input: Value) -> Completion {
    calls_completion(vec![ToolCall::new(name, "r", input)])
}

/// A `Completion` whose assistant turn made every call in `calls`, in parallel.
fn calls_completion(calls: Vec<ToolCall>) -> Completion {
    Completion {
        receipt: Default::default(),
        assistant: Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(vec![], vec![], calls),
            upstream_failure: None,
        },
        id: None,
        model: "test".into(),
        finish: None,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        cache_miss: None,
        failure_class: crate::api::FailureClass::Transient,
    }
}

fn entry(seq: u64, payload: Payload) -> LogEntry {
    use crate::log::Timestamp;
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(1000 + seq),
        submission_id: None,
        source: None,
        payload,
    }
}

mod ask;
mod fork;
mod lineage;
mod parse;
mod validation;
