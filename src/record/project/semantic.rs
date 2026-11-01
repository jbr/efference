//! The dialect-neutral fold of the log: which entries reach the wire, in what
//! order, rendered as far as every dialect agrees (docs/log-and-projection.md).
//!
//! `project_semantic` decides *selection* — summaries, result verdicts, pinned
//! notifications, surfaced context, audit-only exclusions — and produces turns a
//! dialect encoder copies onto its wire. It renders tool-result text (verdict, `[seq]`
//! handle) because every dialect sends the same text; it does not decide image
//! placement, block grouping, or breakpoint carriers, which differ per wire. Pure
//! over the log: no clock, no map iteration order reaches the output.

use std::collections::{HashMap, HashSet};

use super::{Reader, extract_boundaries, render_triage_failure, render_triaged};
use crate::record::log::context::Item;
use crate::record::log::{
    AssistantOutput, CallId, ContentBlock, Disposition, LogEntry, Notification, Payload, RawResult,
    ResultImage, Seq, Triage, WireDialect, WireState,
};

/// One wire turn, before dialect encoding.
#[derive(Clone, Debug, PartialEq)]
pub enum WireTurn {
    /// A user-role turn: prose, tool results answering the previous assistant turn,
    /// summaries, pinned notifications. Parts keep log order; a dialect encoder
    /// may regroup them (Messages coalesces into one message; Responses emits one
    /// input item per result and one message for the prose).
    User(Vec<UserPart>),
    /// One model turn in its provider dialect. Owned rather than borrowed so a fork
    /// tail can append a synthetic turn without a log entry behind it.
    Assistant(AssistantOutput),
    /// A prompt-cache breakpoint requested at this point in the turn list — the
    /// per-request trailing marker, or a triage's frontier ahead of its raw tail
    /// (docs/log-and-projection.md). Each encoder attaches it to the nearest
    /// preceding block its wire lets carry one, so the physical marker may sit
    /// earlier than the logical frontier.
    Breakpoint,
}

/// Why a turn list cannot be encoded onto a target dialect (docs/transport.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialectConflict {
    /// Assistant turns of both dialects in one list. No session writes this; it is
    /// a log stitched together from two, and neither encoder can carry all of it.
    Mixed,
    /// Every assistant turn is in `recorded`, and the request would go out as
    /// `target`. That encoder skips the foreign turns, so the model would see a
    /// history with its own replies missing.
    Foreign {
        recorded: WireDialect,
        target: WireDialect,
    },
}

impl std::fmt::Display for DialectConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mixed => write!(
                f,
                "the session records assistant turns in both the Messages and Responses \
                 dialects; no single request can carry them"
            ),
            Self::Foreign { recorded, target } => write!(
                f,
                "the session's assistant turns are recorded in the {recorded} dialect, \
                 but the declared provider speaks {target}; encoding it would drop every \
                 assistant turn. Resume it under a {recorded} provider"
            ),
        }
    }
}

impl std::error::Error for DialectConflict {}

/// The dialect the assistant turns are recorded in — `None` for a list with no
/// assistant turn yet (a fresh session is dialect-free until its first reply), an
/// error if the turns do not agree.
pub fn recorded_dialect(turns: &[WireTurn]) -> Result<Option<WireDialect>, DialectConflict> {
    let mut recorded = None;
    for turn in turns {
        let WireTurn::Assistant(output) = turn else {
            continue;
        };
        match recorded {
            None => recorded = Some(output.dialect()),
            Some(seen) if seen != output.dialect() => return Err(DialectConflict::Mixed),
            Some(_) => {}
        }
    }
    Ok(recorded)
}

