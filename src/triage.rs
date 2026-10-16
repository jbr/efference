//! TriageOutcome — what a raw tool result projects to on the canonical wire
//! (docs/triage.md, docs/README.md), and the inbox triage that rides the
//! same triage (docs/inbox.md).
//!
//! Every tool result is triaged at append; the durable wire never carries raw
//! output, and a result's `wire` is fixed once and never mutated (so caching is
//! safe by construction). The triager's job is to produce a **verdict** — which
//! lines survive (`keep`) plus a `note` — not rendered bytes: the wire form is
//! re-rendered from `raw` + the verdict at projection time
//! ([`crate::project::render_triaged`], docs/triage.md), so the kept bytes
//! are never stored twice and the structured decision doubles as the audit record.
//!
//! The same throwaway triage also triages the **inbox** (docs/inbox.md): the pending
//! ambient notifications are shown under the output, and the `retain` verdict's
//! `inbox` field pins / dismisses them by `[seq]`. The dispositions ride back on the
//! [`TriageOutcome`] and land on the logged [`Triage`](crate::log::Triage). One type
//! does the triaging:
//!
//! - [`Triager`] — the universal triage (docs/triage.md): fork a
//!   throwaway branch off the cached prefix, show the model the line-numbered raw
//!   output + the inbox, and ask it (via the tail instruction, never a forced
//!   `tool_choice`) to call `retain` to choose what the canonical wire keeps and which
//!   notifications to pin. The triage sends the *same* request envelope as an agent turn —
//!   one shared `tools[]`, `tool_choice: auto`, the same thinking policy — so both
//!   shapes read one cache lineage (docs/triage.md, docs/log-and-projection.md).
//!   A non-compliant response gets a bounded in-fork retry, then a fresh fork; an
//!   upstream failure takes the main loop's schedule. A triage that still has no verdict
//!   returns a [`HoldFailure`] and the loop **holds** the results — nothing reaches
//!   the agent until a verdict does (docs/triage.md). The full untriaged raw
//!   is never welded onto the canonical wire.

use std::collections::HashSet;

use crate::log::RequestOrigin;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{STREAM_ABORT_ATTEMPTS, stream_abort_backoff, stream_abort_retry};
use crate::api::{
    Client, Completion, PlanState, RequestUsage, RetrySurface, Streamed, ToolChoice,
    UpstreamFailureState, UpstreamRetryCell,
};
use crate::context::Item;
use crate::inbox::ShownNotification;
use crate::log::{
    AssistantOutput, Disposition, Keep, LogEntry, Origin, Payload, RawResult, Seq, ToolCall,
    Triaged, WireState,
};
use crate::process_monitor::BgView;
use crate::project::{Reader, TRIAGE_LINE_CAP, UserPart, WireTurn, project_semantic_for};

const RETAIN: &str = "retain";

/// The tool of the **standalone** idle triage (docs/process-monitor.md): no `keep`
/// (there's no tool output to retain), only inbox/background dispositions.
const TRIAGE: &str = "triage";

/// Whether `name` is a triage tool. Both stand in every turn's `tools[]` (the shared
/// envelope, docs/triage.md) but are answered only when a triage's tail
/// instruction asks — the agent loop pairs an un-asked call with a nudge result
/// instead of running the cascade.
pub fn is_triage_tool(name: &str) -> bool {
    name == RETAIN || name == TRIAGE
}

/// A live-process triage disposition (docs/process-monitor.md) — distinct from the seq-keyed
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
/// A triage covers every result a single completion produced (docs/triage.md),
/// so the loop hands the triager a slice of these rather than one at a time.
pub struct Pending<'a> {
    pub call: &'a ToolCall,
    pub raw: &'a RawResult,
}

/// The verdict on **one** tool result (docs/triage.md): the wire form plus the
/// audit pieces the loop stamps into that result's [`Triage`](crate::log::Triage)
/// entry. `raw` is never mutated — it stays in the log.
pub struct TriageOutcome {
    /// The canonical wire form — a [`WireState::Triaged`] verdict ([`Triaged`]:
    /// kept ranges, note, feedback), re-rendered from `raw` at projection time (docs/triage.md).
    pub wire: WireState,
    /// Whether the verdict is the model's or a harness fallback (docs/triage.md).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse — empty for a model-free triager.
    pub raw_keep: String,
}

