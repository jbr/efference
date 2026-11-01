//! The JSON readouts served to clients by `/state` (docs/control-api.md).
//!
//! `/state` is decomposed into one function per [`Topic`], each building the topic's
//! protocol type, and [`Session::state_topics`] is their composition. That split is what
//! lets the composite snapshot and the level-triggered `state` frames come from the same
//! source, so `GET /state` equals the fold of every state frame since connect rather than
//! merely agreeing with it by inspection.
//!
//! Each topic function returns a whole value, never a patch: a `state` frame carries a
//! topic's entire current value and `null` means *cleared*, so nothing here may signal
//! "unchanged" by omission.

use super::*;
use crate::inference::api::UpstreamFailure;
use crate::protocol::channel_value;
use crate::protocol::state::{
    ContextWindow, Cost, Gate, LoopResource, PaceView, RoleView, StateTopics, UsageView,
};
use crate::protocol::status::StatusView;
use crate::protocol::turns::QueuedTurn;

impl Session {
    /// One topic's entire current value — the body of a `state` frame, and one field of
    /// the composite snapshot.
    pub(crate) fn topic_json(&self, topic: Topic) -> Value {
        match topic {
            Topic::Gate => channel_value(self.gate()),
            Topic::Ask => channel_value(self.asking()),
            Topic::Processes => channel_value(self.process_states()),
            Topic::Usage => channel_value(self.usage()),
            Topic::Role => channel_value(self.role_view()),
            Topic::Proposals => channel_value(self.pending_proposals()),
            Topic::Notifications => channel_value(self.pending_notifications()),
            Topic::Pace => channel_value(self.pace_view()),
            Topic::ReviewLevel => channel_value(self.review_level()),
            Topic::ResolvedReviewLevel => channel_value(self.resolved_review_level()),
            Topic::Supervisor => channel_value(self.supervisor()),
            Topic::LoopPhase => channel_value(self.loop_phase()),
            Topic::Verbs => channel_value(self.verbs()),
            Topic::Queued => channel_value(self.queued()),
            Topic::Stall => channel_value(self.stall()),
            Topic::UpstreamFailure => channel_value(self.upstream_failure()),
            Topic::TriageUpstreamFailure => channel_value(self.triage_upstream_failure()),
            Topic::UpstreamRetry => channel_value(self.upstream_retry.get()),
            Topic::Cold => channel_value(self.cold()),
            Topic::CompactionFailure => channel_value(self.compaction_failure()),
            Topic::ConfigRepair => channel_value(self.config_repair()),
            Topic::SessionIdentity => channel_value(self.identity()),
            Topic::CurrentOperations => channel_value(self.current_operations()),
            // A session exists only once it has started (`starting.rs`).
            Topic::Starting => Value::Null,
            Topic::Status => channel_value(self.status_topic()),
            Topic::NewerVersion => channel_value(self.newer_version()),
        }
    }

    /// The composite `GET /state` representation: every topic under its own name, plus
    /// `last_seq`, each built by the function [`Self::topic_json`] serializes.
    ///
    /// `last_seq` is deliberately **not** a topic — it is the log's cursor rather than
    /// control state, redundant for any client taking `log` frames, and useful to a
    /// `?include=state` subscriber only as a bootstrap hint. It therefore sits outside
    /// the fold identity above.
    pub(crate) fn state_topics(&self) -> StateTopics {
        StateTopics {
            last_seq: self.last_committed_seq(),
            gate: self.gate(),
            ask: self.asking(),
            processes: self.process_states(),
            usage: self.usage(),
            role: self.role_view(),
            proposals: self.pending_proposals(),
            notifications: self.pending_notifications(),
            pace: self.pace_view(),
            review_level: self.review_level(),
            resolved_review_level: self.resolved_review_level(),
            supervisor: self.supervisor(),
            loop_phase: self.loop_phase(),
            verbs: self.verbs(),
            queued: self.queued(),
            stall: self.stall(),
            upstream_failure: self.upstream_failure(),
            triage_upstream_failure: self.triage_upstream_failure(),
            upstream_retry: self.upstream_retry.get(),
            cold: self.cold(),
            compaction_failure: self.compaction_failure(),
            config_repair: self.config_repair(),
            session_identity: self.identity(),
            current_operations: self.current_operations(),
            starting: None,
            status: self.status_topic(),
            newer_version: self.newer_version(),
        }
    }

