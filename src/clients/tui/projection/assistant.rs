//! An assistant entry's nodes (docs/tui.md): the record-only mark, its thinking, its prose
//! with the artifacts it linked, and a run per call it made.

use super::*;
use crate::record::log::Seq;

/// The nodes one assistant entry contributes, in reading order.
pub(super) fn assistant_nodes(
    log: &[LogEntry],
    entry: &LogEntry,
    index: &CallIndex,
    view: &ViewState,
    gate: Gate<'_>,
    artifact_kinds: &BTreeMap<Coordinate, Kind>,
) -> Vec<DocumentNode<'static>> {
    let Payload::Assistant { output, excluded } = &entry.payload else {
        return Vec::new();
    };
    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();
    nodes.extend(record_only_mark(excluded.as_ref()));
    nodes.extend(thinking_item(entry.seq, output));
    nodes.extend(prose_entry(entry.seq, output, artifact_kinds));
    nodes.extend(call_nodes(log, index, view, gate, output));
    nodes
}

/// The marker over a record-only entry (docs/transport.md): the stream died before
/// finalizing, or the model finalized with nothing in it, so nothing below
/// is in the model's context. The content still renders — it may be the
/// only copy of what the model produced — but the marker keeps it from
/// reading as a committed turn.
fn record_only_mark(excluded: Option<&Exclusion>) -> Option<DocumentNode<'static>> {
    match excluded {
        Some(Exclusion::Upstream(why)) => Some(annotation_line(
            "⚠ stream aborted — recorded, not in context · ",
            why.why(),
        )),
        Some(Exclusion::Empty) => Some(annotation_line(
            "⚠ empty completion — recorded, not in context, asked again",
            "",
        )),
        None => None,
    }
}

/// The entry's thinking as one foldable item, keyed by the entry it came from.
fn thinking_item(seq: Seq, output: &AssistantOutput) -> Option<DocumentNode<'static>> {
    let thinking: Vec<ContentBlock> = match output {
        AssistantOutput::Messages(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                MessagesOutputBlock::Thinking { text, signature } => Some(ContentBlock::Thinking {
                    text: text.clone(),
                    signature: signature.clone(),
                }),
                MessagesOutputBlock::RedactedThinking { data } => {
                    Some(ContentBlock::RedactedThinking { data: data.clone() })
                }
                _ => None,
            })
            .collect(),
        AssistantOutput::Responses(_) => Vec::new(),
        // Completions reasoning has no signature the TUI could show;
        // the readable text is the display heuristic's.
        AssistantOutput::Completions(message) => message
            .reasoning_text()
            .into_iter()
            .map(|text| ContentBlock::Thinking {
                text: text.to_string(),
                signature: String::new(),
            })
            .collect(),
    };
    let node = thinking_entry(&thinking)?;
    // One foldable item, keyed by the entry it came from (docs/tui.md,
    // "Detail by recency").
    let words = thinking
        .iter()
        .map(|block| match block {
            ContentBlock::Thinking { text, .. } => crate::clients::tui::detail::word_count(text),
            _ => 0,
        })
        .sum();
    Some(DocumentNode::ThinkingItem {
        key: format!("thinking:{seq}").into(),
        words,
        entry: Box::new(node),
    })
}

/// The entry's prose, followed by a notice per artifact fence the bridge received.
/// The page renders in the web client; the terminal keeps the source above.
fn prose_entry(
    seq: Seq,
    output: &AssistantOutput,
    artifact_kinds: &BTreeMap<Coordinate, Kind>,
) -> Option<DocumentNode<'static>> {
    let prose = output.text().join("\n");
    if prose.trim().is_empty() {
        return None;
    }
    let mut body = markdown(&prose);
    // The entry's artifacts, in order, as the bridge received them.
    let entry_artifacts = Coordinate {
        seq,
        text_block: 0,
        fence: 0,
    }..=Coordinate {
        seq,
        text_block: usize::MAX,
        fence: usize::MAX,
    };
    for kind in artifact_kinds.range(entry_artifacts).map(|(_, kind)| kind) {
        let what = match kind {
            Kind::Html => "artifact",
            Kind::Mermaid => "diagram",
        };
        body.push(DocumentNode::Paragraph {
            spans: vec![Span::plain(format!(
                "{what} · renders in the web client — source above"
            ))],
        });
    }
    Some(DocumentNode::entry(EntryKind::AssistantMessage, body))
}

