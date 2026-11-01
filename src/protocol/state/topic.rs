//! The names of the state topics (docs/control-api.md): the keys of `GET /state` and the
//! `kind` of a `state` frame.

use serde::Serialize;

#[cfg(test)]
mod test;

/// A level-triggered state topic (docs/control-api.md). The set is lifted from the
/// units `src/clients/tui/run.rs` already fingerprints out of the `GET /state` blob and emits
/// separately — that poller is the topic decomposition, arrived at by use — with
/// `role`/`proposals` split (the TUI merges them only for its own convenience) and the
/// loop dials named for the fields of the loop resource.
///
/// A `state` frame carries the topic's **entire** value, never a patch: replacement is
/// idempotent and order-insensitive, which is what makes `GET /state` equal to the fold
/// of every state frame since connect.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Topic {
    /// The pending approval gate, or `null` when none is open.
    Gate,
    /// The question an `ask` has put to the operator, or `null` when none is pending:
    /// `{call_id, question, n, of, recalled}` (docs/process-monitor.md). `n` of `of` counts
    /// the asks of one completion, which are put one at a time; `recalled` holds the
    /// turns withdrawn when the ask began, in the shape `DELETE /turns/:id` returns.
    Ask,
    /// Live background processes (docs/process-monitor.md).
    Processes,
    /// Token/cost telemetry (docs/transport.md).
    Usage,
    /// The effective policy in force (docs/permissions.md).
    Role,
    /// Config-edit proposals awaiting an orchestrator decision.
    Proposals,
    /// Pending ambient notifications (docs/inbox.md).
    Notifications,
    /// The operator's pace dial.
    Pace,
    /// The review-level dial (docs/permissions.md).
    ReviewLevel,
    /// The review position **in force**, which is the dial's own value except that
    /// `supervisor` with no live supervisor reviews as `spotter_human` (docs/permissions.md).
    /// Carried beside the dial rather than folded into it because the two answer
    /// different questions — what was set, and who answers a gate open right now —
    /// and because this one moves without anyone turning the dial.
    ResolvedReviewLevel,
    /// The session whose `supervisor` review position answers this one's gates, or
    /// `null` when there is none (docs/permissions.md): the session the host's
    /// supervisor pointer names (by default the creator) while that session is
    /// hosted, so it moves when the pointer is set or cleared and to `null` when the
    /// supervisor ends, without anyone turning the dial; carried as its own topic for
    /// the same reason as [`ResolvedReviewLevel`](Self::ResolvedReviewLevel).
    Supervisor,
    /// Where the loop is (docs/control-api.md): one exclusive phase, tagged by
    /// `phase` — `idle`, `requesting`, `executing`, `delayed` (with `until`), `gated`,
    /// `asking`, `cold_parked`, or `blocked` (with `reason`).
    LoopPhase,
    /// What each loop route would answer now (docs/control-api.md): `turn`,
    /// `continue`, and `compact`, each accepted or refused with the route's message.
    Verbs,
    /// Turns submitted but not yet dequeued, in submission order — `[{submission_id, text}]`
    /// (docs/control-channel.md). The clients' optimistic pending echo keys off it, and it is
    /// what makes the queue addressable for a withdraw.
    Queued,
    /// A trajectory that gave up rather than finished (docs/transport.md).
    /// `null` when the last one finished. Beside [`LoopPhase`](Self::LoopPhase)
    /// because it is what distinguishes the two idle conditions the phase
    /// cannot: "idle, done" from "idle, gave up".
    Stall,
    /// The last non-success upstream response (docs/transport.md).
    UpstreamFailure,
    /// The same reading for a separate triage client (docs/control-api.md): `null`
    /// when the triage borrows the agent's client, and the all-clear on its own.
    TriageUpstreamFailure,
    /// A retry in flight — which surface, which attempt, of what budget
    /// (docs/transport.md). `null` when nothing is being retried.
    UpstreamRetry,
    /// The session's prompt cache is believed to have expired:
    /// `{since, context_tokens}`, `null` while it is believed warm,
    /// before the first request, and on a model with no cache.
    Cold,
    /// The last compaction's failure (docs/summary.md): `{error, at}`, or `null` —
    /// cleared when a turn's request goes out or another compaction is asked for.
    CompactionFailure,
    /// The config file on disk does not parse and could not be restored, so the
    /// loop is parked until a human repairs it (docs/permissions.md). `null`
    /// whenever the file parses — which is nearly always, since a bad edit is
    /// normally reverted rather than left on disk.
    ConfigRepair,
    /// What this session *is* — the configured model and its working directory
    /// (docs/control-api.md). Static for the session today; carried as a topic so a
    /// client reads it from the connect snapshot like everything else, and so a
    /// value that ever moves reaches clients without a new mechanism.
    SessionIdentity,
    /// What the loop is waiting on right now (docs/control-api.md): the upstream
    /// requests open, each with its origin, stage, and the instant that stage began,
    /// the tool calls executing, and a compaction waiting or writing. `null` when
    /// nothing is open. Where the loop itself waits is [`LoopPhase`](Self::LoopPhase).
    CurrentOperations,
    /// The session's preparation, while it is starting (`starting.rs`,
    /// docs/control-api.md): the stage, the MCP server awaiting authorization, and a
    /// failure with the turns it hands back. `null` once the session has started, which
    /// is every value a running session publishes.
    Starting,
    /// Where the session stands, as one view (`status/view.rs`, docs/control-api.md):
    /// its condition with what that condition needs — the held call and why it was
    /// held, the question, the closing message, what a working loop is doing — and a
    /// footer of model, context, cost, background processes, and queued turns. What
    /// subscription notifications render (docs/tools.md).
    Status,
    /// The newest release the host's update check found, while it is newer than the
    /// host (`newer_version.rs`, docs/update-check.md): `{version, tag, url,
    /// update_command}`, or `null`. Host state, the same for every session on the host.
    NewerVersion,
}