/// Refuse to encode `turns` onto `target` when their assistant turns belong to the
/// other dialect (docs/transport.md). Each encoder skips the foreign arm rather than
/// failing, which is right while a live session is pinned to one provider and wrong
/// for a recorded session rendered or continued under another — there the skip is
/// silent loss. A list with no assistant turn passes on either dialect.
pub fn check_dialect(turns: &[WireTurn], target: WireDialect) -> Result<(), DialectConflict> {
    match recorded_dialect(turns)? {
        Some(recorded) if recorded != target => Err(DialectConflict::Foreign { recorded, target }),
        _ => Ok(()),
    }
}

/// One part of a user-role turn.
#[derive(Clone, Debug, PartialEq)]
pub enum UserPart {
    Text(String),
    /// A tool result as the model will read it: verdict applied, `[seq]` handle
    /// prefixed. `seq` is `None` for a fork-local result that has no log entry yet.
    ToolResult {
        call_id: CallId,
        seq: Option<u64>,
        content: String,
        is_error: bool,
        images: Vec<ResultImage>,
    },
    /// A prompt-cache breakpoint requested after the preceding part. The trailing
    /// per-request breakpoint is placed by the client at encode time; this marker
    /// is for a caller that wants one mid-turn (the spotter's stable scope block).
    Breakpoint,
}

impl WireTurn {
    /// A user turn of one prose part.
    pub fn text(text: impl Into<String>) -> Self {
        Self::User(vec![UserPart::Text(text.into())])
    }
}

/// Render a logged result the way the model reads it: the verdict applied, the
/// `[seq]` handle prefixed, images kept only where the verdict keeps content.
///
/// An `ask`'s reply (docs/process-monitor.md) is framed as an answer: `asked: <question>` on
/// the first line, so a reply names its question rather than leaning on call order,
/// then the reply prefixed `source: ` when a client other than the operator sent it,
/// as a sourced `User` turn is. Any other result leaves the bytes as they were.
pub fn render_result(
    seq: u64,
    call_id: &CallId,
    raw: &RawResult,
    wire: &WireState,
    reply: Option<AskReply<'_>>,
) -> UserPart {
    let (content, is_error) = result_content(raw, wire);
    let images = match wire {
        WireState::Raw | WireState::Triaged(_) => raw.images.clone(),
        WireState::Pending
        | WireState::Elided
        | WireState::Interrupted(_)
        | WireState::TriageFailed { .. } => Vec::new(),
    };
    UserPart::ToolResult {
        call_id: call_id.clone(),
        seq: Some(seq),
        content: match reply.filter(|_| !matches!(wire, WireState::Elided)) {
            Some(AskReply { question, source }) => {
                let from = source.map_or_default(|s| {
                    format!("{}: ", crate::record::names::sources::wire_label(s))
                });
                format!("[{seq}] asked: {question}\n{from}{content}")
            }
            None => format!("[{seq}] {content}"),
        },
        is_error,
        images,
    }
}

/// A result's text and error flag as the model reads them, before the `[seq]` handle:
/// the verdict applied to `raw` (docs/triage.md). The separate triage's text view shows
/// recent results this way too.
pub fn result_content(raw: &RawResult, wire: &WireState) -> (String, bool) {
    match wire {
        WireState::Raw => (raw.content.clone(), raw.is_error),
        WireState::Pending => ("[awaiting triage]".to_string(), false),
        WireState::Triaged(verdict) => (render_triaged(&raw.content, verdict), raw.is_error),
        WireState::Elided => ("[elided]".to_string(), false),
        WireState::Interrupted(interruption) => (
            super::render::render_interruption(interruption),
            interruption.killed,
        ),
        WireState::TriageFailed { why } => (render_triage_failure(why), true),
    }
}

/// What frames a result as the answer to an `ask`: the question, and who replied when
/// it was not the operator.
#[derive(Clone, Copy, Debug)]
pub struct AskReply<'a> {
    pub question: &'a str,
    pub source: Option<&'a str>,
}

