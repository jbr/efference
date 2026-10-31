//! What a transcript handle names, for a reply that says which question it answers
//! (docs/control-api.md): the call at the handle, and for an `ask`, what became of it.
//! Read across the whole log, as the conversation's outcomes are (`index.rs`).

use super::index::Index;
use super::*;
use crate::protocol::transcript::Handle;

/// The call a reference resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Referenced {
    /// No call has that handle or id.
    Nothing,
    /// A call that is not a question.
    Call { handle: String, tool: String },
    /// A question, and its answer once one is recorded.
    Question {
        handle: String,
        question: String,
        answer: Option<Answer>,
    },
}

/// A question's recorded result: who sent the reply, and what the result says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Answer {
    /// The reply's `source`, `None` for the operator.
    pub(crate) source: Option<String>,
    /// The result's text, kept to one line.
    pub(crate) text: String,
    /// Whether the ask ended without a reply (an interrupt, a malformed call).
    pub(crate) is_error: bool,
}

/// The most of a recorded answer a refusal quotes.
const ANSWER_CAP: usize = 200;

/// The call id at `handle`: the one call an entry issued for a bare seq, the n-th for
/// `seq.n`. `None` when no assistant entry there issued that call.
pub(crate) fn call_id_at(log: &[LogEntry], handle: Handle) -> Option<String> {
    let index = Index::new(log);
    let entry = index.entry(handle.seq)?;
    let Payload::Assistant {
        output,
        excluded: None,
    } = &entry.payload
    else {
        return None;
    };
    let (_, calls) = assistant(output);
    let call = match handle.call {
        None if calls.len() == 1 => calls.first(),
        None => None,
        Some(n) => calls.get(n - 1),
    }?;
    Some(call.id.0.clone())
}

/// What the call `call_id` is.
pub(crate) fn referenced(log: &[LogEntry], call_id: &str) -> Referenced {
    let index = Index::new(log);
    let Some(call) = index.call(call_id) else {
        return Referenced::Nothing;
    };
    let Some((question, _, _)) = &call.ask else {
        return Referenced::Call {
            handle: call.handle.clone(),
            tool: crate::mcp::display_name(&call.call.name),
        };
    };
    let answer = index
        .result(call_id)
        .and_then(|entry| match &entry.payload {
            Payload::ToolResult { raw, .. } => Some(Answer {
                source: entry.source.clone(),
                text: clip(raw.content.trim(), ANSWER_CAP),
                is_error: raw.is_error,
            }),
            _ => None,
        });
    Referenced::Question {
        handle: call.handle.clone(),
        question: question.clone(),
        answer,
    }
}

#[cfg(test)]
mod test;