/// The outcome of one triage (docs/triage.md). A triage rides the results of
/// a single completion — one or several calls (docs/README.md), all of them triaged
/// together in one triage. That grouping is what keeps the triage's request valid: every `tool_use` in
/// the trailing assistant message is answered by a real result in the same tail, so
/// nothing is ever orphaned.
///
/// The inbox / background / surfacing riders are **triage-scoped**, not per-result: they
/// are decided once for the step, which is why they sit here rather than on
/// [`TriageOutcome`]. `Default` is the outcome of a triage with nothing to triage.
#[derive(Default)]
pub struct HoldOutcome {
    /// One verdict per [`Pending`] handed in, in the same order. Always the same
    /// length as the input — a triage without a verdict for every result is a
    /// [`HoldFailure`], never a short vector — so the loop can zip them without checking.
    pub results: Vec<TriageOutcome>,
    /// Per-notification dispositions this triage chose, by arrival `seq` (docs/inbox.md) —
    /// filtered to notifications actually shown. Empty when nothing was pinned or
    /// dismissed (everything left for the next triage).
    pub inbox: Vec<(Seq, Disposition)>,
    /// Live-process dispositions this triage chose, by `p#<n>` handle (docs/process-monitor.md) —
    /// filtered to processes actually shown. The loop materializes a `Pin` into a
    /// snapshot `Ambient` and applies a `Mute` to the process monitor.
    pub background: Vec<(String, BgVerdict)>,
    /// Context-item names this triage chose to surface (docs/triage.md) —
    /// filtered to the roster actually shown (invented names dropped). Stored on the
    /// `Triage` (`surfaced`); `project` re-renders each as a read-me reminder. Empty
    /// when nothing was surfaced.
    pub surfaced: Vec<String>,
    /// The [`RequestUsage`] of every request this triage made — empty for a model-free
    /// triager, one entry per fork round-trip (in-fork retries included). Each fork
    /// request reads the shared canonical prefix (sliding its cache TTL), and the loop
    /// folds them all into the cache telemetry (docs/log-and-projection.md).
    pub usage: Vec<RequestUsage>,
    /// One attempt-stamped breadcrumb per triage attempt that had to be retried —
    /// recorded even when a retry recovered (docs/triage.md). Two families,
    /// distinguishable by wording: a non-compliant verdict (wrong tool, no tool,
    /// malformed input), and a stream that died before finalizing. The loop lands
    /// these on the triage's `Triage` (`retries`), the durable record; a runtime triage
    /// failure is never accepted silently, recovered or not.
    pub retries: Vec<String>,
    /// The reasoning behind the verdict — one entry per triage attempt that emitted
    /// thinking, in attempt order (docs/triage.md). The loop lands these on the
    /// triage's `Triage` (`thinking`), which is the only record of why a verdict came out
    /// the way it did; the fork's transcript is still discarded.
    pub thinking: Vec<String>,
    /// The triage's request, for the triage lineage's keepalive to re-send, where it
    /// re-sends result triages (docs/transport.md).
    pub sent: Option<SentTriage>,
}

/// A triage that produced no verdict (docs/triage.md): why, plus the audit
/// pieces its attempts accumulated, which the loop carries onto the `Triage` of the
/// attempt that finally succeeds. The results are never shown to the agent in this
/// state; the loop holds them.
pub struct HoldFailure {
    pub failure: TriageFailure,
    pub usage: Vec<RequestUsage>,
    pub retries: Vec<String>,
    pub thinking: Vec<String>,
    /// The last request that got a response, as on [`HoldOutcome::sent`]. Boxed: the
    /// failure is the `Err` of every triage's result.
    pub sent: Option<Box<SentTriage>>,
}

impl HoldFailure {
    fn new(failure: TriageFailure, record: TriageRecord) -> Self {
        Self {
            failure,
            usage: record.usage,
            retries: record.retries,
            thinking: record.thinking,
            sent: record.sent.map(Box::new),
        }
    }
}

