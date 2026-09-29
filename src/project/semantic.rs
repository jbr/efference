//! The dialect-neutral fold of the log: which entries reach the wire, in what
//! order, rendered as far as every dialect agrees (docs/log-and-projection.md).
//!
//! `project_semantic` decides *selection* — boundary folds, result verdicts, pinned
//! notifications, surfaced context, audit-only exclusions — and produces turns a
//! dialect encoder copies onto its wire. It renders tool-result text (verdict, `[seq]`
//! handle) because every dialect sends the same text; it does not decide image
//! placement, block grouping, or breakpoint carriers, which differ per wire. Pure
//! over the log: no clock, no map iteration order reaches the output.

use std::collections::{HashMap, HashSet};

use super::{extract_boundaries, render_distilled, render_triage_failure};
use crate::api::WireDialect;
use crate::context::Item;
use crate::log::{
    AssistantOutput, CallId, ContentBlock, Disposition, LogEntry, Notification, Payload, RawResult,
    ResultImage, Seq, WireState,
};

/// One wire turn, before dialect encoding.
#[derive(Clone, Debug, PartialEq)]
pub enum WireTurn {
    /// A user-role turn: prose, tool results answering the previous assistant turn,
    /// fold summaries, pinned notifications. Parts keep log order; a dialect encoder
    /// may regroup them (Messages coalesces into one message; Responses emits one
    /// input item per result and one message for the prose).
    User(Vec<UserPart>),
    /// One model turn in its provider dialect. Owned rather than borrowed so a fork
    /// tail can append a synthetic turn without a log entry behind it.
    Assistant(AssistantOutput),
    /// A prompt-cache breakpoint requested at this point in the turn list — the
    /// per-request trailing marker, or a fork peek's frontier ahead of its raw tail
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
/// `[seq]` handle prefixed, images kept only where the verdict keeps content. A result
/// entry with a `source` (an `ask`'s reply, docs/supervisor.md) opens its content with
/// `source: `, as a sourced `User` turn does; no source leaves the bytes as they were.
pub fn render_result(
    seq: u64,
    call_id: &CallId,
    raw: &RawResult,
    wire: &WireState,
    source: Option<&str>,
) -> UserPart {
    let (content, is_error) = match wire {
        WireState::Raw => (raw.content.clone(), raw.is_error),
        WireState::Distilled(verdict) => (render_distilled(&raw.content, verdict), raw.is_error),
        WireState::Elided => ("[elided]".to_string(), false),
        WireState::TriageFailed { why } => (render_triage_failure(why), true),
    };
    let images = match wire {
        WireState::Raw | WireState::Distilled(_) => raw.images.clone(),
        WireState::Elided | WireState::TriageFailed { .. } => Vec::new(),
    };
    UserPart::ToolResult {
        call_id: call_id.clone(),
        seq: Some(seq),
        content: match source.filter(|_| !matches!(wire, WireState::Elided)) {
            Some(source) => format!("[{seq}] {source}: {content}"),
            None => format!("[{seq}] {content}"),
        },
        is_error,
        images,
    }
}

/// Fold the append-only log into dialect-neutral turns (docs/log-and-projection.md).
///
/// Deterministic by construction: a pure function of `log`, with no clock, map
/// iteration, or other ambient input leaking into the output.
pub fn project_semantic(log: &[LogEntry]) -> Vec<WireTurn> {
    let (boundaries, elided) = extract_boundaries(log);
    // Notification bodies by arrival `seq`, so a `Triage` can emit a block for each
    // notification it pinned (docs/inbox.md) without re-walking the log.
    let notifications: HashMap<Seq, &Notification> = log
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Ambient(n) => Some((e.seq, n)),
            _ => None,
        })
        .collect();
    // The `[context]` listing by item name — (description, path) for each read-on-demand
    // stub — so a `Triage` can re-render a surfaced context item (docs/redaction-fork.md)
    // at cache-read cost, mirroring the notification body map. Sourced from the last boot
    // `Preamble`'s structured snapshot (context.md); only looked up by name, so `HashMap`
    // order can't reach the wire.
    let listing: HashMap<&str, (&str, &str)> = log
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
    // Cursor into `boundaries` — advance past each one as we emit the first
    // non-elided entry after its `upto`.
    let mut boundary_cursor = 0;

    let mut turns = Vec::new();
    // The user turn currently being assembled. Tool results coalesce here (the
    // results answering one assistant turn's calls form the following user turn),
    // and a `User` entry that immediately follows them merges into the *same* turn
    // rather than forming a second consecutive user turn — the seam-drain shape
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

    for entry in log {
        // Inject the summary just before the first non-elided entry that follows
        // the fold span (i.e., the first entry with seq > upto). This places the
        // summary at the head of the live conversation rather than at the mark's
        // own log position (docs/README.md).
        while boundary_cursor < boundaries.len()
            && !elided.contains(&entry.seq)
            && entry.seq > boundaries[boundary_cursor].upto
        {
            flush(&mut pending, &mut turns);
            turns.push(WireTurn::text(boundaries[boundary_cursor].summary.clone()));
            boundary_cursor += 1;
        }

        // Skip elided entries (the folded old-task span + the mark itself).
        if elided.contains(&entry.seq) {
            continue;
        }

        match &entry.payload {
            Payload::User(blocks) => {
                // Merge into the pending user turn rather than flushing first (see
                // `pending` above). User turns author prose and attachments; the
                // thinking block forms exist on `ContentBlock` for the legacy fixture
                // shape and never occur in a `User` entry. An attachment projects as
                // its notice, never its bytes (docs/inbox.md).
                //
                // A source names who is speaking, when it is not the operator
                // (docs/control-api.md); it opens the turn's first prose part so the
                // model reads it as an address. No source renders no prefix, which is
                // what keeps every unsourced turn's bytes unchanged.
                // An attachments-only turn has no prose to open, so its source gets a
                // line of its own: the `⟢` of the harness's note stays at a line start.
                let mut source = entry.source.as_deref();
                pending.extend(blocks.iter().filter_map(|block| {
                    let (text, separator) = match block {
                        ContentBlock::Text(text) => (text.clone(), " "),
                        ContentBlock::Attachment(arrival) => {
                            (crate::attachments::turn_line(arrival), "\n")
                        }
                        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                            return None;
                        }
                    };
                    Some(UserPart::Text(match source.take() {
                        Some(source) => format!("{source}:{separator}{text}"),
                        None => text,
                    }))
                }));
            }
            // A record-only partial (docs/transport.md): the stream that produced this
            // entry died before finalizing, so the provider has no record of it and the
            // content must not enter the trajectory — a thinking-only partial projects
            // as a dangling thinking block. Skipped entirely, like `Ambient`: the retry
            // that follows re-projects to the same bytes, which is what lets it ride
            // the cached prefix.
            Payload::Assistant {
                upstream_failure: Some(_),
                ..
            } => {}
            Payload::Assistant {
                output,
                upstream_failure: None,
            } => {
                flush(&mut pending, &mut turns);
                turns.push(WireTurn::Assistant(output.clone()));
            }
            Payload::ToolResult { call_id, raw, wire } => {
                pending.push(render_result(
                    entry.seq,
                    call_id,
                    raw,
                    wire,
                    entry.source.as_deref(),
                ));
            }
            // Marks are in the elided set (extract_boundaries inserts every
            // Boundary mark's own seq); this arm is only reachable for future
            // non-Boundary mark kinds, which can be added here when they land.
            Payload::Mark(_) => {}
            // An `Ambient` arrival is audit-only on the wire (docs/inbox.md) — it never
            // itself projects; a *pinned* one is emitted by its `Triage` (below).
            Payload::Ambient(_) => {}
            // A spotter verdict (docs/permissions.md) is audit-only — the permission
            // decision lives in the log, never on the canonical wire.
            Payload::Spot(_) => {}
            // Per-call tool telemetry (docs/tools.md) is log metadata in the same
            // sense timestamps are: it measures a call the wire already carries,
            // and adds nothing the model reads. It projects to nothing, which is
            // what lets it be appended on every call without moving the cached
            // prefix (docs/log-and-projection.md).
            Payload::ToolTelemetry(_) => {}
            // A process lifecycle record (docs/supervisor.md) is audit metadata
            // beside events the wire already carries in prose — the backgrounding
            // result and the exit notification. It projects to nothing, so a
            // spawn or an exit mid-session cannot move the cached prefix
            // (docs/log-and-projection.md).
            Payload::ProcessEvent(_) => {}
            // A grant (docs/permissions.md) is audit-only too — its *effect* reaches
            // the model through the classifier (the policy fold), never the
            // transcript. So it projects to nothing, and appending one cannot perturb
            // the cached prefix (docs/log-and-projection.md).
            Payload::ConfigEdit(_) => {}
            // The boot preamble snapshot (docs/context.md) is replay/audit data:
            // the system prompt rides the request's preamble slot, never the turn
            // list, so the snapshot projects to nothing.
            Payload::Preamble { .. } => {}
            // The session title (docs/session-title.md) is presentation for the
            // clients, never for the model: it projects to nothing, so a title
            // written mid-session cannot move the cached prefix.
            Payload::Title(_) => {}
            // A fork verdict emits a block per **pinned** notification and per
            // **surfaced** context item at its own position — notification bodies
            // looked up from the `Ambient` arrival (docs/inbox.md), surfaced items
            // re-rendered from the boot listing (docs/redaction-fork.md). This
            // tail-ward placement keeps the wire append-only: content acknowledged
            // at a later gate than its origin never splices behind a breakpoint
            // (docs/log-and-projection.md). Dismissed / left / already-surfaced
            // dispositions don't project. The verdict is otherwise audit-only.
            Payload::Triage(t) => {
                let mut blocks: Vec<String> = t
                    .inbox
                    .iter()
                    .filter(|(_, d)| matches!(d, Disposition::Pin))
                    .filter_map(|(seq, _)| {
                        notifications
                            .get(seq)
                            .map(|n| format!("⟢ {}: {}", n.source, n.body))
                    })
                    .collect();
                // Surfaced context items — name resolved to its one-line description and
                // read path off the listing. A name absent from the current listing (a
                // cross-boot listing change) simply drops: deterministic, never a panic.
                blocks.extend(t.surfaced.iter().filter_map(|name| {
                    listing.get(name.as_str()).map(|(description, path)| {
                        format!("⟢ context · {name} may be relevant: {description} — read {path}")
                    })
                }));
                if !blocks.is_empty() {
                    flush(&mut pending, &mut turns);
                    turns.push(WireTurn::text(blocks.join("\n")));
                }
            }
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
