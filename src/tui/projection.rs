//! Project the append-only log (plus the in-flight streaming tail) into a
//! renderable [`Document`] (docs/README.md — the transcript is another `project(log)`,
//! a sibling of the wire projection in `crate::project`).
//!
//! This mirrors one rule from the wire projection ([`crate::project`]): a tool
//! result shows its *selection* when present, else its raw content. Unlike the
//! wire projection it does not coalesce or elide — every entry renders, whole, so
//! the human sees the whole record (docs/positions.md).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::frontend::SubmittedTurn;
use crate::log::{
    AssistantOutput, ContentBlock, Exclusion, LogEntry, MessagesOutputBlock, Payload, RawResult,
    Summary, ToolCall, Triaged, WireState,
};
use crate::tui::artifacts::Coordinate;
use crate::tui::document::{
    DiffLine, DiffLineKind, Document, DocumentNode, EntryKind, Span, SpanStyle,
};
use crate::tui::markdown::MarkdownRenderer;

/// Fold the log + live tail into a flat sequence of transcript
/// [`Entry`](DocumentNode::Entry) nodes — one per top-level thing on the wire,
/// decorated for presentation (docs/tui.md). `view` carries the human's per-result
/// raw-vs-triaged flips across this rebuild.
///
/// `pending_call` is the `call_id` the agent is blocked on for approval. The loop
/// appends the assistant `tool_use` **before** it gates and one tool call runs per
/// turn (docs/README.md), so the pending call is always the tail call with no result.
///
/// `pending_turns` are just-submitted user messages awaiting the committed `User` log
/// entry that confirms them; they render after the committed log and live tail.
pub(crate) fn project(
    log: &[LogEntry],
    live: &LiveTail,
    view: &ViewState,
    gate: Gate<'_>,
    pending_turns: &[SubmittedTurn],
) -> Document<'static> {
    let mut index = CallIndex::default();
    for (idx, entry) in log.iter().enumerate() {
        index.absorb(idx, entry);
    }

    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();
    let artifact_urls = BTreeMap::new();
    for idx in 0..log.len() {
        nodes.extend(entry_nodes(log, idx, &index, view, gate, &artifact_urls));
    }
    nodes.extend(tail_nodes(live, view, pending_turns));
    Document::with_nodes(nodes)
}

/// The pending approval gate as the transcript sees it (docs/tui.md): the call the
/// loop is blocked on, and the spotter's reason when the client learned it from
/// `/state` before the `Spot` entry arrived. Ephemeral control state — it is not in
/// the log, so it travels alongside it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Gate<'g> {
    /// The `call_id` awaiting approval.
    pub call: Option<&'g str>,
    /// The `call_id` of the `ask` the live topic names — the question the composer's
    /// send answers. Not a gate, but ephemeral control state that changes how a
    /// committed call renders, so it travels with one (docs/tui.md).
    pub ask: Option<&'g str>,
    /// The reason to show beside it, when no committed `Spot` carries one.
    pub reason: Option<&'g str>,
    /// The leaf action a bound `agents__decide` authorizes (docs/permissions.md),
    /// shown beside the approval so the human sees the child's held move.
    pub acting_on: Option<&'g crate::tui::ActingOn>,
}

/// Where a call's *late-arriving* annotations live: `call_id` → the log index of
/// the entry carrying its result, and of the entry carrying the spotter's verdict.
/// Both land in a later entry than the call they are about, so pairing them with
/// the call needs this one indirection.
///
/// Holding indices rather than borrows is what lets the incremental projection
/// ([`Transcript`]) keep the index across rebuilds without borrowing the log.
///
/// It also places summaries. A summary is drawn where projection injects its
/// account — after the entry its range ends in — not where it was appended
/// (docs/summary.md). A range ending in a tool result ends in the entry that
/// made the call, since the result is drawn there.
#[derive(Debug, Default)]
pub(crate) struct CallIndex {
    result_at: HashMap<String, usize>,
    spot_at: HashMap<String, usize>,
    pending_calls: HashMap<u64, String>,
    verdict_at: HashMap<u64, usize>,
    /// `call_id` → the index of the entry that made the call.
    call_owner: HashMap<String, usize>,
    /// `seq` → the index of the entry whose nodes draw it.
    drawn_by: BTreeMap<u64, usize>,
    /// Entry index → the summaries drawn after its nodes, in log order.
    summaries_after: HashMap<usize, Vec<usize>>,
    /// Summary index → the entry index it is drawn after.
    summary_host: HashMap<usize, usize>,
}