    /// `GET /state`'s body: [`Self::state_topics`] as the channel carries it.
    pub(crate) fn state_json(&self) -> String {
        channel_value(self.state_topics()).to_string()
    }

    /// The loop resource (`GET /loop`, docs/control-channel.md): the operator dials
    /// and the read-only `loop_phase`, each field the same value — and the same
    /// name — as its state topic, so the resource and the topics cannot drift.
    pub(crate) fn loop_json(&self) -> String {
        channel_value(LoopResource {
            pace: self.pace_view(),
            review_level: self.review_level(),
            loop_phase: self.loop_phase(),
        })
        .to_string()
    }

    /// The highest committed `seq` — the watermark a level-triggered `state` frame is
    /// stamped with, and the `last_seq` of the composite snapshot. `None` on an empty
    /// log.
    pub(crate) fn last_committed_seq(&self) -> Option<u64> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .len()
            .checked_sub(1)
            .map(|i| i as u64)
    }

    /// The `gate` topic: the pending approval gate, or `None` when none is open.
    fn gate(&self) -> Option<Gate> {
        self.pending
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| pending.gate.clone())
    }

    /// The `usage` topic: token/cost telemetry (docs/transport.md).
    fn usage(&self) -> UsageView {
        // Taken before `metrics` is locked: it locks the log, and nothing else in the
        // daemon acquires the two in the other order. Keep this ordering if this
        // function is ever split further.
        let retention = self.retention();
        let metrics = self.metrics.lock().unwrap();
        // Context-window occupancy (docs/transport.md): the last **main-loop**
        // request's total prompt tokens — uncached input + cache reads + cache
        // writes; `input_tokens` alone is only the uncached remainder — against
        // the boot-fetched window itself. The output reservation is deliberately
        // not subtracted: it shrinks as the prompt grows (`api/reservation.rs`),
        // so a limit computed from it moves through the session and reads as a
        // ceiling the prompt never reaches. The prompt stops fitting
        // `MARGIN + RESERVATION_MIN` short of the window, so occupancy approaches
        // 100% rather than reaching it. Deliberately `last_turn_usage`, not
        // `last_usage`: the side channels each carry their own, much smaller
        // prefix, so folding them here makes occupancy fall mid-turn and correct
        // itself afterwards. `used` is null until the first *turn*; `context` is
        // null entirely when no capabilities were fetched (tests, offline verbs),
        // and `limit` null on its own when they were fetched from a source that
        // reported no window (a custom endpoint, docs/transport.md).
        let context = self
            .capabilities
            .lock()
            .unwrap()
            .as_ref()
            .map(|caps| ContextWindow {
                limit: caps.context_length,
                used: metrics.last_turn_usage.as_ref().map(Usage::prompt_tokens),
            });
        // Session cost (docs/transport.md): the billed running total from the
        // provider's accounting endpoint, null when the provider has none
        // (direct Anthropic). `unpriced` counts requests the fetch couldn't
        // price, so a nonzero value marks the total as an undercount.
        let cost = {
            let cell = self.cost.lock().unwrap();
            cell.endpoint.as_ref().map(|_| Cost {
                usd: cell.total_usd,
                unpriced: cell.unpriced,
            })
        };
        UsageView {
            last: metrics.last_usage.clone(),
            cumulative: Usage {
                input_tokens: metrics.total_input,
                output_tokens: metrics.total_output,
                cache_read_input_tokens: metrics.total_cache_read,
                cache_creation_input_tokens: metrics.total_cache_creation,
            },
            // Absolute, not elapsed (docs/control-api.md): a ticking field would make
            // this topic re-emit on every read under level-triggered emission. Clients
            // subtract from their own clock.
            last_request_at: metrics.last_request_wall,
            context,
            cost,
            // The live end of the empirical prefix oracle
            // (docs/log-and-projection.md): consecutive main-loop requests that read
            // nothing from the cache and had no legitimate reason to (not boot, not a
            // model change, not idle past the TTL). A number, not a flag, so the
            // threshold stays a client decision; 0 whenever the last turn read warm.
            cold_streak: metrics.cold_streak.streak(),
            // The same count over the separate triage lineage's reads (triage results
            // and primers); null while the triage borrows the agent's client.
            triage_cold_streak: self
                .triage_client
                .get()
                .map(|_| metrics.triage_cold_streak.streak()),
            // What the provider said about the most recent miss it attributed
            // (docs/transport.md): the *why* beside the streak's *how often*. Null until
            // one arrives, and the reason is the provider's own spelling — the two that
            // mean no comparison was produced read as themselves here rather than being
            // filtered, so a client states "no comparison" instead of showing nothing.
            // Absolute stamp, like `last_request_at`, and for the same reason.
            last_cache_miss: metrics.last_cache_miss.clone(),
            // The keepalive's stance when it is not pinging on schedule, or
            // null while it is or was never armed (docs/transport.md).
            keepalive_skipped: metrics.keepalive_skipped.clone(),
            // The triage lineage's keepalive stance (docs/transport.md), by the same rule.
            triage_keepalive_skipped: metrics.triage_keepalive_skipped.clone(),
            // The Codex backend's subscription windows (docs/transport.md): the
            // plan and its two usage windows as of the last response that named
            // them, resets as absolute millis. null on every other provider.
            plan: self.plan.lock().unwrap().clone(),
            // The separate triage client's own windows, in the same shape; null while
            // the triage borrows the agent's client.
            triage_plan: self
                .triage_client
                .get()
                .and_then(|(_, plan)| plan.lock().unwrap().clone()),
            // Triage retention (docs/triage.md): how much of what the triage
            // showed survives on the wire, derived from the log rather than from
            // transport telemetry. `withheld` counts results whose output a failed
            // triage suppressed — a distinct condition from a low ratio, and kept out
            // of it, so a failing triage can't read as excellent compression.
            retention,
        }
    }

    /// The `pace` topic: the operator's pace dial (docs/control-channel.md) — loud to the
    /// operator's clients, silent to the agent (never logged/projected). `speed_ms` is
    /// the between-turns delay; `held` is the pause overlay.
    fn pace_view(&self) -> PaceView {
        let (speed, held) = self.pace_snapshot();
        PaceView {
            speed_ms: speed.as_millis() as u64,
            held,
        }
    }

    /// The `queued` topic: the pending turn queue in submission order, empty when
    /// nothing is queued (docs/control-api.md). A pending turn is ephemeral control
    /// state — never logged, never projected — so this is the only place it is readable,
    /// and it is what a client's optimistic echo and its recall affordance both address.
    fn queued(&self) -> Vec<QueuedTurn> {
        self.queued_turns().iter().map(QueuedTurn::from).collect()
    }

    /// The `upstream_failure` topic: the last non-success upstream response
    /// (docs/transport.md). Cleared by the next 2xx, which is the recovery boundary the
    /// retry topic shares.
    fn upstream_failure(&self) -> Option<UpstreamFailure> {
        self.upstream_failure.lock().unwrap().clone()
    }

    /// The `triage_upstream_failure` topic: the separate triage client's last
    /// non-success response, in the shape of [`upstream_failure`](Self::upstream_failure).
    /// `None` both when the triage borrows the agent's client and when its own client is
    /// clear.
    fn triage_upstream_failure(&self) -> Option<UpstreamFailure> {
        self.triage_client
            .get()
            .and_then(|(failure, _)| failure.lock().unwrap().clone())
    }

    /// The `status` topic: where the session stands, as one view (docs/control-api.md).
    fn status_topic(&self) -> StatusView {
        let id = self.identity().map_or_default(|identity| identity.id);
        self.status_view(&id, &self.status())
    }

    /// The `role` topic: the **effective** policy — the base config plus every committed
    /// config edit folded in, i.e. the *live* perimeter, not the boot config
    /// (docs/permissions.md). It carries full **contents**, not counts: an orchestrator
    /// inspecting a subordinate (or a human checking "what am I running?") needs the
    /// actual commands/paths/scope in force — and the config is bounded-small by design,
    /// never projected, with no secret to withhold. `scope` and `watch-for` are surfaced
    /// here because the effective scope can drift from the frozen boot preamble (a live
    /// scope edit) and `watch-for` is on no other wire. `source` is the base config's
    /// provenance (path, or `None` for the built-in default).
    fn role_view(&self) -> RoleView {
        let source = match &self.role.source {
            ConfigSource::Default => None,
            ConfigSource::Path(p) => Some(p.display().to_string()),
        };
        // The live policy, not the frozen base: a session grant or an fs-watch edit moves the
        // real perimeter, and reading `self.role` here would report the boot config instead.
        let role = self.effective_config();
        RoleView {
            source,
            scope: role.scope.clone(),
            watch_for: role.watch_for.clone(),
            commands: role.commands.clone(),
            tools: role.tools.entries().collect(),
            paths: role.paths.clone(),
        }
    }
}