impl HoldOutcome {
    /// The outcome of a triage the operator interrupted before any verdict
    /// (docs/control-channel.md): every result is withheld from the wire exactly as
    /// the interrupt's *after* juncture withholds one — [`WireState::Elided`], the raw
    /// kept in the log — and the operator's own message is the next thing the agent
    /// reads. Nothing here says triage failed, because from the agent's side nothing
    /// did: the operator stopped the turn.
    pub fn interrupted(count: usize, retries: Vec<String>, thinking: Vec<String>) -> Self {
        Self {
            results: (0..count)
                .map(|_| TriageOutcome {
                    wire: WireState::Elided,
                    origin: Origin::Fallback,
                    raw_keep: "interrupted by the operator before a verdict".to_string(),
                })
                .collect(),
            inbox: Vec::new(),
            background: Vec::new(),
            surfaced: Vec::new(),
            usage: Vec::new(),
            retries,
            thinking,
            sent: None,
        }
    }
}

/// The verdict of a **standalone** idle triage (docs/process-monitor.md) — a triage that rides no
/// tool result, so it has no [`WireState`]; just the inbox/background dispositions (and
/// the audit pieces the loop stamps into a [`Triage`](crate::log::Triage) with
/// `for_result: None`).
pub struct InboxTriage {
    pub inbox: Vec<(Seq, Disposition)>,
    pub background: Vec<(String, BgVerdict)>,
    pub origin: Origin,
    /// One entry per request the triage made (in-fork retries included), like
    /// [`HoldOutcome::usage`]; empty for a model-free triager or a transport failure.
    pub usage: Vec<RequestUsage>,
    /// Non-compliant-attempt breadcrumbs, like [`HoldOutcome::retries`].
    pub retries: Vec<String>,
    /// Per-attempt triage reasoning, like [`HoldOutcome::thinking`].
    pub thinking: Vec<String>,
}

/// The model triage — the universal triage (docs/triage.md) realized.
///
/// The triage is a concrete type, not a trait object. It was built behind a
/// single-method `Triager` trait threaded as a generic bound through
/// `agent::run`, `daemon::serve`, and `run_idle_triage`; the second implementation
/// that bound existed for never arrived, and the one that briefly did (a model-free
/// `HeadTail` stub) had no call sites, tests included. Swapping the *model* behind
/// the triage is [`Self::own`] — a second `Client` and a triage-only `tools[]`, not
/// another implementation; a materially different triage *shape* (a compacted second
/// projection, a different transcript budget) is what would earn the indirection back.
///
/// Two configurations, chosen at boot (docs/triage.md):
///
/// - [`Self::shared`] — the default. The triage borrows the agent turn's client and
///   `tools[]`, so it sends the agent's envelope with a different tail and reads the
///   agent lineage's cache. Nothing the request keys a cache on differs.
/// - [`Self::own`] — the triage model. The triage has its own client (its own model,
///   system prompt, and session key) and a `tools[]` of just the triage tools. Its
///   prefix is the same canonical projection, so it has its own warm lineage, but it
///   shares nothing with the agent's.
///
/// `Clone` hands the triager's surface to a detached task (the title ask,
/// docs/session-title.md): a `Client` clone shares the Arc-backed live states,
/// and the rest is configuration.
#[derive(Clone)]
pub struct Triager {
    /// The triage model's client and tools when the triage runs on its own model;
    /// `None` borrows the agent turn's.
    own: Option<OwnTriage>,
}

/// The separate-model triage's request surface: everything cache-keyed that the shared
/// configuration would have borrowed from the agent turn.
#[derive(Clone)]
struct OwnTriage {
    client: Client,
    tools: Vec<Value>,
    /// Force the asked-for tool (`tool_choice: {type: tool}`) rather than `auto`.
    /// The default: the first separate-model run opened five of eight triages with an
    /// invented work call (backlog/inbox/distiller-experiment.md), and forcing removes that
    /// path by construction. `auto` is the knob, kept for the ablation and for a
    /// Claude triage model that should think (a forced choice disables thinking).
    force_choice: bool,
}

/// The separate-model triage's output cap (docs/triage.md): a verdict is one
/// small tool call, so reserving the model's whole output cap against the window
/// only brings the overflow forward — a 65k cap on a 131k model overran at a 66k
/// prompt. Generous enough for a thinking triage model's reasoning.
pub const TRIAGE_MAX_OUTPUT: u64 = 16_384;