impl CallIndex {
    /// Record what entry `idx` says about a call, returning the `call_id` it
    /// annotates — which is exactly the call whose entries must be rebuilt.
    /// Later entries overwrite earlier ones, so a re-judged call shows its latest
    /// verdict, matching the daemon's `latest_spot_reason` (which reads in reverse).
    pub(crate) fn absorb(&mut self, idx: usize, entry: &LogEntry) -> Option<String> {
        let drawn_by = match &entry.payload {
            Payload::ToolResult { call_id, .. } => {
                self.call_owner.get(&call_id.0).copied().unwrap_or(idx)
            }
            _ => idx,
        };
        self.drawn_by.insert(entry.seq, drawn_by);
        match &entry.payload {
            Payload::Assistant { output, .. } => {
                for call in output.tool_calls().unwrap_or_default() {
                    self.call_owner.insert(call.id.0, idx);
                }
                None
            }
            Payload::Summary(summary) => {
                let upto = summary.upto;
                if let Some((_, &host)) = self.drawn_by.range(..=upto).next_back() {
                    self.summaries_after.entry(host).or_default().push(idx);
                    self.summary_host.insert(idx, host);
                }
                None
            }
            Payload::ToolResult { call_id, wire, .. } => {
                self.result_at.insert(call_id.0.clone(), idx);
                if matches!(wire, WireState::Pending) {
                    self.pending_calls.insert(entry.seq, call_id.0.clone());
                }
                Some(call_id.0.clone())
            }
            Payload::ResultVerdict { for_result, wire } if !matches!(wire, WireState::Pending) => {
                let call = self.pending_calls.get(for_result)?.clone();
                self.verdict_at.entry(*for_result).or_insert(idx);
                Some(call)
            }
            Payload::Spot(spot) => {
                self.spot_at.insert(spot.call_id.0.clone(), idx);
                Some(spot.call_id.0.clone())
            }
            _ => None,
        }
    }

    /// The entry a summary at `idx` is drawn after, when its range ends in a
    /// committed entry: the one to re-project when the summary arrives.
    pub(crate) fn summary_host(&self, idx: usize) -> Option<usize> {
        self.summary_host.get(&idx).copied()
    }

    /// A call's committed result, with the `seq` of the entry carrying it — the
    /// address a result image's bytes resolve through (docs/control-api.md).
    fn result<'l>(
        &self,
        log: &'l [LogEntry],
        call_id: &str,
    ) -> Option<(u64, &'l RawResult, &'l WireState)> {
        let entry = log.get(*self.result_at.get(call_id)?)?;
        match &entry.payload {
            Payload::ToolResult { raw, wire, .. } => {
                let wire = self
                    .verdict_at
                    .get(&entry.seq)
                    .and_then(|idx| log.get(*idx))
                    .and_then(|entry| match &entry.payload {
                        Payload::ResultVerdict { wire, .. } => Some(wire),
                        _ => None,
                    })
                    .unwrap_or(wire);
                Some((entry.seq, raw, wire))
            }
            _ => None,
        }
    }

    /// The whole entry carrying a call's result — for the parts of it a result's
    /// body does not include (`source`, `submission_id`).
    fn result_entry<'l>(&self, log: &'l [LogEntry], call_id: &str) -> Option<&'l LogEntry> {
        log.get(*self.result_at.get(call_id)?)
    }

    fn spot<'l>(&self, log: &'l [LogEntry], call_id: &str) -> Option<&'l crate::log::Spot> {
        match &log.get(*self.spot_at.get(call_id)?)?.payload {
            Payload::Spot(spot) => Some(spot),
            _ => None,
        }
    }
}

