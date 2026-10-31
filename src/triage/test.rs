use super::title::{parse_title, title_instruction};
use super::*;
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

/// The id of the result a single-result triage is triaging. Arbitrary — what matters is
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
    Completion::new(AssistantOutput::from_legacy_messages(vec![], vec![], calls))
}

fn entry(seq: u64, payload: Payload) -> LogEntry {
    use crate::log::Timestamp;
    LogEntry::new(seq, payload).with_at(Timestamp(1000 + seq))
}

mod ask;
mod fork;
mod lineage;
mod parse;
mod result_view;
mod riders;
mod shared_bytes;
mod validation;