/// What the fold resolves by key instead of re-walking the log. Only looked up, never
/// iterated, so `HashMap` order cannot reach the wire.
struct Lookups<'a> {
    /// Each `ask`'s question by call id, so its reply can name it (`render_result`).
    questions: HashMap<CallId, String>,
    /// Notification bodies by arrival `seq`, so a `Triage` can emit a block for each
    /// notification it pinned (docs/inbox.md) without re-walking the log.
    notifications: HashMap<Seq, &'a Notification>,
    /// The `[context]` listing by item name — (description, path) for each read-on-demand
    /// stub — so a `Triage` can re-render a surfaced context item (docs/triage.md)
    /// at cache-read cost, mirroring the notification body map. Sourced from the last boot
    /// `Preamble`'s structured snapshot (context.md).
    listing: HashMap<&'a str, (&'a str, &'a str)>,
}

impl<'a> Lookups<'a> {
    fn of(log: &'a [LogEntry]) -> Self {
        let questions = log
            .iter()
            .filter_map(|e| match &e.payload {
                Payload::Assistant { output, .. } => output.tool_calls().ok(),
                _ => None,
            })
            .flatten()
            .filter(|call| call.name == crate::record::names::tools::ASK)
            .filter_map(|call| {
                let question = call.input.get("question")?.as_str()?.trim().to_string();
                Some((call.id, question))
            })
            .collect();
        let notifications = log
            .iter()
            .filter_map(|e| match &e.payload {
                Payload::Ambient(n) => Some((e.seq, n)),
                _ => None,
            })
            .collect();
        let listing = log
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                Payload::Preamble { context, .. } => Some(context),
                _ => None,
            })
            .into_iter()
            .flat_map(|ctx| ctx.sections.iter().flat_map(|s| &s.items))
            .filter_map(|item| match item {
                Item::Listed {
                    name,
                    description,
                    path,
                } => Some((name.as_str(), (description.as_str(), path.as_str()))),
                Item::Inline { .. } | Item::Exec { .. } => None,
            })
            .collect();
        Self {
            questions,
            notifications,
            listing,
        }
    }
}

/// A `User` entry's parts. User turns author prose and attachments; the thinking block
/// forms exist on `ContentBlock` for the legacy fixture shape and never occur in a `User`
/// entry. An attachment projects as its notice, never its bytes (docs/inbox.md).
///
/// A source names who is speaking, when it is not the operator
/// (docs/control-api.md); it opens the turn's first prose part so the
/// model reads it as an address, another session's as `<id> (agent)`.
/// No source renders no prefix, which is what keeps every unsourced
/// turn's bytes unchanged.
/// An attachments-only turn has no prose to open, so its source gets a
/// line of its own: the `⟢` of the harness's note stays at a line start.
fn user_parts<'a>(
    entry: &'a LogEntry,
    blocks: &'a [ContentBlock],
) -> impl Iterator<Item = UserPart> + 'a {
    let mut source = entry.source.as_deref();
    blocks.iter().filter_map(move |block| {
        let (text, separator) = match block {
            ContentBlock::Text(text) => (text.clone(), " "),
            ContentBlock::Attachment(arrival) => {
                (crate::record::log::attachment::turn_line(arrival), "\n")
            }
            ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                return None;
            }
        };
        Some(UserPart::Text(match source.take() {
            Some(source) => {
                format!(
                    "{}:{separator}{text}",
                    crate::record::names::sources::wire_label(source)
                )
            }
            None => text,
        }))
    })
}