/// The top-level nodes one committed log entry contributes. An assistant entry
/// contributes several — its thinking, its prose, and a call/annotation/result run
/// per call it made — because the transcript is flat (docs/tui.md). Each entry's
/// nodes are a pure function of the entry, the annotations indexed
/// against the calls it makes, and the human's per-call view toggles; nothing about
/// its *neighbours* is read. That independence is what the incremental projection
/// ([`Transcript`]) rests on, so keep it when adding a payload here.
pub(crate) fn entry_nodes(
    log: &[LogEntry],
    idx: usize,
    index: &CallIndex,
    view: &ViewState,
    gate: Gate<'_>,
    artifact_urls: &BTreeMap<Coordinate, String>,
) -> Vec<DocumentNode<'static>> {
    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();
    let Some(entry) = log.get(idx) else {
        return nodes;
    };
    {
        match &entry.payload {
            // The kickoff belongs to the session's opening, as the preamble does, so it
            // is not drawn as a turn: the agent's reply is the first thing shown
            // (docs/context.md).
            Payload::User(_)
                if entry.source.as_deref() == Some(crate::frontend::KICKOFF_SOURCE) => {}
            // A hand-off brief (docs/tools.md) is the predecessor speaking, not the
            // operator: drawn as the harness's note of the hand-off with the brief under
            // it, so the operator sees what this session was told.
            Payload::User(blocks)
                if let Some(predecessor) = entry
                    .source
                    .as_deref()
                    .and_then(crate::frontend::handoff_predecessor) =>
            {
                let mut body = vec![DocumentNode::Paragraph {
                    spans: vec![Span {
                        text: format!("⟢ handed off from {predecessor} · its brief:").into(),
                        style: SpanStyle::Comment,
                        action: None,
                    }],
                }];
                body.extend(markdown(&join_text(blocks)));
                nodes.push(DocumentNode::entry(EntryKind::Annotation, body));
            }
            Payload::User(blocks) => {
                // A sourced turn is attributed with the same `source:` address it
                // carries on the wire (docs/control-api.md); the operator's turns
                // have none, so their rendering is unchanged.
                let text = join_text(blocks);
                let attachments: Vec<_> = blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Attachment(arrival) => Some(arrival.clone()),
                        _ => None,
                    })
                    .collect();
                let mut body = sourced_markdown(entry.source.as_deref(), &text);
                body.extend(attachment_lines(&attachments));
                nodes.push(DocumentNode::entry(EntryKind::UserMessage, body));
            }
            Payload::Assistant { output, excluded } => {
                // A record-only entry (docs/transport.md): the stream died before
                // finalizing, or the model finalized with nothing in it, so nothing below
                // is in the model's context. The content still renders — it may be the
                // only copy of what the model produced — but the marker keeps it from
                // reading as a committed turn.
                match excluded {
                    Some(Exclusion::Upstream(why)) => nodes.push(annotation_line(
                        "⚠ stream aborted — recorded, not in context · ",
                        why.why(),
                    )),
                    Some(Exclusion::Empty) => nodes.push(annotation_line(
                        "⚠ empty completion — recorded, not in context, asked again",
                        "",
                    )),
                    None => {}
                }
                let thinking: Vec<ContentBlock> = match output {
                    AssistantOutput::Messages(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            MessagesOutputBlock::Thinking { text, signature } => {
                                Some(ContentBlock::Thinking {
                                    text: text.clone(),
                                    signature: signature.clone(),
                                })
                            }
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
                if let Some(node) = thinking_entry(&thinking) {
                    // One foldable item, keyed by the entry it came from (docs/tui.md,
                    // "Detail by recency").
                    let words = thinking
                        .iter()
                        .map(|block| match block {
                            ContentBlock::Thinking { text, .. } => {
                                crate::tui::detail::word_count(text)
                            }
                            _ => 0,
                        })
                        .sum();
                    nodes.push(DocumentNode::ThinkingItem {
                        key: format!("thinking:{}", entry.seq).into(),
                        words,
                        entry: Box::new(node),
                    });
                }
                let prose = output.text().join("\n");
                if !prose.trim().is_empty() {
                    let mut body = markdown(&prose);
                    // The entry's artifacts, in order, as the links the bridge received.
                    let entry_artifacts = Coordinate {
                        seq: entry.seq,
                        text_block: 0,
                        fence: 0,
                    }..=Coordinate {
                        seq: entry.seq,
                        text_block: usize::MAX,
                        fence: usize::MAX,
                    };
                    for url in artifact_urls.range(entry_artifacts).map(|(_, url)| url) {
                        body.push(DocumentNode::Paragraph {
                            spans: vec![Span::plain(format!("↗ artifact · {url}"))],
                        });
                    }
                    nodes.push(DocumentNode::entry(EntryKind::AssistantMessage, body));
                }
                let calls = output.tool_calls().unwrap_or_default();
                for (i, call) in calls.iter().enumerate() {
                    // A question put to the operator is drawn as the assistant's prose
                    // and the operator's reply, not as a call (docs/tui.md).
                    if let Some(question) = crate::process_monitor::ask_of_call(&calls, i) {
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
                    if call.name == crate::consolidate::NAME
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
                    if let Some(spot) = index.spot(log, call.id.0.as_str()) {
                        nodes.push(spotter_line(Some(spot.stance), &spot.reason));
                    } else if is_pending && let Some(reason) = gate.reason {
                        nodes.push(spotter_line(None, reason));
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
            }
            // Emitted as its call's result entry above.
            Payload::ToolResult { .. } => {}
            // Drawn after the entry its range ends in (see `CallIndex`); in place
            // only when that entry is not in this log.
            Payload::Summary(summary) if index.summary_host(idx).is_none() => {
                push_summary(&mut nodes, summary, replaced(log, summary));
            }
            Payload::Summary(_) => {}
            // An ambient notification (docs/inbox.md), rendered as a marked line so the
            // human sees the out-of-band event in the stream. The transcript shows the
            // whole record — every *shown* notification renders here, where the wire
            // carries only the pinned ones.
            Payload::Ambient(n) => nodes.push(DocumentNode::Paragraph {
                spans: vec![
                    Span {
                        text: format!("⟢ {} · ", n.source).into(),
                        style: SpanStyle::Comment,
                        action: None,
                    },
                    Span::plain(n.body.clone()),
                ],
            }),
            // The verdict is internal — audit-only on the transcript — except the
            // compliance signal (docs/triage.md): a triage that needed retries
            // is a harness defect worth the operator's eyes even when a retry
            // recovered, so each non-compliant attempt renders as an annotation.
            Payload::Triage(t) => {
                for why in &t.retries {
                    nodes.push(annotation_line("⚠ triage retry · ", why));
                }
            }
            // A spotter permission verdict (docs/permissions.md) renders beside the call
            // it judged, not at its own log position — it is indexed by `call_id` above
            // and emitted beside that call, so nothing is drawn here.
            Payload::Spot(_) => {}
            // A grant (docs/permissions.md) — audit-only on the transcript, so it
            // draws nothing here.
            Payload::ConfigEdit(_) => {}
            // Telemetry (docs/log-and-projection.md) — per-call measurements,
            // request stages, push and cold-check steps — reaches the reader through
            // the state topics it feeds, if at all; it draws nothing here.
            Payload::Telemetry(_) => {}
            Payload::Resumed(resumed) => {
                // `render_resumed` is the wire notice and carries its own leading
                // `⟢`, so the annotation marker is empty here — supplying one
                // doubled the glyph. The web client strips the wire glyph and
                // re-adds its own for the same reason (web/src/ui/resumed.ts).
                nodes.push(annotation_line(
                    "",
                    &crate::project::render_resumed(resumed, entry.seq),
                ));
            }
            // A process lifecycle record (docs/process-monitor.md) — the durable half
            // of what the transcript already shows in prose (the backgrounding
            // result, the exit notification), kept for readers with no daemon.
            // It draws nothing here.
            Payload::ProcessEvent(_) => {}
            // A session's worktree life (docs/worktrees.md): one muted notice line
            // per event, the shape a `Resumed` entry uses. The entry carries no
            // wire block, so it draws only here.
            Payload::Worktree(event) => nodes.push(worktree_notice(event)),
            // Read through the transcript view; the placeholder in each result
            // already says what was withheld.
            Payload::ImagesRefused { .. } => {}
            // Lifecycle record (docs/resumption.md); the session is gone from the TUI.
            Payload::Archived { .. } => {}
            // The boot preamble snapshot (docs/context.md) — session boot record,
            // not conversation; the human inspects it via `efference dev wire`, not the transcript.
            Payload::Preamble { .. } => {}
            // The session title (docs/session-title.md) is sidebar state, carried by
            // the `session_identity` topic — it draws nothing in the transcript.
            Payload::Title(_) => {}
            Payload::ResultVerdict { .. } => {}
        }
    }
    for &at in index.summaries_after.get(&idx).into_iter().flatten() {
        if let Some(Payload::Summary(summary)) = log.get(at).map(|e| &e.payload) {
            push_summary(&mut nodes, summary, replaced(log, summary));
        }
    }
    nodes
}

/// How many conversational entries a summary's range replaces: turns, completions and
/// results, not the telemetry and bookkeeping logged among them.
fn replaced(log: &[LogEntry], summary: &Summary) -> usize {
    log.iter()
        .filter(|e| {
            summary.range().contains(&e.seq)
                && matches!(
                    e.payload,
                    Payload::User(_) | Payload::Assistant { .. } | Payload::ToolResult { .. }
                )
        })
        .count()
}

/// The uncommitted end of the transcript: the in-flight streaming tail, then the
/// optimistic echoes of just-submitted turns. It depends on nothing committed, and
/// it changes on nearly every event — so the incremental projection ([`Transcript`])
/// rebuilds exactly this and reuses everything before it.
pub(crate) fn tail_nodes(
    live: &LiveTail,
    view: &ViewState,
    pending_turns: &[SubmittedTurn],
) -> Vec<DocumentNode<'static>> {
    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();

    // The live tail renders after everything committed: in-flight assistant prose
    // as its own entry, and each running tool as a peek at its output so far.
    // Thinking leads the turn, as it does on the wire and in the committed entry.
    // Accumulated rather than a single-slot preview (docs/tui.md): it stays put when the model
    // moves on to prose or a tool call, and the committed block supersedes it.
    if let Some(entry) = live_thinking_entry(&live.thinking) {
        nodes.push(entry);
    }
    if !live.assistant.is_empty() {
        nodes.push(DocumentNode::entry(
            EntryKind::AssistantMessage,
            markdown(&live.assistant),
        ));
    }
    // Each tool call whose input is still arriving — one entry per call, in the order
    // the model opened them. Without these the window between "turn submitted" and
    // "first prose" is silent, and silence is indistinguishable from a hung harness
    // (docs/tui.md). One slot instead of a collection showed only the last call, and
    // dropped what the operator was watching arrive for the first (docs/control-api.md
    // scopes every delta by content block, which is what makes the collection possible).
    for call in live.calls() {
        // An `ask` is drawn as prose once committed (docs/tui.md); its arguments
        // arriving as a call would show the plumbing.
        if call.name == crate::log::ASK {
            continue;
        }
        nodes.push(live_call_entry(call));
    }
    for (call_id, buf) in &live.tools {
        nodes.push(running_tool_peek(call_id, buf, view.is_expanded(call_id)));
    }

    // Optimistic echoes of just-submitted turns, last of all (docs/tui.md).
    for turn in pending_turns {
        nodes.push(pending_turn(&turn.text, &turn.attachments));
    }

    nodes
}

/// The call the agent made, as its own entry: the pseudocode signature
/// (docs/web-client.md, "A call reads as a signature") over whatever did not fit
/// inside it.
///
/// Agent-generated content only, and never marked by what happened *to* it: the
/// `✗` of a failed result belongs to the result, and the `⏸` of a held call belongs
/// to the gate affordance — both are entries of their own (docs/tui.md).
fn call_entry(call: &ToolCall) -> DocumentNode<'static> {
    let view = signature::call_view(&call.name, &call.input);
    // The call's `intent` (docs/tools.md) is its title: the operator reads why
    // before what, and it is the one part of a structured call that isn't inferable
    // from the arguments. Absent on calls from before the field, and on any the model
    // omitted, and then the signature is the entry's first row.
    let intent = view.intent.clone();
    let blocks = call_blocks(view, false);
    match intent {
        Some(intent) => {
            DocumentNode::headed_entry(EntryKind::ToolCall, None, intent_header(&intent), blocks)
        }
        None => DocumentNode::entry(EntryKind::ToolCall, blocks),
    }
}

