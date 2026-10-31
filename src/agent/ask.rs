//! The loop-owned `ask` (docs/process-monitor.md): a question to the operator that blocks
//! like a tool call. The reply is the next turn a client submits, delivered here as the
//! call's result.

use super::*;

/// Where one `ask` falls among the asks of a completion: they are put to the operator
/// one at a time, each announced as question `n` of `of`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AskPosition {
    pub n: usize,
    pub of: usize,
}

impl AskPosition {
    /// The position of `calls[index]` among the asks of `calls` (1 of 1 for a call
    /// that is not one, which nothing reads).
    pub(super) fn of_call(calls: &[ToolCall], index: usize) -> Self {
        let is_ask = |c: &&ToolCall| c.name == crate::log::ASK;
        Self {
            n: calls[..index].iter().filter(is_ask).count() + 1,
            of: calls.iter().filter(is_ask).count(),
        }
    }
}

/// Who answered: the submission the reply came from, and its client's label. The
/// result entry carries both, as a `User` entry does.
pub(super) struct Reply {
    pub submission_id: String,
    pub source: Option<String>,
}

/// Run one `ask`: register the question, wait for a submitted turn or an interrupt, and
/// return the result. A reply is `Some`; an interrupt or a malformed call is an error
/// result with none.
pub(super) async fn ask_result(
    shared: &Arc<Session>,
    call: &ToolCall,
    at: AskPosition,
) -> (RawResult, Option<Box<Reply>>) {
    let Some(question) = call
        .input
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty())
    else {
        return (RawResult::error("ask requires a `question`"), None);
    };
    // Armed before the latch is read, for the same lost-wakeup protection `wait` has.
    let interrupt = shared.interrupt_signal();
    let interrupted = interrupt.notified();
    tokio::pin!(interrupted);
    if interrupted.as_mut().enable() || shared.interrupt_pending() {
        return (RawResult::error("ask interrupted by user"), None);
    }
    let mut wait = shared.begin_ask(&call.id.0, question.to_string(), at.n, at.of);
    tokio::select! {
        reply = wait.reply() => match reply {
            Some(turn) => {
                let result = RawResult::text(turn.reply_text());
                let reply = Box::new(Reply { submission_id: turn.submission_id, source: turn.source });
                (result, Some(reply))
            }
            None => (RawResult::error("ask ended without a reply"), None),
        },
        _ = &mut interrupted => (RawResult::error("ask interrupted by user"), None),
    }
}
