//! An `ask` as the transcript draws it (docs/tui.md): the question is the assistant's
//! prose with an *asked* mark, and the reply is the operator's message. The call and
//! its result are how the loop and the model exchange them; the operator reads a turn
//! ending and their own answer.

use super::*;

/// Where one `ask` stands, as far as this client can observe it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AskState {
    /// The question the live `ask` topic names: the composer's send answers it.
    Current,
    /// Committed with no result while the topic names another ask of the same
    /// completion: put to the operator later.
    Upcoming,
    /// No result and no live topic to say more (a replay, a client that connected
    /// after the fact): drawn as merely asked, since nothing says otherwise.
    Asked,
}

impl AskState {
    /// The state of an ask with no committed result, given the live topic's call id.
    pub(super) fn pending(call_id: &str, live: Option<&str>) -> Self {
        match live {
            Some(live) if live == call_id => Self::Current,
            Some(_) => Self::Upcoming,
            None => Self::Asked,
        }
    }
}

/// The nodes for one `ask` call: the question, then — once its result is committed —
/// the reply, or a note that there was none. `question` is
/// [`crate::tools::process_monitor::ask_of_call`]'s `(text, n, of)`; `result` is the entry carrying
/// the call's result; `state` applies only while there is no result.
///
/// A reply is a result whose entry carries a `submission_id`; the error results an
/// interrupt or a malformed call produce carry none. The reply is attributed with the
/// `source:` prefix a sourced `User` entry gets, and an operator's has none.
pub(super) fn ask_nodes(
    (question, n, of): (String, usize, usize),
    result: Option<&LogEntry>,
    state: AskState,
) -> Vec<DocumentNode<'static>> {
    let place = if of > 1 {
        format!(" · question {n} of {of}")
    } else {
        String::new()
    };
    let (mark, style) = match (result, state) {
        (None, AskState::Current) => (format!("▸ answering{place}"), SpanStyle::Strong),
        (None, AskState::Upcoming) => (format!("next{place}"), SpanStyle::Comment),
        _ => (format!("asked{place}"), SpanStyle::Comment),
    };
    let mut body = markdown(&question);
    // The mark trails the question's last line when that is a paragraph, so it reads as
    // a note on the question and not as a block of its own.
    let mark = Span {
        text: if matches!(body.last(), Some(DocumentNode::Paragraph { .. })) {
            format!("  {mark}")
        } else {
            mark
        }
        .into(),
        style,
        action: None,
    };
    match body.last_mut() {
        Some(DocumentNode::Paragraph { spans }) => spans.push(mark),
        _ => body.push(DocumentNode::paragraph(vec![mark])),
    }
    let mut nodes = vec![DocumentNode::entry(EntryKind::AssistantMessage, body)];
    if let Some(entry) = result
        && let Payload::ToolResult { raw, .. } = &entry.payload
    {
        if entry.submission_id.is_some() {
            nodes.push(DocumentNode::entry(
                EntryKind::UserMessage,
                sourced_markdown(entry.source.as_deref(), &raw.content),
            ));
        } else {
            nodes.push(annotation_line("· ", "not answered"));
        }
    }
    nodes
}
