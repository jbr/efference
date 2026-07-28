//! Distillation — what a raw tool result projects to on the canonical wire
//! (docs/redaction-fork.md, docs/README.md), and the inbox triage that rides the
//! same fork peek (docs/inbox.md).
//!
//! Every tool result is distilled at append; the durable wire never carries raw
//! output, and a result's `wire` is fixed once and never mutated (so caching is
//! safe by construction). The distiller's job is to produce a **verdict** — which
//! lines survive (`keep`) plus a `note` — not rendered bytes: the wire form is
//! re-rendered from `raw` + the verdict at projection time
//! ([`crate::project::render_distilled`], docs/redaction-fork.md), so the kept bytes
//! are never stored twice and the structured decision doubles as the audit record.
//!
//! The same throwaway peek also triages the **inbox** (docs/inbox.md): the pending
//! ambient notifications are shown under the output, and the `retain` verdict's
//! `inbox` field pins / dismisses them by `[seq]`. The dispositions ride back on the
//! [`Distillation`] and land on the logged [`Triage`](crate::log::Triage). The
//! distiller is the pluggable policy (docs/tools.md) behind a trait:
//!
//! - [`HeadTail`] — a deterministic stub (first `head` + last `tail` lines).
//! - [`ForkPeek`] — the universal triage gate (docs/redaction-fork.md): fork a
//!   throwaway branch off the cached prefix, show the model the line-numbered raw
//!   output + the inbox, and let it call `retain` to choose what the canonical wire
//!   keeps and which notifications to pin. One tool call per turn (docs/README.md).
//!   On any failure it keeps everything and leaves the inbox untouched — never loses
//!   data.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Client, Completion, ToolChoice, Usage};
use crate::context::Item;
use crate::inbox::ShownNotification;
use crate::log::{
    Disposition, Keep, LogEntry, Origin, Payload, RawResult, Seq, ToolCall, WireState,
};
use crate::project::{
    ApiBlock, ApiMessage, PEEK_LINE_CAP, Role, place_trailing_cache_breakpoint, project,
};
use crate::supervisor::BgView;

const RETAIN: &str = "retain";

/// The forced tool of the **standalone** idle-triage peek (docs/supervisor.md): no `keep`
/// (there's no tool output to retain), only inbox/background dispositions.
const TRIAGE: &str = "triage";

/// A live-process triage disposition (docs/supervisor.md) — distinct from the seq-keyed
/// notification [`Disposition`] because a running process is addressed by its live
/// `bg#<n>` handle, not a frozen arrival seq, and its "dismiss" is **mute** (stop
/// surfacing the tail; the process keeps running), not clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgVerdict {
    /// Snapshot the current tail onto the canonical wire — the loop materializes it
    /// as a pinned `Ambient` at the `Triage` position (reusing the notification pin
    /// path).
    Pin,
    /// Stop surfacing the running tail; the process runs on and its exit still notifies.
    Mute,
}

/// One tool result awaiting triage: the call that produced it and its raw output.
/// A gate covers every result a single completion produced (docs/redaction-fork.md),
/// so the loop hands the distiller a slice of these rather than one at a time.
pub struct Pending<'a> {
    pub call: &'a ToolCall,
    pub raw: &'a RawResult,
}

/// The verdict on **one** tool result (docs/redaction-fork.md): the wire form plus the
/// audit pieces the loop stamps into that result's [`Triage`](crate::log::Triage)
/// entry. `raw` is never mutated — it stays in the log.
pub struct Distillation {
    /// The canonical wire form — a [`WireState::Distilled`] verdict (kept ranges +
    /// note), re-rendered from `raw` at projection time (docs/redaction-fork.md).
    pub wire: WireState,
    /// Whether the verdict is the model's or the keep-all fallback (docs/redaction-fork.md).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse — empty for a model-free distiller.
    pub raw_keep: String,
}

