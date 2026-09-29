//! An `ask` as the transcript draws it (docs/tui.md): the question is the assistant's
//! prose with an *asked* mark, and the reply is the operator's message. The call and
//! its result are how the loop and the model exchange them; the operator reads a turn
//! ending and their own answer.

use super::*;

/// The nodes for one `ask` call: the question, then — once its result is committed —
/// the reply, or a note that there was none. `question` is
/// [`crate::supervisor::ask_of_call`]'s `(text, n, of)`; `result` is the entry carrying
/// the call's result.
///
/// A reply is a result whose entry carries a `submission_id`; the error results an
/// interrupt or a malformed call produce carry none. The reply is attributed with the
/// `source:` prefix a sourced `User` entry gets, and an operator's has none.
pub(super) fn ask_nodes(
    (question, n, of): (String, usize, usize),
    result: Option<&LogEntry>,
) -> Vec<DocumentNode<'static>> {
    let mark = if of > 1 {
        format!("asked · question {n} of {of}")
    } else {
        "asked".to_string()
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
        style: SpanStyle::Comment,
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
            let text = match &entry.source {
                Some(source) => format!("{source}: {}", raw.content),
                None => raw.content.clone(),
            };
            nodes.push(DocumentNode::entry(EntryKind::UserMessage, markdown(&text)));
        } else {
            nodes.push(annotation_line("· ", "not answered"));
        }
    }
    nodes
}