impl Topic {
    /// Every topic, in `GET /state` key order. The one enumeration: the composite
    /// snapshot is built by folding this, and the level-triggered emitter diffs by
    /// walking it, so a topic added here reaches both surfaces or neither — which is
    /// what keeps `GET /state` equal to the fold of every state frame
    /// (docs/control-api.md).
    pub(crate) const ALL: [Topic; 26] = [
        Self::Gate,
        Self::Ask,
        Self::Processes,
        Self::Usage,
        Self::Role,
        Self::Proposals,
        Self::Notifications,
        Self::Pace,
        Self::ReviewLevel,
        Self::ResolvedReviewLevel,
        Self::Supervisor,
        Self::LoopPhase,
        Self::Verbs,
        Self::Queued,
        Self::Stall,
        Self::UpstreamFailure,
        Self::TriageUpstreamFailure,
        Self::UpstreamRetry,
        Self::Cold,
        Self::CompactionFailure,
        Self::ConfigRepair,
        Self::SessionIdentity,
        Self::CurrentOperations,
        Self::Starting,
        Self::Status,
        Self::NewerVersion,
    ];

    /// The topic named by a `?include=state:<topic>` token, or `None` if there is no such
    /// topic. The inverse of [`Topic::as_str`], and the reason that method exists: the
    /// grammar is parsed against the same names the frames carry.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == name)
    }

    /// The wire name, for `?include=state:<topic>` parsing and the SSE `event:` field.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Gate => "gate",
            Self::Ask => "ask",
            Self::Processes => "processes",
            Self::Usage => "usage",
            Self::Role => "role",
            Self::Proposals => "proposals",
            Self::Notifications => "notifications",
            Self::Pace => "pace",
            Self::ReviewLevel => "review_level",
            Self::ResolvedReviewLevel => "resolved_review_level",
            Self::Supervisor => "supervisor",
            Self::LoopPhase => "loop_phase",
            Self::Verbs => "verbs",
            Self::Queued => "queued",
            Self::Stall => "stall",
            Self::UpstreamFailure => "upstream_failure",
            Self::TriageUpstreamFailure => "triage_upstream_failure",
            Self::UpstreamRetry => "upstream_retry",
            Self::Cold => "cold",
            Self::CompactionFailure => "compaction_failure",
            Self::ConfigRepair => "config_repair",
            Self::SessionIdentity => "session_identity",
            Self::CurrentOperations => "current_operations",
            Self::Starting => "starting",
            Self::Status => "status",
            Self::NewerVersion => "newer_version",
        }
    }
}