impl Triager {
    /// Make the own-model triage's reads answer to the operator's interrupt
    /// (docs/control-channel.md); the shared triage already rides the agent client.
    pub fn set_abort(&mut self, abort: std::sync::Arc<tokio::sync::Notify>) {
        if let Some(own) = &mut self.own {
            own.client.set_abort(abort);
        }
    }

    /// Report the own-model triage's request stages to `sink`
    /// (docs/log-and-projection.md); the shared triage rides the agent client.
    pub fn set_stage_sink(&mut self, sink: crate::api::StageSink) {
        if let Some(own) = &mut self.own {
            own.client.set_stage_sink(sink);
        }
    }

    /// The shared-lineage triage: the agent's client and `tools[]`, borrowed per call.
    pub fn shared() -> Self {
        Triager { own: None }
    }

    /// The separate-model triage over `client`. Its `tools[]` is the two triage tools —
    /// no world tools, since nothing is ever asked of them in a triage, and this
    /// lineage shares no cache with the agent turn to keep the arrays identical for.
    pub fn own(client: Client) -> Self {
        Triager {
            own: Some(OwnTriage {
                client,
                tools: vec![retain_tool(), triage_tool()],
                force_choice: true,
            }),
        }
    }

    /// Send the own triage's ask under `tool_choice: auto` instead of forcing the tool
    /// — the ablation knob. No effect on the shared triage, which is always `auto`.
    pub fn with_auto_choice(mut self) -> Self {
        if let Some(own) = &mut self.own {
            own.force_choice = false;
        }
        self
    }

    /// The own triage client's failure and plan cells, for `GET /state`
    /// (`triage_upstream_failure`, `usage.triage_plan`; docs/control-api.md). `None` when
    /// the triage borrows the agent's client, whose cells the unprefixed topics already
    /// publish.
    pub fn client_state(&self) -> Option<(UpstreamFailureState, PlanState)> {
        self.own
            .as_ref()
            .map(|o| (o.client.upstream_failure(), o.client.plan_state()))
    }

    /// Whether the triage runs on its own model.
    pub fn is_own(&self) -> bool {
        self.own.is_some()
    }

    /// The projection this triage reads (docs/summary.md): the triage view on its own
    /// client, the main view on the agent's. A shared triage reads the agent's cached
    /// prefix, so it can have no fold the agent's wire does not.
    pub fn reader(&self) -> Reader {
        if self.is_own() {
            Reader::Triage
        } else {
            Reader::Main
        }
    }

    /// The view this triage's result requests read (docs/triage.md): the agent's
    /// transcript on the shared triage, whose fork reads the agent's cached prefix; the
    /// text view on its own model, which shares no cache with the agent and may speak
    /// another wire format.
    pub fn view(&self) -> TriageView {
        if self.is_own() {
            TriageView::Text
        } else {
            TriageView::Transcript
        }
    }

    /// The model the triage runs on when it has its own, for the boot line and
    /// `GET /state`.
    pub fn own_model(&self) -> Option<&str> {
        self.own.as_ref().map(|o| o.client.model())
    }

    /// A user turn is starting: the own client forgets its turn-routing token the
    /// way the agent's does (`Client::begin_turn`, docs/transport.md). The shared
    /// triage rides the agent's client, whose token the loop already resets.
    pub fn begin_turn(&self) {
        if let Some(own) = &self.own {
            own.client.begin_turn();
        }
    }

    /// The client a triage sends through: the own one when set, else the agent's,
    /// tagged with what the request is for (docs/log-and-projection.md).
    fn client(&self, agent: &Client, origin: RequestOrigin) -> Client {
        self.own
            .as_ref()
            .map_or(agent, |o| &o.client)
            .with_origin(origin)
    }

    /// The `tools[]` a triage sends: the triage-only array when own, else the agent's.
    fn tools<'a>(&'a self, agent: &'a [Value]) -> &'a [Value] {
        self.own.as_ref().map_or(agent, |o| o.tools.as_slice())
    }