/// The text a `Triage` projects at its own position: a block per **pinned** notification and
/// per **surfaced** context item — notification bodies looked up from the `Ambient` arrival
/// (docs/inbox.md), surfaced items re-rendered from the boot listing (docs/triage.md). This
/// tail-ward placement keeps the wire append-only: content acknowledged at a later triage
/// than its origin never splices behind a breakpoint (docs/log-and-projection.md).
/// Dismissed / left / already-surfaced dispositions don't project. The verdict is otherwise
/// audit-only, and `None` when nothing is pinned or surfaced.
fn triage_text(t: &Triage, lookups: &Lookups) -> Option<String> {
    let mut blocks: Vec<String> = t
        .inbox
        .iter()
        .filter(|(_, d)| matches!(d, Disposition::Pin))
        .filter_map(|(seq, _)| {
            lookups.notifications.get(seq).map(|n| {
                let source = crate::record::names::sources::wire_label(&n.source);
                format!("⟢ {source}: {}", n.body)
            })
        })
        .collect();
    // Surfaced context items — name resolved to its one-line description and
    // read path off the listing. A name absent from the current listing (a
    // cross-boot listing change) simply drops: deterministic, never a panic.
    blocks.extend(t.surfaced.iter().filter_map(|name| {
        lookups
            .listing
            .get(name.as_str())
            .map(|(description, path)| {
                format!("⟢ context · {name} may be relevant: {description} — read {path}")
            })
    }));
    (!blocks.is_empty()).then(|| blocks.join("\n"))
}

/// Fold the append-only log into dialect-neutral turns (docs/log-and-projection.md).
///
/// Deterministic by construction: a pure function of `log`, with no clock, map
/// iteration, or other ambient input leaking into the output.
pub fn project_semantic(log: &[LogEntry]) -> Vec<WireTurn> {
    project_semantic_for(log, Reader::Main)
}