/// The outcome of one triage gate (docs/redaction-fork.md). A gate rides the results of
/// a single completion — usually one, but a provider that ignores the single-call
/// request (docs/README.md) returns several, and all of them are triaged together in
/// one peek. That grouping is what keeps the peek's request valid: every `tool_use` in
/// the trailing assistant message is answered by a real result in the same tail, so
/// nothing is ever orphaned.
///
/// The inbox / background / surfacing riders are **gate-scoped**, not per-result: they
/// are decided once for the step, which is why they sit here rather than on
/// [`Distillation`].
pub struct GateOutcome {
    /// One verdict per [`Pending`] handed in, in the same order. Always the same
    /// length as the input — a failed peek yields keep-all verdicts, never a short
    /// vector, so the loop can zip them without checking.
    pub results: Vec<Distillation>,
    /// Per-notification dispositions this peek chose, by arrival `seq` (docs/inbox.md) —
    /// filtered to notifications actually shown. Empty when nothing was pinned or
    /// dismissed (everything left for the next gate).
    pub inbox: Vec<(Seq, Disposition)>,
    /// Live-process dispositions this peek chose, by `bg#<n>` handle (docs/supervisor.md) —
    /// filtered to processes actually shown. The loop materializes a `Pin` into a
    /// snapshot `Ambient` and applies a `Mute` to the supervisor.
    pub background: Vec<(String, BgVerdict)>,
    /// Context-item names this peek chose to surface (docs/redaction-fork.md) —
    /// filtered to the roster actually shown (invented names dropped). Stored on the
    /// `Triage` (`surfaced`); `project` re-renders each as a read-me reminder. Empty
    /// when nothing was surfaced.
    pub surfaced: Vec<String>,
    /// The [`Usage`] of every request this gate made — empty for a model-free
    /// distiller, one entry for a fork peek. A `Vec` rather than an `Option` so a gate
    /// that ever makes more than one request reports what it actually spent: each fork
    /// request reads the shared canonical prefix (sliding its cache TTL), and the loop
    /// folds them all into the cache telemetry (docs/log-and-projection.md).
    pub usage: Vec<Usage>,
}

impl GateOutcome {
    /// The keep-all fallback for a whole gate (docs/redaction-fork.md): every result
    /// survives intact, the inbox is untouched. Used when the peek fails outright or a
    /// model-free distiller runs.
    ///
    /// Data is never lost, but this is not a free outcome: `WireState` is fixed once,
    /// so an uncurated result stays in the cached prefix for the rest of the session
    /// and every later turn pays for it. `Origin::Fallback` is the one signal the
    /// rendered artifact erases, which is why it is stamped here.
    ///
    /// `breadcrumb` records **why** — it lands in each result's `raw_keep`, the field the
    /// `Triage` entry carries for audit. Without it a fallback is anonymous in the log: a
    /// malformed verdict, a wrong-tool call, and a transport error all read identically
    /// after the fact, which is exactly the gap that made the observed fallbacks
    /// unclassifiable. Empty for a model-free distiller, which never failed at anything.
    pub fn keep_all(count: usize, usage: Vec<Usage>, breadcrumb: &str) -> Self {
        Self {
            results: (0..count)
                .map(|_| Distillation {
                    wire: WireState::Distilled {
                        keep: Keep::All,
                        note: None,
                    },
                    origin: Origin::Fallback,
                    raw_keep: breadcrumb.to_string(),
                })
                .collect(),
            inbox: Vec::new(),
            background: Vec::new(),
            surfaced: Vec::new(),
            usage,
        }
    }
}

/// The verdict of a **standalone** idle-triage peek (docs/supervisor.md) — a gate that rides no
/// tool result, so it has no [`WireState`]; just the inbox/background dispositions (and
/// the audit pieces the loop stamps into a [`Triage`](crate::log::Triage) with
/// `for_result: None`).
pub struct InboxTriage {
    pub inbox: Vec<(Seq, Disposition)>,
    pub background: Vec<(String, BgVerdict)>,
    pub origin: Origin,
    pub usage: Option<Usage>,
}

