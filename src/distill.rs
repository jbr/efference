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
//! [`Distillation`] and land on the logged [`Triage`](crate::log::Triage). One type
//! does the distilling:
//!
//! - [`ForkPeek`] — the universal triage gate (docs/redaction-fork.md): fork a
//!   throwaway branch off the cached prefix, show the model the line-numbered raw
//!   output + the inbox, and ask it (via the tail instruction, never a forced
//!   `tool_choice`) to call `retain` to choose what the canonical wire keeps and which
//!   notifications to pin. The peek sends the *same* request envelope as an agent turn —
//!   one shared `tools[]`, `tool_choice: auto`, the same thinking policy — so both
//!   shapes read one cache lineage (docs/redaction-fork.md, docs/log-and-projection.md).
//!   A non-compliant response gets a bounded in-fork retry; exhaustion is a **hard
//!   error** — the results project [`WireState::TriageFailed`] (an error in place of
//!   the output; the raw stays in the log) and the inbox is left untouched. The full
//!   untriaged raw is never welded onto the canonical wire.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{STREAM_ABORT_ATTEMPTS, stream_abort_backoff, stream_abort_retry};
use crate::api::{Client, Completion, RequestUsage, RetrySurface, ToolChoice};
use crate::context::Item;
use crate::inbox::ShownNotification;
use crate::log::{
    AssistantOutput, Disposition, Distilled, Keep, LogEntry, Origin, Payload, RawResult, Seq,
    ToolCall, WireState,
};
use crate::project::{PEEK_LINE_CAP, UserPart, WireTurn, project_semantic};
use crate::supervisor::BgView;

const RETAIN: &str = "retain";

/// The tool of the **standalone** idle-triage peek (docs/supervisor.md): no `keep`
/// (there's no tool output to retain), only inbox/background dispositions.
const TRIAGE: &str = "triage";

/// Whether `name` is a peek tool. Both stand in every turn's `tools[]` (the shared
/// envelope, docs/redaction-fork.md) but are answered only when a peek's tail
/// instruction asks — the agent loop pairs an un-asked call with a nudge result
/// instead of running the cascade.
pub fn is_peek_tool(name: &str) -> bool {
    name == RETAIN || name == TRIAGE
}

/// A live-process triage disposition (docs/supervisor.md) — distinct from the seq-keyed
/// notification [`Disposition`] because a running process is addressed by its live
/// `p#<n>` handle, not a frozen arrival seq, and its "dismiss" is **mute** (stop
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
    /// The canonical wire form — a [`WireState::Distilled`] verdict ([`Distilled`]:
    /// kept ranges, note, feedback), re-rendered from `raw` at projection time (docs/redaction-fork.md).
    pub wire: WireState,
    /// Whether the verdict is the model's or a harness fallback (docs/redaction-fork.md).
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
/// [`Distillation`]. `Default` is the outcome of a gate with nothing to triage.
#[derive(Default)]
pub struct GateOutcome {
    /// One verdict per [`Pending`] handed in, in the same order. Always the same
    /// length as the input — a failed peek yields [`WireState::TriageFailed`]
    /// verdicts, never a short vector, so the loop can zip them without checking.
    pub results: Vec<Distillation>,
    /// Per-notification dispositions this peek chose, by arrival `seq` (docs/inbox.md) —
    /// filtered to notifications actually shown. Empty when nothing was pinned or
    /// dismissed (everything left for the next gate).
    pub inbox: Vec<(Seq, Disposition)>,
    /// Live-process dispositions this peek chose, by `p#<n>` handle (docs/supervisor.md) —
    /// filtered to processes actually shown. The loop materializes a `Pin` into a
    /// snapshot `Ambient` and applies a `Mute` to the supervisor.
    pub background: Vec<(String, BgVerdict)>,
    /// Context-item names this peek chose to surface (docs/redaction-fork.md) —
    /// filtered to the roster actually shown (invented names dropped). Stored on the
    /// `Triage` (`surfaced`); `project` re-renders each as a read-me reminder. Empty
    /// when nothing was surfaced.
    pub surfaced: Vec<String>,
    /// The [`RequestUsage`] of every request this gate made — empty for a model-free
    /// distiller, one entry per fork round-trip (in-fork retries included). Each fork
    /// request reads the shared canonical prefix (sliding its cache TTL), and the loop
    /// folds them all into the cache telemetry (docs/log-and-projection.md).
    pub usage: Vec<RequestUsage>,
    /// One attempt-stamped breadcrumb per peek attempt that had to be retried —
    /// recorded even when a retry recovered (docs/redaction-fork.md). Two families,
    /// distinguishable by wording: a non-compliant verdict (wrong tool, no tool,
    /// malformed input), and a stream that died before finalizing. The loop lands
    /// these on the gate's `Triage` (`retries`), the durable record; a runtime triage
    /// failure is never accepted silently, recovered or not.
    pub retries: Vec<String>,
    /// The reasoning behind the verdict — one entry per peek attempt that emitted
    /// thinking, in attempt order (docs/redaction-fork.md). The loop lands these on the
    /// gate's `Triage` (`thinking`), which is the only record of why a verdict came out
    /// the way it did; the fork's transcript is still discarded.
    pub thinking: Vec<String>,
}