/// [`project_semantic`] for one reader (docs/summary.md): the same fold, with the
/// summaries addressed to the other reader left out.
pub fn project_semantic_for(log: &[LogEntry], reader: Reader) -> Vec<WireTurn> {
    let skipped = super::skipped_seqs(log);
    let (boundaries, elided) = extract_boundaries(log, &skipped, reader);
    let verdicts = crate::record::log::result_verdicts(log);
    let lookups = Lookups::of(log);
    // Cursor into `boundaries` — advance past each one as we emit the first
    // non-elided entry after its `upto`.
    let mut boundary_cursor = 0;

    let mut turns = Vec::new();
    // The user turn currently being assembled. Tool results coalesce here (the
    // results answering one assistant turn's calls form the following user turn),
    // and a `User` entry that immediately follows them merges into the *same* turn
    // rather than forming a second consecutive user turn — the shape of a queued turn delivered mid-task
    // (docs/log-and-projection.md): an injected/redirect user turn appended right
    // after a turn's results must project as one turn `[result…, text]`, preserving
    // role-alternation and byte-stability. Flushed at the next assistant / boundary /
    // end.
    let mut pending: Vec<UserPart> = Vec::new();
    let flush = |pending: &mut Vec<UserPart>, turns: &mut Vec<WireTurn>| {
        if !pending.is_empty() {
            turns.push(WireTurn::User(std::mem::take(pending)));
        }
    };
    let elided: HashSet<Seq> = elided;
    // The restart notices of sessions resumed at an unfinished batch (docs/resumption.md),
    // each with the calls whose results it waits behind. A notice is text, and a `tool_use`'s
    // results have to lead the user turn that answers it, so it lands in `pending` after the
    // last of them rather than at its own position. Held at the end of the log when the
    // batch is still unanswered, it is not projected, which is the wire a live gate has.
    let mut held_notices: Vec<(Vec<CallId>, String)> = Vec::new();
    let mut emitted: HashSet<CallId> = HashSet::new();
    let release = |held: &mut Vec<(Vec<CallId>, String)>,
                   emitted: &HashSet<CallId>,
                   pending: &mut Vec<UserPart>| {
        held.retain(|(waiting, text)| {
            let ready = waiting.iter().all(|id| emitted.contains(id));
            if ready {
                pending.push(UserPart::Text(text.clone()));
            }
            !ready
        });
    };

    for entry in log {
        // Inject the summary just before the first non-elided entry that follows
        // the summarized range (i.e., the first entry with seq > upto). This places the
        // summary at the head of the live conversation rather than at the entry's
        // own log position (docs/README.md).
        while boundary_cursor < boundaries.len()
            && !elided.contains(&entry.seq)
            && !skipped.contains(&entry.seq)
            && entry.seq > boundaries[boundary_cursor].upto
        {
            flush(&mut pending, &mut turns);
            turns.push(WireTurn::text(boundaries[boundary_cursor].summary.clone()));
            boundary_cursor += 1;
        }

        // Skip elided entries (the summarized old-task range + the summary entry itself) and the
        // entries a `Resumed` entry rolled back (docs/resumption.md).
        if elided.contains(&entry.seq) || skipped.contains(&entry.seq) {
            continue;
        }

        match &entry.payload {
            // Merge into the pending user turn rather than flushing first (see
            // `pending` above).
            Payload::User(blocks) => pending.extend(user_parts(entry, blocks)),
            // A record-only entry (`excluded`, docs/transport.md) falls to the last arm.
            Payload::Assistant {
                output,
                excluded: None,
            } => {
                flush(&mut pending, &mut turns);
                turns.push(WireTurn::Assistant(output.clone()));
            }
            Payload::ToolResult { call_id, raw, wire } => {
                // A reply is the result a submission produced; an ask that ended
                // without one (an interrupt, a refused call) renders as any result.
                let wire = verdicts.get(&entry.seq).copied().unwrap_or(wire);
                if matches!(wire, WireState::Pending) {
                    continue;
                }
                let reply = entry
                    .submission_id
                    .as_ref()
                    .and_then(|_| lookups.questions.get(call_id))
                    .map(|question| AskReply {
                        question,
                        source: entry.source.as_deref(),
                    });
                pending.push(render_result(entry.seq, call_id, raw, wire, reply));
                emitted.insert(call_id.clone());
                release(&mut held_notices, &emitted, &mut pending);
            }
            // The restart notice (docs/log-and-projection.md): one harness text block at
            // the entry's own position, so it appends to the wire like any later turn.
            Payload::Resumed(resumed) if !resumed.pending_calls.is_empty() => {
                held_notices.push((
                    resumed.pending_calls.clone(),
                    super::render::render_resumed(resumed, entry.seq),
                ));
                release(&mut held_notices, &emitted, &mut pending);
            }
            Payload::Resumed(resumed) => {
                flush(&mut pending, &mut turns);
                turns.push(WireTurn::text(super::render::render_resumed(
                    resumed, entry.seq,
                )));
            }
            // A triage verdict emits its pinned and surfaced blocks at its own position
            // and is otherwise audit-only.
            Payload::Triage(t) => {
                if let Some(text) = triage_text(t, &lookups) {
                    flush(&mut pending, &mut turns);
                    turns.push(WireTurn::text(text));
                }
            }
            // Every other entry projects to nothing at its own position, which is what
            // lets one be appended mid-session without moving the cached prefix
            // (docs/log-and-projection.md):
            // - a record-only assistant entry (docs/transport.md): the loop asks again
            //   past it, and the retry re-projects to the same bytes;
            // - a summary, which is in the elided set and is injected at its boundary
            //   above; a result's verdict, read where its result projects; an `Ambient`
            //   arrival, which reaches the wire only when a `Triage` pins it
            //   (docs/inbox.md);
            // - the session's audit record, which the model never reads: spotter
            //   verdicts, telemetry, process and worktree records, a grant (its effect
            //   reaches the model through the classifier), the boot preamble (the
            //   system prompt rides the request's own slot), the title, the archive
            //   record, and an image refusal (the client withholds images in its own
            //   encoding, docs/transport.md).
            _ => {}
        }
    }

    // Flush any boundaries whose upto is past the end of the log (degenerate
    // edge: consolidate called with nothing after it yet).
    while boundary_cursor < boundaries.len() {
        flush(&mut pending, &mut turns);
        turns.push(WireTurn::text(boundaries[boundary_cursor].summary.clone()));
        boundary_cursor += 1;
    }

    flush(&mut pending, &mut turns);
    turns
}