/// A resolved `consolidate` call as its bare name, `› consolidate`.
fn collapsed_consolidate(call: &ToolCall) -> DocumentNode<'static> {
    let mut view = signature::call_view(&call.name, &call.input);
    view.args = None;
    view.bodies.clear();
    let signature = signature::flat_signature(&view);
    let intent = view.intent.clone();
    let blocks = vec![DocumentNode::signature(view, false)];
    let entry = match &intent {
        Some(intent) => {
            DocumentNode::headed_entry(EntryKind::ToolCall, None, intent_header(intent), blocks)
        }
        None => DocumentNode::entry(EntryKind::ToolCall, blocks),
    };
    DocumentNode::CallItem {
        key: call.id.0.clone().into(),
        must_show: false,
        intent,
        signature,
        note: None,
        nodes: vec![entry],
    }
}

/// A call and the entries that read as part of it, as one foldable item. `must_show`
/// holds it whole at any distance — the calls whose state the operator must see: no
/// result yet (running, or held at the gate), a failed one, output withheld by a
/// failed triage, or a spotter verdict that gated or watched it.
fn call_item(
    call: &ToolCall,
    result: Option<(u64, &RawResult, &WireState)>,
    is_pending: bool,
    spot: Option<&crate::log::Spot>,
    nodes: Vec<DocumentNode<'static>>,
) -> DocumentNode<'static> {
    use crate::log::Stance;
    let failed = result.is_some_and(|(_, raw, wire)| {
        raw.is_error || matches!(wire, WireState::TriageFailed { .. })
    });
    let flagged = spot.is_some_and(|s| matches!(s.stance, Stance::Gate | Stance::Watch));
    let note = match result {
        Some((_, _, WireState::Triaged(verdict))) => verdict
            .note
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string),
        _ => None,
    };
    let view = signature::call_view(&call.name, &call.input);
    DocumentNode::CallItem {
        key: call.id.0.clone().into(),
        must_show: result.is_none() || is_pending || failed || flagged,
        intent: view.intent.clone(),
        signature: signature::flat_signature(&view),
        note,
        nodes,
    }
}