impl GateOutcome {
    /// The hard-error outcome for a whole gate (docs/redaction-fork.md): the peek
    /// could not elicit a usable verdict (a transport failure, or in-fork retry
    /// exhaustion), so every result projects [`WireState::TriageFailed`] — an error
    /// in place of the output, `is_error` forced true, exactly as if the call itself
    /// had failed. The full untriaged raw is **never** the fallback: `WireState` is
    /// fixed once, so an uncurated result would stay in the cached prefix for the
    /// rest of the session with every later turn paying for it. `raw` stays in the
    /// log; the agent decides whether to re-issue the call, and the error is visible
    /// to the operator in the transcript like any failed call.
    ///
    /// `why` records the cause — it renders in each error result and lands in each
    /// result's `raw_keep`, the field the `Triage` entry carries for audit. Without
    /// it a failure is anonymous: a malformed verdict, a wrong-tool call, and a
    /// transport error all read identically after the fact.
    pub fn failed(count: usize, usage: Vec<RequestUsage>, why: &str) -> Self {
        Self {
            results: (0..count)
                .map(|_| Distillation {
                    wire: WireState::TriageFailed {
                        why: why.to_string(),
                    },
                    origin: Origin::Fallback,
                    raw_keep: why.to_string(),
                })
                .collect(),
            inbox: Vec::new(),
            background: Vec::new(),
            surfaced: Vec::new(),
            usage,
            retries: Vec::new(),
            thinking: Vec::new(),
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
    /// One entry per request the peek made (in-fork retries included), like
    /// [`GateOutcome::usage`]; empty for a model-free distiller or a transport failure.
    pub usage: Vec<RequestUsage>,
    /// Non-compliant-attempt breadcrumbs, like [`GateOutcome::retries`].
    pub retries: Vec<String>,
    /// Per-attempt peek reasoning, like [`GateOutcome::thinking`].
    pub thinking: Vec<String>,
}

/// The model peek — the universal triage gate (docs/redaction-fork.md) realized.
///
/// The gate is a concrete type, not a trait object. It was built behind a
/// single-method `Distiller` trait threaded as a generic bound through
/// `agent::run`, `daemon::serve`, and `run_idle_triage`; the second implementation
/// that bound existed for never arrived, and the one that briefly did (a model-free
/// `HeadTail` stub) had no call sites, tests included. Swapping the *model* behind
/// the peek is [`Self::own`] — a second `Client` and a peek-only `tools[]`, not
/// another implementation; a materially different peek *shape* (a compacted second
/// projection, a different transcript budget) is what would earn the indirection back.
///
/// Two configurations, chosen at boot (docs/redaction-fork.md):
///
/// - [`Self::shared`] — the default. The peek borrows the agent turn's client and
///   `tools[]`, so it sends the agent's envelope with a different tail and reads the
///   agent lineage's cache. Nothing the request keys a cache on differs.
/// - [`Self::own`] — the triage model. The peek has its own client (its own model,
///   system prompt, and session key) and a `tools[]` of just the peek tools. Its
///   prefix is the same canonical projection, so it has its own warm lineage, but it
///   shares nothing with the agent's.
pub struct ForkPeek {
    /// The triage model's client and tools when the peek runs on its own model;
    /// `None` borrows the agent turn's.
    own: Option<OwnPeek>,
}

/// The separate-model peek's request surface: everything cache-keyed that the shared
/// configuration would have borrowed from the agent turn.
struct OwnPeek {
    client: Client,
    tools: Vec<Value>,
    /// Force the asked-for tool (`tool_choice: {type: tool}`) rather than `auto`.
    /// The default: the first separate-model run opened five of eight gates with an
    /// invented work call (backlog/distiller-experiment.md), and forcing removes that
    /// path by construction. `auto` is the knob, kept for the ablation and for a
    /// Claude triage model that should think (a forced choice disables thinking).
    force_choice: bool,
}

/// The separate-model peek's output cap (docs/redaction-fork.md): a verdict is one
/// small tool call, so reserving the model's whole output cap against the window
/// only brings the overflow forward — a 65k cap on a 131k model overran at a 66k
/// prompt. Generous enough for a thinking triage model's reasoning.
pub const PEEK_MAX_OUTPUT: u64 = 16_384;

impl ForkPeek {
    /// The shared-lineage peek: the agent's client and `tools[]`, borrowed per call.
    pub fn shared() -> Self {
        ForkPeek { own: None }
    }

    /// The separate-model peek over `client`. Its `tools[]` is the two peek tools —
    /// no world tools, since nothing is ever asked of them in a peek, and this
    /// lineage shares no cache with the agent turn to keep the arrays identical for.
    pub fn own(client: Client) -> Self {
        ForkPeek {
            own: Some(OwnPeek {
                client,
                tools: vec![retain_tool(), triage_tool()],
                force_choice: true,
            }),
        }
    }

    /// Send the own peek's ask under `tool_choice: auto` instead of forcing the tool
    /// — the ablation knob. No effect on the shared peek, which is always `auto`.
    pub fn with_auto_choice(mut self) -> Self {
        if let Some(own) = &mut self.own {
            own.force_choice = false;
        }
        self
    }

    /// Whether the peek runs on its own model.
    pub fn is_own(&self) -> bool {
        self.own.is_some()
    }

    /// The model the peek runs on when it has its own, for the boot line and
    /// `GET /state`.
    pub fn own_model(&self) -> Option<&str> {
        self.own.as_ref().map(|o| o.client.model())
    }

    /// A user turn is starting: the own client forgets its turn-routing token the
    /// way the agent's does (`Client::begin_turn`, docs/transport.md). The shared
    /// peek rides the agent's client, whose token the loop already resets.
    pub fn begin_turn(&self) {
        if let Some(own) = &self.own {
            own.client.begin_turn();
        }
    }

    /// The client a peek sends through: the own one when set, else the agent's.
    fn client<'a>(&'a self, agent: &'a Client) -> &'a Client {
        self.own.as_ref().map_or(agent, |o| &o.client)
    }

    /// The `tools[]` a peek sends: the peek-only array when own, else the agent's.
    fn tools<'a>(&'a self, agent: &'a [Value]) -> &'a [Value] {
        self.own.as_ref().map_or(agent, |o| o.tools.as_slice())
    }

    /// The `tool_choice` a peek sends for `tool`: forced on the own peek unless
    /// [`Self::with_auto_choice`]; always `auto` on the shared peek, whose envelope
    /// must match the agent turn's (docs/redaction-fork.md).
    fn choice(&self, tool: &'static str) -> ToolChoice {
        match &self.own {
            Some(own) if own.force_choice => ToolChoice::force(tool),
            _ => ToolChoice::auto(),
        }
    }

    /// Decide the wire form of the results a completion produced, and triage the inbox +
    /// live background processes (docs/redaction-fork.md, docs/inbox.md, docs/supervisor.md).
    /// `tools` is the turn's full `tools[]` — the fork sends the *same* array as the agent
    /// turn so both shapes share one cache lineage (docs/redaction-fork.md); `prefix` is the
    /// log through the assistant turn that made the calls (the fork's cached head);
    /// `pending` is every result that turn produced, in call order; `inbox` is the pending
    /// notifications and `background` the running processes' fresh tails to show.
    ///
    /// Taking `pending` as a slice rather than one result is what makes the peek valid for a
    /// multi-call completion: the trailing assistant message carries N `tool_use` blocks, so
    /// the peek's tail must answer all N (docs/redaction-fork.md). One gate, N verdicts.
    pub async fn distill(
        &self,
        agent: &Client,
        agent_tools: &[Value],
        prefix: &[LogEntry],
        pending: &[Pending<'_>],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> GateOutcome {
        if pending.is_empty() {
            return GateOutcome::default();
        }
        // The ids this gate is asking about, in call order — the `retain` verdict's
        // coordinate system, minus the results the shape rule bypassed. Coverage of
        // every offered id is part of verdict *validity* ([`require_coverage`]): an
        // incomplete verdict retries in-fork rather than leaving a result untriaged.
        if let Some(outcome) = bypassed_gate(pending) {
            return outcome;
        }
        let offered = offered_ids(pending);
        let shown_lines: Vec<(String, usize)> = pending
            .iter()
            .filter(|p| bypassed(p).is_none())
            .map(|p| (p.call.id.0.clone(), shown_line_count(p.raw)))
            .collect();

        let roster = surfaceable_roster(prefix);
        let messages = peek_messages(prefix, pending, inbox, background, &roster);

        let mut record = PeekRecord::default();
        let parsed = peek_with_retries(
            self.client(agent),
            messages,
            self.tools(agent_tools),
            self.choice(RETAIN),
            RETAIN,
            |c| {
                let verdict = parse_retain(c, &offered).ok_or_else(|| unusable(c, RETAIN))?;
                let verdict = require_coverage(verdict, &offered)?;
                require_in_bounds(verdict, &shown_lines)
            },
            &mut record,
        )
        .await;

        let verdict = match parsed {
            Ok(v) => v,
            Err(why) => {
                // No usable verdict. The results are withheld as hard errors
                // (docs/redaction-fork.md) — never kept untriaged, which would weld
                // the full raw into the cached prefix for the rest of the session.
                // The error is on the wire, so the agent can re-issue the call and
                // the operator sees the failure in the transcript; the raw stays in
                // the log, and the breadcrumb classifies what came back.
                log::error!(
                    "fork peek produced no usable verdict for {} result(s); withholding \
                     output as a hard error — {why}",
                    offered.len()
                );
                let mut outcome = GateOutcome::failed(pending.len(), record.usage, &why);
                outcome.retries = record.retries;
                // A gate that failed is the one whose reasoning most needs reading.
                outcome.thinking = record.thinking;
                return outcome;
            }
        };

        // Apply the model's verdicts in call order. Coverage was validated above, so
        // every pending result finds its verdict; ids the model invented simply find
        // no home — the same harness-side guard applied to notification ids and
        // roster names below.
        let results = pending
            .iter()
            .map(|p| match bypassed(p) {
                // Decided by the shape rule before the fork was consulted
                // (docs/redaction-fork.md) — a `retain` entry for it, had the peek
                // written one unasked, finds no home.
                Some(shape) => bypass_verdict(shape),
                None => match verdict.results.iter().find(|v| v.call_id == p.call.id.0) {
                    Some(v) => {
                        let wire = WireState::Distilled(Distilled {
                            keep: v.keep.clone(),
                            note: v.note.clone(),
                            intent_feedback: v.intent_feedback.clone(),
                        });
                        Distillation {
                            wire,
                            origin: Origin::Model,
                            raw_keep: v.raw_keep.clone(),
                        }
                    }
                    // Unreachable in practice (coverage validated); defensively
                    // withhold rather than keep untriaged output.
                    None => Distillation {
                        wire: WireState::TriageFailed {
                            why: "no verdict for this result".into(),
                        },
                        origin: Origin::Fallback,
                        raw_keep: String::new(),
                    },
                },
            })
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
        let offered_names: HashSet<&str> = roster.iter().map(roster_name).collect();
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
            usage: record.usage,
            retries: record.retries,
            thinking: record.thinking,
        }
    }

    /// The standalone idle-triage peek (docs/supervisor.md, docs/inbox.md): fork off
    /// the cached prefix, show the inbox + background with no tool result, and ask for
    /// the `triage` tool via the tail instruction (no `keep` — there is no tool
    /// output). The caller wakes this when an event arrives while idle and escalates to
    /// a real turn iff it pins something. On any failure it pins/dismisses nothing —
    /// the agent simply goes back to idle, losing no data.
    pub async fn triage(
        &self,
        agent: &Client,
        agent_tools: &[Value],
        prefix: &[LogEntry],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> InboxTriage {
        let mut view = String::new();
        append_inbox_and_background(&mut view, inbox, background);
        view.push_str("\n\n");
        view.push_str(TRIAGE_INSTRUCTION);

        let mut messages = project_semantic(prefix);
        messages.push(WireTurn::Breakpoint);
        messages.push(WireTurn::text(view));

        let mut record = PeekRecord::default();
        let parsed = peek_with_retries(
            self.client(agent),
            messages,
            self.tools(agent_tools),
            self.choice(TRIAGE),
            TRIAGE,
            |c| parse_dispositions(c, TRIAGE).ok_or_else(|| unusable(c, TRIAGE)),
            &mut record,
        )
        .await;

        match parsed {
            Ok((ibx, bg)) => {
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
                    usage: record.usage,
                    retries: record.retries,
                    thinking: record.thinking,
                }
            }
            Err(why) => {
                log::warn!("idle triage peek produced no usable verdict; back to idle — {why}");
                InboxTriage {
                    inbox: Vec::new(),
                    background: Vec::new(),
                    origin: Origin::Fallback,
                    usage: record.usage,
                    retries: record.retries,
                    thinking: record.thinking,
                }
            }
        }
    }
}

// ---- the verdict ----

/// Triaged dispositions: inbox items (seq-keyed) and background processes (handle-keyed).
type Dispositions = (Vec<(Seq, Disposition)>, Vec<(String, BgVerdict)>);

#[cfg(test)]
mod test;

mod handles;
mod own;
mod parse;
mod peek;
mod prompt;
mod replay;
mod schema;
mod validate;

use handles::*;
use parse::*;
use peek::*;
use prompt::*;
use validate::*;

pub use own::TriageModel;
pub use replay::{PeekAttempt, ReplayVerdict, peek_request};
pub use schema::{retain_tool, triage_tool};