    /// The `tool_choice` a triage sends for `tool`: forced on the own triage unless
    /// [`Self::with_auto_choice`]; always `auto` on the shared triage, whose envelope
    /// must match the agent turn's (docs/triage.md).
    fn choice(&self, tool: &'static str) -> ToolChoice {
        match &self.own {
            Some(own) if own.force_choice => ToolChoice::force(tool),
            _ => ToolChoice::auto(),
        }
    }

    /// The envelope of the standalone idle triage: the agent's client, the agent's
    /// `tools[]`, and `auto`, whether or not a separate triage model is configured
    /// (docs/triage.md). The idle ask decides whether the session wakes at
    /// all, and a wrong dismiss there is silent; it runs on the agent's own lineage,
    /// at the agent's thinking, reading the cache the keepalive holds warm. The
    /// separate model keeps the per-result triages, which are the volume.
    fn idle_envelope<'a>(
        &self,
        agent: &'a Client,
        agent_tools: &'a [Value],
    ) -> (&'a Client, &'a [Value], ToolChoice) {
        (agent, agent_tools, ToolChoice::auto())
    }

    /// Decide the wire form of the results a completion produced, and triage the inbox +
    /// live background processes (docs/triage.md, docs/inbox.md, docs/process-monitor.md).
    /// `tools` is the turn's full `tools[]` — the fork sends the *same* array as the agent
    /// turn so both shapes share one cache lineage (docs/triage.md); `prefix` is the
    /// log through the assistant turn that made the calls (the fork's cached head);
    /// `pending` is every result that turn produced, in call order; `inbox` is the pending
    /// notifications and `background` the running processes' fresh tails to show.
    ///
    /// Taking `pending` as a slice rather than one result is what makes the triage valid for a
    /// multi-call completion: the trailing assistant message carries N `tool_use` blocks, so
    /// the triage's tail must answer all N (docs/triage.md). One triage, N verdicts.
    #[allow(clippy::too_many_arguments)] // the turn's results, the shown riders, and the stream sink
    pub async fn triage_results(
        &self,
        agent: &Client,
        agent_tools: &[Value],
        prefix: &[LogEntry],
        pending: &[Pending<'_>],
        inbox: &[ShownNotification],
        background: &[BgView],
        on_delta: &(dyn Fn(Streamed) + Sync),
    ) -> Result<HoldOutcome, HoldFailure> {
        if pending.is_empty() {
            return Ok(HoldOutcome::default());
        }
        // The labels this triage is asking about, in call order — the `retain` verdict's
        // coordinate system ([`labels`]), minus the results the shape rule bypassed.
        // Coverage of every offered label is part of verdict *validity*
        // ([`require_coverage`]): an incomplete verdict retries in-fork rather than
        // leaving a result untriaged.
        if let Some(outcome) = bypassed_triage(pending) {
            return Ok(outcome);
        }
        let labels = labels(pending);
        let offered = offered_labels(pending);
        let shown_lines = shown_lines(pending, &labels);

        let messages = triage_request(
            prefix,
            self.reader(),
            self.view(),
            pending,
            inbox,
            background,
        );
        let roster = surfaceable_roster(prefix);

        let client = self.client(agent, RequestOrigin::Triage);
        let tools = self.tools(agent_tools);
        let choice = self.choice(RETAIN);
        // Kept for the triage keepalive, which re-sends the request as sent
        // (docs/transport.md); the stamp precedes the send, as the primer's does.
        let resend = self
            .resends_results()
            .then(|| (messages.clone(), crate::log::Timestamp::now()));
        let mut record = TriageRecord::default();
        let parsed = triage_with_retries(
            &client,
            &agent.upstream_retry(),
            Patience::TRIAGE,
            messages,
            tools,
            choice,
            &call_now(RETAIN),
            |c| {
                let verdict = parse_retain(c, &offered).ok_or_else(|| unusable(c, RETAIN))?;
                let verdict = require_coverage(verdict, &offered)?;
                require_in_bounds(verdict, &shown_lines)
            },
            &mut record,
            on_delta,
        )
        .await;
        record.sent = resend
            .and_then(|(turns, at)| self.sent_triage(&client, turns, tools, choice, at, &record));

        let verdict = match parsed {
            Ok(v) => v,
            // No verdict: the loop holds the results (docs/triage.md) — never
            // kept untriaged, which would weld the full raw into the cached prefix, and
            // never withheld as an error the agent would have to reason about.
            Err(failure) => return Err(HoldFailure::new(failure, record)),
        };

        // Apply the model's verdicts in call order. Coverage was validated above, so
        // every pending result finds its verdict; labels the model invented simply find
        // no home — the same harness-side guard applied to notification ids and
        // roster names below.
        let results = pending
            .iter()
            .zip(&labels)
            .map(|(p, label)| match bypassed(p) {
                // Decided by the shape rule before the fork was consulted
                // (docs/triage.md) — a `retain` entry for it, had the triage
                // written one unasked, finds no home.
                Some(shape) => Some(bypass_verdict(shape)),
                None => match verdict
                    .results
                    .iter()
                    .find(|v| Some(&v.label) == label.as_ref())
                {
                    Some(v) => {
                        let wire = WireState::Triaged(Triaged {
                            keep: v.keep.clone(),
                            note: v.note.clone(),
                            intent_feedback: v.intent_feedback.clone(),
                        });
                        Some(TriageOutcome {
                            wire,
                            origin: Origin::Model,
                            raw_keep: v.raw_keep.clone(),
                        })
                    }
                    // Unreachable in practice (coverage validated); a gap is a
                    // harness defect, answered like any other triage without a verdict.
                    None => None,
                },
            })
            .collect::<Option<Vec<_>>>();
        let Some(results) = results else {
            return Err(HoldFailure::new(
                TriageFailure::Unusable("a validated verdict left a result unanswered".into()),
                record,
            ));
        };

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

        Ok(HoldOutcome {
            results,
            inbox: dispositions,
            background: bg,
            surfaced,
            usage: record.usage,
            retries: record.retries,
            thinking: record.thinking,
            sent: record.sent,
        })
    }

    /// The standalone idle triage (docs/process-monitor.md, docs/inbox.md): fork off
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
        on_delta: &(dyn Fn(Streamed) + Sync),
    ) -> InboxTriage {
        let mut view = String::new();
        append_inbox_and_background(&mut view, inbox, background);
        view.push_str("\n\n");
        view.push_str(ASK_BANNER);
        view.push_str("\n\n");
        view.push_str(TRIAGE_INSTRUCTION);

        // The agent's projection: the idle triage sends on the agent's client.
        let mut messages = project_semantic_for(prefix, Reader::Main);
        messages.push(WireTurn::Breakpoint);
        messages.push(WireTurn::text(view));

        let mut record = TriageRecord::default();
        let (client, tools, choice) = self.idle_envelope(agent, agent_tools);
        let client = client.with_origin(RequestOrigin::Triage);
        let parsed = triage_with_retries(
            &client,
            &agent.upstream_retry(),
            Patience::ASIDE,
            messages,
            tools,
            choice,
            &call_now(TRIAGE),
            |c| parse_dispositions(c, TRIAGE).ok_or_else(|| unusable(c, TRIAGE)),
            &mut record,
            on_delta,
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
                log::warn!("idle triage produced no usable verdict; back to idle — {why}");
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

mod ask_variant;
mod digest_view;
mod handles;
mod line_view;
mod own;
mod parse;
mod primer;
mod prompt;
mod replay;
mod request;
mod resend;
mod schema;
mod text_view;
mod title;
mod validate;

use ask_variant::*;
use handles::*;
use line_view::*;
use parse::*;
use prompt::*;
use request::*;
use validate::*;

pub use digest_view::TriageView;
pub use handles::{reply_only_triage, shows_rider};
pub use own::TriageModel;
pub use primer::{TriagePrimerInput, TriagePrimerOutcome};
pub use replay::{ReplayVerdict, TriageAttempt, triage_request};
pub use request::{TRIAGE_RESTARTS, TriageFailure};
pub use resend::SentTriage;
pub use schema::{retain_tool, triage_tool};
pub use text_view::{
    TRIAGED_TAIL_HIGH_WATER, TRIAGED_TAIL_LOW_WATER, TextViewLayout, text_view_layout,
};
pub use title::{TITLE_MAX_CHARS, TitlePeek};