/// What the call returned, as its own entry, whole. When its item is trimmed the
/// renderer cuts the body with a clip (docs/tui.md, "Detail by recency"); the entry
/// itself is the same at every level.
///
/// The header is a quiet `result (…)` row naming the wire state and how much was
/// kept, as the web client's result head does: `raw` (natural, or the operator's
/// flip), `triaged, kept 3 of 412 lines`, nothing when triage kept every line. It
/// carries the `✗` marker when the call failed (the entry that failed is the result,
/// not the call, which is a call regardless of how it went), and it is the focus
/// target the raw/triaged verb hangs off.
///
/// Keyed by `call_id`, which is what carries the raw flip across the re-projection
/// every streamed token triggers.
fn result_entry(
    call: &ToolCall,
    seq: u64,
    raw: &RawResult,
    wire: &WireState,
    view: &ViewState,
) -> DocumentNode<'static> {
    let call_id = call.id.0.as_str();
    // The human can flip a triaged result to its full raw output (docs/tui.md)
    // — both are already client-side, so this is a pure render choice. A
    // forced-raw view passes a synthetic `Raw` wire so `result_body` gives it the
    // same rich tool-specific rendering (diff/read) a natural raw result gets.
    let show_raw = view.is_raw(call_id);

    // A triaged result is built from the *verdict*, not from the wire's rendering
    // of it: the wire says "these lines came from there" as `@@ lines a-b @@` headers
    // because text is all it has, and a terminal has a gutter (docs/tui.md). Both
    // views resolve the verdict through `project::selection`, so they can differ in
    // form but never in which lines they show.
    //
    // **The triaged lines are what a triaged result shows**, for every tool — the
    // default view is the agent's view. A tool with its own representation keeps it:
    // the tool-specific parse runs over the whole output and the kept-line filter is
    // applied to the rows it produced (`triaged_body`), so a `read` is still
    // highlighted and framing-stripped and an edit's diff is still colored, at
    // whatever subset survived triage.
    //
    // The note still shows either way: it is the triager's account of the output,
    // and it is true regardless of which form of the output is on screen.
    // The header names the state and the size, the way the web client's result head
    // does (docs/web-client.md): `result (triaged, kept 3 of 412 lines)`. When
    // triage kept every line both go quiet and the head is a bare `result` — the
    // view is everything the model saw (a third of real results), and nothing may
    // imply the operator sees less.
    let mut parts: Vec<String> = Vec::new();
    let mut body = match wire {
        WireState::Triaged(verdict) if !show_raw => {
            let selection = crate::project::selection(&raw.content, &verdict.keep);
            let kept = selection.numbered_lines();
            let total = selection.shown.len() + selection.dropped;
            if kept.len() != total {
                parts.push("triaged".to_string());
                // Verb first, so which number is which is unambiguous — `3 of
                // 412` alone could be read either way round.
                parts.push(format!(
                    "kept {} of {total} line{}",
                    kept.len(),
                    if total == 1 { "" } else { "s" }
                ));
            }
            triaged_body(call.name.as_str(), raw, &kept)
        }
        _ => {
            // A forced-raw view renders the full output, so it passes a synthetic
            // `Raw` wire to get the tool-specific rendering (diff / read) a natural
            // raw result gets.
            let (content, effective_wire) = if show_raw {
                parts.push("raw".to_string());
                (raw.content.clone(), &WireState::Raw)
            } else {
                let content = match wire {
                    WireState::Pending => {
                        parts.push("awaiting triage".to_string());
                        raw.content.clone()
                    }
                    WireState::Raw => {
                        parts.push("raw".to_string());
                        raw.content.clone()
                    }
                    WireState::Elided => {
                        parts.push("elided".to_string());
                        "[elided]".to_string()
                    }
                    // The line the agent reads; the operator can flip to raw.
                    WireState::Interrupted(interruption) => {
                        parts.push("interrupted".to_string());
                        crate::project::render_interruption(interruption)
                    }
                    // The same message the agent sees on the wire; the operator can
                    // flip to raw — the withheld output is still in the log.
                    WireState::TriageFailed { why } => {
                        parts.push(format!("triage failed: {why}"));
                        crate::project::render_triage_failure(why)
                    }
                    // Unreachable: a triaged result is handled above unless it is
                    // being shown raw, which this branch already excluded.
                    WireState::Triaged(verdict) => {
                        crate::project::render_triaged(&raw.content, verdict)
                    }
                };
                (content, wire)
            };
            result_body(Some(call.name.as_str()), raw, effective_wire, &content)
        }
    };

    // Images ride below the text body, in every view — they are not triaged
    // lines, and the wire projects them regardless of the text's wire state
    // (the web client shows them unconditionally for the same reason). The
    // node is a *reference* (docs/tui.md): the bytes and their encodings live
    // in the state's image store, so this stays a pure function of the entry.
    for (idx, image) in raw.images.iter().enumerate() {
        body.push(DocumentNode::image(
            seq,
            idx,
            image.media_type.clone(),
            image.width,
            image.height,
        ));
    }
    match raw.images.len() {
        0 => {}
        1 => parts.push("1 image".to_string()),
        n => parts.push(format!("{n} images")),
    }
    // The control channel truncates an oversized result's `content` on the window
    // and stream views and tags what it cut (docs/control-api.md). Name the cut in
    // the header: a body that stops mid-output would otherwise read as the whole of
    // what the tool returned. The rest lives at `GET /log/:seq`, which this client
    // does not fetch today — the marker says what is on screen, not that it can be
    // completed.
    if let Some(elision) = raw.content_elided {
        parts.push(format!(
            "{} of {} shown",
            byte_size(elision.kept_bytes),
            byte_size(elision.total_bytes)
        ));
    }
    // A withheld-output result is surfaced as an error even when the tool itself
    // succeeded — the same signal the agent gets on the wire.
    let failed = raw.is_error || matches!(wire, WireState::TriageFailed { .. });
    if raw.is_error {
        parts.push("error".to_string());
    }
    let label = if parts.is_empty() {
        "result".to_string()
    } else {
        format!("result ({})", parts.join(", "))
    };
    let header = vec![Span {
        text: label.into(),
        style: SpanStyle::Comment,
        action: None,
    }];

    DocumentNode::headed_entry(
        EntryKind::ToolResult,
        // Only a failure is marked: the `✗` reads at a glance in the gutter, and a
        // result that went fine needs no glyph to say so.
        failed.then(|| status_span(true)),
        header,
        body,
    )
    .keyed(call_id.to_string())
}

