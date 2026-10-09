//! Why a provider ended its output, in one vocabulary across dialects
//! (docs/transport.md). No dialect's spelling is the harness's: each assembler maps
//! its provider's words onto [`FinishReason`] here, in one table per dialect, and
//! keeps the provider's own word beside it for the record.

#[cfg(test)]
mod test;

/// The ending, named for what the harness does about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    /// The model ended its turn.
    Finished,
    /// The model ended to call tools.
    ToolCalls,
    /// The output cap cut the response.
    OutputCap,
    /// The context window cut the response.
    ContextWindow,
    /// A stop sequence matched.
    StopSequence,
    /// The model declined to answer.
    Refused,
    /// A moderation layer cut the output.
    Filtered,
    /// The provider paused a long-running turn for the client to continue.
    Paused,
    /// A word this build does not map.
    Other,
}

/// A finish: the neutral reason and the provider's own word for it. The word is
/// what the request's `Ended` stage records (docs/log-and-projection.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    pub reason: FinishReason,
    pub raw: String,
}

impl Finish {
    /// A Messages `stop_reason`.
    pub fn messages(raw: &str) -> Self {
        let reason = match raw {
            "end_turn" => FinishReason::Finished,
            "tool_use" => FinishReason::ToolCalls,
            "max_tokens" => FinishReason::OutputCap,
            "model_context_window_exceeded" => FinishReason::ContextWindow,
            "stop_sequence" => FinishReason::StopSequence,
            "refusal" => FinishReason::Refused,
            "pause_turn" => FinishReason::Paused,
            _ => FinishReason::Other,
        };
        Self::new(reason, raw)
    }

    /// A Responses terminal `status` and its `incomplete_details.reason`. The
    /// status says only completed or incomplete, so a completed response that
    /// carries a call is the tool-call ending. The raw word is the status, then
    /// the reason after a colon when there is one.
    pub fn responses(status: &str, incomplete: Option<&str>, has_calls: bool) -> Self {
        let reason = match (status, incomplete) {
            ("completed", _) if has_calls => FinishReason::ToolCalls,
            ("completed", _) => FinishReason::Finished,
            ("incomplete", Some("max_output_tokens")) => FinishReason::OutputCap,
            ("incomplete", Some("content_filter")) => FinishReason::Filtered,
            _ => FinishReason::Other,
        };
        let raw = match incomplete {
            Some(why) => format!("{status}:{why}"),
            None => status.to_string(),
        };
        Self::new(reason, raw)
    }

    /// A Completions `finish_reason`. A call under `stop` is still a call: some
    /// servers end a tool turn that way.
    pub fn completions(raw: &str, has_calls: bool) -> Self {
        let reason = match raw {
            "stop" | "tool_calls" | "function_call" if has_calls => FinishReason::ToolCalls,
            "stop" => FinishReason::Finished,
            "tool_calls" | "function_call" => FinishReason::ToolCalls,
            "length" => FinishReason::OutputCap,
            "content_filter" => FinishReason::Filtered,
            _ => FinishReason::Other,
        };
        Self::new(reason, raw)
    }

    fn new(reason: FinishReason, raw: impl Into<String>) -> Self {
        Self {
            reason,
            raw: raw.into(),
        }
    }
}