/// Decide the wire form of the results a completion produced, and triage the inbox +
/// live background processes (docs/redaction-fork.md, docs/inbox.md, docs/supervisor.md).
/// `prefix` is the log through the assistant turn that made the calls (the fork's cached
/// head); `pending` is every result that turn produced, in call order; `inbox` is the
/// pending notifications and `background` the running processes' fresh tails to show.
///
/// Taking `pending` as a slice rather than one result is what makes the peek valid for a
/// multi-call completion: the trailing assistant message carries N `tool_use` blocks, so
/// the peek's tail must answer all N (docs/redaction-fork.md). One gate, N verdicts.
#[allow(async_fn_in_trait)] // single-threaded loop; no Send bound needed.
pub trait Distiller {
    async fn distill(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        pending: &[Pending<'_>],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> GateOutcome;

    /// Triage the inbox + background with **no tool result to ride** — the
    /// idle-triggered standalone peek (docs/supervisor.md). The loop calls this when a
    /// notification wakes an idle agent; it escalates to a real turn iff this pins
    /// something. Default: no-op (the stub distiller never escalates).
    async fn triage(
        &self,
        _client: &Client,
        _prefix: &[LogEntry],
        _inbox: &[ShownNotification],
        _background: &[BgView],
    ) -> InboxTriage {
        InboxTriage {
            inbox: Vec::new(),
            background: Vec::new(),
            origin: Origin::Fallback,
            usage: None,
        }
    }
}

/// Deterministic stub: keep the first `head` + last `tail` lines. Ignores the fork
/// context, the inbox, and the background processes — for tests and as the
/// model-free fallback.
pub struct HeadTail {
    pub head: usize,
    pub tail: usize,
}

impl Distiller for HeadTail {
    async fn distill(
        &self,
        _client: &Client,
        _prefix: &[LogEntry],
        pending: &[Pending<'_>],
        _inbox: &[ShownNotification],
        _background: &[BgView],
    ) -> GateOutcome {
        // Model-free: a `Fallback` verdict per result, no usage, inbox + background +
        // surfacing untouched. Each result is head/tailed independently — the stub has
        // no cross-result judgment to make.
        let results = pending
            .iter()
            .map(|p| {
                let n = p.raw.content.lines().count();
                let keep = if n <= self.head + self.tail {
                    Keep::All
                } else {
                    Keep::Ranges(vec![(1, self.head), (n - self.tail + 1, n)])
                };
                Distillation {
                    wire: WireState::Distilled { keep, note: None },
                    origin: Origin::Fallback,
                    raw_keep: String::new(),
                }
            })
            .collect();
        GateOutcome {
            results,
            inbox: Vec::new(),
            background: Vec::new(),
            surfaced: Vec::new(),
            usage: Vec::new(),
        }
    }
}

/// The model peek — the universal triage gate (docs/redaction-fork.md) realized.
pub struct ForkPeek;

/// One fork round-trip: force `retain` and decode the verdict, pushing the request's
/// [`Usage`] onto `usage` whether or not the verdict was usable (the request happened
/// and slid the prefix's TTL; only a transport error yields none).
///
/// Transport errors are deliberately *not* retried here — a failed request is the
/// transport's problem, and retrying transient statuses belongs on the `Client`
/// (backlog/transient-api-failures.md). What this retries is a *usable response
/// carrying an unusable verdict*, which is a different failure with a different fix.
async fn peek_once(
    client: &Client,
    messages: &[ApiMessage],
    tools: &[Value],
    offered: &[String],
    usage: &mut Vec<Usage>,
) -> Result<ParsedRetain, String> {
    // No-op sink: the fork peek's triage tokens are internal and must never surface on
    // `GET /stream` — only the main thread's prose streams.
    match client
        .complete(messages, tools, ToolChoice::force(RETAIN), |_| {})
        .await
    {
        Ok(completion) => {
            let parsed = parse_retain(&completion, offered);
            let outcome = parsed.ok_or_else(|| unusable(&completion));
            usage.push(completion.usage);
            outcome
        }
        Err(e) => Err(format!("peek request failed: {e}")),
    }
}

/// Describe a completion that produced no usable verdict, for the `Triage` breadcrumb.
/// The model's actual `retain` input is the most useful form when there was one — that
/// is what a future reader needs to tell a malformed verdict from a wrong-tool call.
fn unusable(completion: &Completion) -> String {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return "peek returned no assistant payload".into();
    };
    match calls.iter().find(|c| c.name == RETAIN) {
        Some(c) => format!("unparseable retain input: {}", c.input),
        None if calls.is_empty() => "peek called no tool".into(),
        None => format!(
            "peek called {} instead of retain",
            calls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Build the fork peek's request (docs/redaction-fork.md): the cached
/// prefix-through-`tool_use`, then one user message carrying every result plus the
/// gate-scoped riders and the retain prompt.
///
/// **The invariant this exists to hold:** the trailing assistant message carries one
/// `tool_use` per call the completion made, and the API rejects a request that leaves
/// any of them unanswered. So the tail emits one `tool_result` per [`Pending`] — which
/// is only correct because the loop runs *every* call of a completion before opening the
/// gate. Peeking between calls would leave the later ones orphaned, which is exactly the
/// failure this replaced.
///
/// The riders (inbox, background, surfacing roster) are gate-scoped, so they ride the
/// trailing text block rather than any one result. Pure and separate from the round-trip
/// so the pairing invariant is testable without a live client.
fn peek_messages(
    prefix: &[LogEntry],
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[String],
) -> Vec<ApiMessage> {
    let mut sections = String::new();
    append_inbox_and_background(&mut sections, inbox, background);
    append_roster(&mut sections, roster);

    let mut instruction = RETAIN_INSTRUCTION.to_string();
    if !inbox.is_empty() {
        instruction.push_str(INBOX_INSTRUCTION);
    }
    if !background.is_empty() {
        instruction.push_str(BACKGROUND_INSTRUCTION);
    }
    if !roster.is_empty() {
        instruction.push_str(SURFACE_INSTRUCTION);
    }

    // The prefix hits cache; only the raws and the instruction are fresh, and they are
    // discarded — the canonical lineage never sees raw (docs/redaction-fork.md).
    let mut messages = project(prefix);
    place_trailing_cache_breakpoint(&mut messages);
    let mut content: Vec<ApiBlock> = pending
        .iter()
        .map(|p| ApiBlock::ToolResult {
            tool_use_id: p.call.id.0.clone(),
            content: result_view(p.call, p.raw),
            is_error: p.raw.is_error,
            cache_control: None,
        })
        .collect();
    content.push(ApiBlock::Text {
        text: format!("{sections}\n\n{instruction}"),
        cache_control: None,
    });
    messages.push(ApiMessage {
        role: Role::User,
        content,
    });
    messages
}

impl Distiller for ForkPeek {
    async fn distill(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        pending: &[Pending<'_>],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> GateOutcome {
        if pending.is_empty() {
            return GateOutcome::keep_all(0, Vec::new(), "");
        }
        // The ids this gate is asking about, in call order — the `retain` verdict's
        // coordinate system, and what the reconcile below checks coverage against.
        let offered: Vec<String> = pending.iter().map(|p| p.call.id.0.clone()).collect();

        let roster = surfaceable_roster(prefix);
        let messages = peek_messages(prefix, pending, inbox, background, &roster);

        let tools = [retain_tool(inbox, background, &roster)];
        let mut usage = Vec::new();
        let parsed = peek_once(client, &messages, &tools, &offered, &mut usage).await;

        let verdict = match parsed {
            Ok(v) => v,
            Err(why) => {
                // No usable verdict. Keep everything rather than drop data — but this is
                // a real failure, not a neutral default: the uncurated output is now
                // durable on the wire (`WireState` is fixed once), so every later turn
                // pays for it. Logged at error, stamped `Origin::Fallback`, and the
                // breadcrumb records what came back so the next occurrence is
                // classifiable rather than anonymous. Surfacing it to the operator as a
                // session-visible failure is tracked with the turn-failure work
                // (backlog/transient-api-failures.md).
                log::error!(
                    "fork peek produced no usable verdict for {} result(s); keeping all \
                     output uncurated on the wire — {why}",
                    offered.len()
                );
                return GateOutcome::keep_all(pending.len(), usage, &why);
            }
        };

        // Reconcile the model's verdicts against what was actually offered, in call order.
        // A result the model never addressed keeps everything (loudly); ids it invented
        // simply find no home — the same harness-side guard applied to notification ids
        // and roster names below.
        let results = pending
            .iter()
            .map(
                |p| match verdict.results.iter().find(|v| v.call_id == p.call.id.0) {
                    Some(v) => Distillation {
                        wire: WireState::Distilled {
                            keep: v.keep.clone(),
                            note: v.note.clone(),
                        },
                        origin: Origin::Model,
                        raw_keep: v.raw_keep.clone(),
                    },
                    None => {
                        log::error!(
                            "fork peek returned no verdict for `{}` ({}); keeping it uncurated",
                            p.call.name,
                            p.call.id.0
                        );
                        Distillation {
                            wire: WireState::Distilled {
                                keep: Keep::All,
                                note: None,
                            },
                            origin: Origin::Fallback,
                            raw_keep: String::new(),
                        }
                    }
                },
            )
            .collect();

        // Keep only dispositions for notifications we actually showed (drop any stray id
        // the model invented).
        let shown: HashSet<Seq> = inbox.iter().map(|n| n.seq).collect();
        let dispositions = verdict
            .inbox
            .into_iter()
            .filter(|(s, _)| shown.contains(s))
            .collect();
        // Same guard for background handles: only verdicts on processes we actually showed.
        let shown_bg: HashSet<&str> = background.iter().map(|b| b.handle.as_str()).collect();
        let bg = verdict
            .background
            .into_iter()
            .filter(|(h, _)| shown_bg.contains(h.as_str()))
            .collect();
        // Keep only names actually on the roster (drop any the model invented), deduped.
        let offered_names: HashSet<&str> = roster.iter().map(String::as_str).collect();
        let mut kept = HashSet::new();
        let surfaced = verdict
            .surfaced
            .into_iter()
            .filter(|n| offered_names.contains(n.as_str()) && kept.insert(n.clone()))
            .collect();

        GateOutcome {
            results,
            inbox: dispositions,
            background: bg,
            surfaced,
            usage,
        }
    }

    /// The standalone idle-triage peek (docs/supervisor.md, docs/inbox.md): fork off
    /// the cached prefix, show the inbox + background with no tool result, and force
    /// the `triage` tool (no
    /// `keep`). The caller wakes this when an event arrives while idle and escalates to
    /// a real turn iff it pins something. On any failure it pins/dismisses nothing —
    /// the agent simply goes back to idle, losing no data.
    async fn triage(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> InboxTriage {
        let mut view = String::new();
        append_inbox_and_background(&mut view, inbox, background);
        view.push_str("\n\n");
        view.push_str(TRIAGE_INSTRUCTION);

        let mut messages = project(prefix);
        place_trailing_cache_breakpoint(&mut messages);
        messages.push(ApiMessage {
            role: Role::User,
            content: vec![ApiBlock::Text {
                text: view,
                cache_control: None,
            }],
        });

        let (parsed, usage) = match client
            .complete(
                &messages,
                &[triage_tool(inbox, background)],
                ToolChoice::force(TRIAGE),
                |_| {},
            )
            .await
        {
            Ok(completion) => (
                parse_dispositions(&completion, TRIAGE),
                Some(completion.usage),
            ),
            Err(_) => (None, None),
        };

        match parsed {
            Some((ibx, bg)) => {
                let shown: HashSet<Seq> = inbox.iter().map(|n| n.seq).collect();
                let shown_bg: HashSet<&str> =
                    background.iter().map(|b| b.handle.as_str()).collect();
                InboxTriage {
                    inbox: ibx.into_iter().filter(|(s, _)| shown.contains(s)).collect(),
                    background: bg
                        .into_iter()
                        .filter(|(h, _)| shown_bg.contains(h.as_str()))
                        .collect(),
                    origin: Origin::Model,
                    usage,
                }
            }
            None => InboxTriage {
                inbox: Vec::new(),
                background: Vec::new(),
                origin: Origin::Fallback,
                usage,
            },
        }
    }
}

// ---- the verdict ----

/// Triaged dispositions: inbox items (seq-keyed) and background processes (handle-keyed).
type Dispositions = (Vec<(Seq, Disposition)>, Vec<(String, BgVerdict)>);

// ---- the peek view ----

#[cfg(test)]
mod test;

mod parse;
mod prompt;
mod schema;

use parse::*;
use prompt::*;
use schema::*;