/// A run of nodes per call the entry made.
fn call_nodes(
    log: &[LogEntry],
    index: &CallIndex,
    view: &ViewState,
    gate: Gate<'_>,
    output: &AssistantOutput,
) -> Vec<DocumentNode<'static>> {
    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();
    let calls = output.tool_calls().unwrap_or_default();
    for (i, call) in calls.iter().enumerate() {
        // A question put to the operator is drawn as the assistant's prose
        // and the operator's reply, not as a call (docs/tui.md).
        if let Some(question) = crate::tools::process_monitor::ask_of_call(&calls, i) {
            let reply = index.result_entry(log, call.id.0.as_str());
            let state = ask::AskState::pending(call.id.0.as_str(), gate.ask);
            nodes.extend(ask::ask_nodes(question, reply, state));
            continue;
        }
        let result = index.result(log, call.id.0.as_str());
        let is_pending = result.is_none() && gate.call == Some(call.id.0.as_str());
        // A resolved consolidate collapses to its name: its input is the
        // summary rendered below in full, and its result is a
        // constant acknowledgement (docs/consolidate.md). A pending,
        // gated, or failed one renders whole — an error is information.
        if call.name == crate::record::names::tools::CONSOLIDATE
            && let Some((_, raw, _)) = result
            && !raw.is_error
        {
            nodes.push(collapsed_consolidate(call));
            continue;
        }
        // The call and everything that reads as part of it are one item,
        // so the renderer can fold it by distance from the tail
        // (docs/tui.md, "Detail by recency").
        let outer = std::mem::take(&mut nodes);
        nodes.push(call_entry(call));
        // Reading order is call → what the spotter made of it → what came
        // back. The harness's own lines about a call are their own entries
        // (`EntryKind::Annotation`), never children of the call: they are a
        // different rung's judgment *about* it, and keeping them separate is
        // what stops any presentation choice made for the call's content
        // from reaching them (docs/tui.md).
        //
        // Every judged call carries its verdict, not just a held one: a
        // cleared call's reason is the only evidence of what the spotter
        // thought, and it is the majority of verdicts. The committed `Spot`
        // is the source; `pending_reason` (from `/state`) covers only the
        // window where a gate has registered but its log entry hasn't
        // reached this client yet, so the two never both render.
        //
        // A call that reached this client by escalation names its escalator, and the
        // escalator's note takes the spotter's place when it says something: it is
        // written for whoever decides the call now, where the spotter's account is the
        // older judgment (docs/permissions.md). A noteless escalation keeps the
        // spotter's account beneath the attribution.
        let escalation = if is_pending {
            gate.escalations.last()
        } else {
            None
        };
        let escalation_note = escalation
            .and_then(|hop| hop.note.as_deref())
            .filter(|note| !note.trim().is_empty());
        if let Some(hop) = escalation {
            nodes.push(escalation_line(hop, escalation_note));
        }
        if escalation_note.is_none() {
            if let Some(spot) = index.spot(log, call.id.0.as_str()) {
                nodes.push(spotter_line(Some(spot.stance), &spot.reason));
            } else if is_pending && let Some(reason) = gate.reason {
                nodes.push(spotter_line(None, reason));
            }
        }
        if is_pending {
            // A bound `agents__decide` shows the child's held move
            // beside the approval (docs/permissions.md): the logged call
            // line is the model's, and this names what it acts on.
            if let Some(bound) = gate.acting_on {
                nodes.push(bound_action(bound));
            }
            nodes.push(gate_affordance());
        }
        if let Some((seq, raw, wire)) = result {
            nodes.push(result_entry(call, seq, raw, wire, view));
            // The triager's note is *about* the result, not part of it
            // — the same rung distinction the spotter's verdict gets — so
            // it is its own entry rather than a trailing line boxed in
            // with the output (docs/triage.md).
            nodes.extend(selection_note(call, raw, wire, view));
        }
        let item = std::mem::replace(&mut nodes, outer);
        nodes.push(call_item(
            call,
            result,
            is_pending,
            index.spot(log, call.id.0.as_str()),
            item,
        ));
    }
    nodes
}