/// The triager's note on a result, and the breadcrumb for lines the triage cap
/// dropped before triage ever saw them — each as its own
/// [`Annotation`](EntryKind::Annotation) entry beside the result (docs/tui.md).
///
/// The wire has to fold these into the result text, because a tool result is one
/// string there; the transcript does not. Empty while the result is flipped to raw:
/// raw is what the tool actually returned, and the note is not part of that.
fn selection_note(
    call: &ToolCall,
    raw: &RawResult,
    wire: &WireState,
    view: &ViewState,
) -> Vec<DocumentNode<'static>> {
    let WireState::Triaged(verdict) = wire else {
        return Vec::new();
    };
    let Triaged {
        keep,
        note,
        intent_feedback,
    } = verdict;
    if view.is_raw(call.id.0.as_str()) {
        return Vec::new();
    }
    let mut nodes = Vec::new();
    if let Some(note) = note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        nodes.push(annotation_line("⟢ ", note));
    }
    // Triage's request for a better intent, shown as the agent reads it: rare, and
    // "triage couldn't tell what you wanted" is useful to a human on its own.
    if let Some(fb) = intent_feedback
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        nodes.push(annotation_line(crate::project::INTENT_FEEDBACK_LEAD, fb));
    }
    let selection = crate::project::selection(&raw.content, keep);
    if selection.dropped > 0 {
        nodes.push(annotation_line(
            "… ",
            &format!("{} more lines truncated before triage", selection.dropped),
        ));
    }
    nodes
}

/// A byte count for the result header's elision marker: bytes below a kibibyte,
/// whole KiB above it. Rounded, because it is a gloss on how much of an output the
/// stream carried, not an accounting figure.
fn byte_size(bytes: usize) -> String {
    match bytes {
        n if n < 1024 => format!("{n} B"),
        n => format!("{} KiB", n / 1024),
    }
}

/// `"1 line"` / `"N lines"` — the result header's size gloss. Counted over the
/// *rendered* body rather than the raw content, so it describes what the reader is
/// about to scroll: a `read` result's sentinel framing is stripped before it is
/// shown, and a diff's body is the diff alone.
fn line_count(body: &[DocumentNode<'static>]) -> String {
    let lines: usize = body
        .iter()
        .map(|node| match node {
            DocumentNode::CodeBlock { code, .. } => code.lines().count().max(1),
            DocumentNode::Diff { lines } => lines.len().max(1),
            DocumentNode::NumberedCode { lines, .. } => lines.len().max(1),
            _ => 1,
        })
        .sum();
    match lines {
        1 => "1 line".to_string(),
        n => format!("{n} lines"),
    }
}

#[cfg(test)]
mod test;

mod ask;
mod incremental;
mod nodes;
mod partial;
pub(crate) mod signature;
mod tail;
mod tools;

pub(crate) use incremental::Transcript;
pub(crate) use nodes::join_text;
pub(crate) use tail::{LiveCall, LiveTail, ViewState};

use nodes::*;
use tools::*;
