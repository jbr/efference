//! The JSON readouts served to clients by `/state` and `/log` (docs/control-api.md).
//!
//! `/state` is decomposed into one function per [`Topic`], and [`Shared::state_json`] is
//! their composition over [`Topic::ALL`]. That split is what lets the composite snapshot
//! and the level-triggered `state` frames come from the same source, so `GET /state`
//! equals the fold of every state frame since connect rather than merely agreeing with
//! it by inspection.
//!
//! Each topic function returns a whole value, never a patch: a `state` frame carries a
//! topic's entire current value and `Value::Null` means *cleared*, so nothing here may
//! signal "unchanged" by omission.

use super::*;

impl Shared {
    /// One topic's entire current value — the body of a `state` frame, and one field of
    /// the composite snapshot.
    pub(super) fn topic_json(&self, topic: Topic) -> Value {
        match topic {
            Topic::Gate => self.gate_json(),
            Topic::Processes => self.processes_json(),
            Topic::Usage => self.usage_json(),
            Topic::Role => self.role_json(),
            Topic::Proposals => self.proposals_json(),
            Topic::Notifications => self.notifications_json(),
            Topic::Pace => self.pace_json(),
            Topic::ReviewLevel => self.review_level_json(),
            Topic::Running => self.running_json(),
            Topic::UpstreamFailure => self.upstream_failure_json(),
        }
    }

    /// The composite `GET /state` representation: every topic under its own name, plus
    /// `last_seq`.
    ///
    /// `last_seq` is deliberately **not** a topic — it is the log's cursor rather than
    /// control state, redundant for any client taking `log` frames, and useful to a
    /// `?include=state` subscriber only as a bootstrap hint. It therefore sits outside
    /// the fold identity above.
    pub(super) fn state_json(&self) -> String {
        let mut out = serde_json::Map::new();
        out.insert("last_seq".into(), self.last_seq_json());
        for topic in Topic::ALL {
            out.insert(topic.as_str().to_string(), self.topic_json(topic));
        }
        Value::Object(out).to_string()
    }

    /// The highest committed `seq`, or `null` on an empty log.
    fn last_seq_json(&self) -> Value {
        match self.last_committed_seq() {
            Some(seq) => Value::from(seq),
            None => Value::Null,
        }
    }

    /// The highest committed `seq` — the watermark a level-triggered `state` frame is
    /// stamped with, and the `last_seq` of the composite snapshot. `None` on an empty
    /// log.
    pub(super) fn last_committed_seq(&self) -> Option<u64> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .len()
            .checked_sub(1)
            .map(|i| i as u64)
    }

    /// The pending approval gate, or `null` when none is open.
    fn gate_json(&self) -> Value {
        serde_json::to_value(self.pending.lock().unwrap().clone()).unwrap_or(Value::Null)
    }

    /// The live background processes (docs/supervisor.md) — the same `views()` snapshot
    /// the peek auto-shows the model, surfaced here for the human clients (the TUI
    /// process panel, docs/tui.md). Ephemeral, like the rest of `/state`.
    fn processes_json(&self) -> Value {
        serde_json::to_value(self.bg_snapshot()).unwrap_or(Value::Null)
    }

    /// Token/cost telemetry (docs/transport.md).
    fn usage_json(&self) -> Value {
        // Taken before `metrics` is locked: it locks the log, and nothing else in the
        // daemon acquires the two in the other order. Keep this ordering if this
        // function is ever split further.
        let retention = self.retention();
        let metrics = self.metrics.lock().unwrap();
        // Context-window occupancy (docs/transport.md): the last request's total
        // prompt tokens — uncached input + cache reads + cache writes; `input_tokens`
        // alone is only the uncached remainder — against the boot-fetched limit.
        // `used` is null until the first request; `context` is null entirely when no
        // capabilities were fetched (tests, offline verbs).
        let context = self.capabilities.lock().unwrap().as_ref().map(|caps| {
            serde_json::json!({
                "limit": caps.context_length,
                "used": metrics.last_usage.as_ref().map(|u| {
                    u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens
                }),
            })
        });
        // Session cost (docs/transport.md): the billed running total from the
        // provider's accounting endpoint, null when the provider has none
        // (direct Anthropic). `unpriced` counts requests the fetch couldn't
        // price, so a nonzero value marks the total as an undercount.
        let cost = {
            let cell = self.cost.lock().unwrap();
            cell.endpoint
                .as_ref()
                .map(|_| serde_json::json!({ "usd": cell.total_usd, "unpriced": cell.unpriced }))
        };
        serde_json::json!({
            "last": metrics.last_usage,
            "cumulative": {
                "input_tokens": metrics.total_input,
                "output_tokens": metrics.total_output,
                "cache_read_input_tokens": metrics.total_cache_read,
                "cache_creation_input_tokens": metrics.total_cache_creation,
            },
            "seconds_since_last_request": metrics.last_request_at.map(|t| t.elapsed().as_secs()),
            "context": context,
            "cost": cost,
            // Triage retention (docs/redaction-fork.md): how much of what the gate
            // showed survives on the wire, derived from the log rather than from
            // transport telemetry. `withheld` counts results whose output a failed
            // gate suppressed — a distinct condition from a low ratio, and kept out
            // of it, so a failing gate can't read as excellent compression.
            "retention": retention,
        })
    }

    /// Async config edits awaiting an orchestrator decision (the prefix-promotion
    /// proposal, the session's config-write) — read by the client's policy surface
    /// alongside the gate; resolved over `POST /proposals/:seq`.
    fn proposals_json(&self) -> Value {
        serde_json::to_value(self.pending_proposals()).unwrap_or(Value::Null)
    }

    /// Pending ambient notifications (docs/inbox.md).
    fn notifications_json(&self) -> Value {
        serde_json::to_value(self.pending_notifications()).unwrap_or(Value::Null)
    }

    /// The operator's pace dial (docs/control-channel.md) — loud to the operator's
    /// clients, silent to the agent (never logged/projected). `speed_ms` is the
    /// between-turns delay; `held` is the pause overlay.
    fn pace_json(&self) -> Value {
        let (speed, held) = self.pace_snapshot();
        serde_json::json!({ "speed_ms": speed.as_millis() as u64, "held": held })
    }

    /// The review-level dial (docs/permissions.md): who reviews a gated call —
    /// `autonomous` / `spotter_human` / `human_only`. Runtime-settable.
    fn review_level_json(&self) -> Value {
        Value::from(self.review_level().as_str())
    }

    /// Whether the loop is executing a turn (vs. parked idle) — the "is something
    /// happening" signal the TUI's graduated Esc reads to decide pause-vs-swallow
    /// (docs/tui.md). Unlike streaming deltas, this stays true through a *silent*
    /// long-running tool, so Esc can still reach it.
    fn running_json(&self) -> Value {
        Value::from(self.running.load(Ordering::SeqCst))
    }

    /// The last non-success upstream response (docs/transport.md).
    fn upstream_failure_json(&self) -> Value {
        serde_json::to_value(*self.upstream_failure.lock().unwrap()).unwrap_or(Value::Null)
    }

    /// The **effective** policy report — the base config plus every committed config edit folded
    /// in, i.e. the *live* perimeter, not the boot config (docs/permissions.md). It carries full
    /// **contents**, not counts: an orchestrator inspecting a subordinate (or a human checking
    /// "what am I running?") needs the actual commands/paths/scope in force — and the config is
    /// bounded-small by design, never projected, with no secret to withhold. `scope` and
    /// `watch-for` are surfaced here because the effective scope can drift from the frozen boot
    /// preamble (a live scope edit) and `watch-for` is on no other wire. `source` is the base
    /// config's provenance (path, or `null` for the built-in default).
    pub(super) fn role_json(&self) -> Value {
        let source = match &self.role.source {
            ConfigSource::Default => Value::Null,
            ConfigSource::Path(p) => Value::String(p.display().to_string()),
        };
        // The live policy, not the frozen base — this is the fix for `/state` reporting the boot
        // config while a session grant / fs-watch edit had already moved the real perimeter.
        let role = self.effective_config();
        let paths: serde_json::Map<String, Value> = role
            .paths
            .iter()
            .map(|(p, perms)| {
                (
                    p.clone(),
                    serde_json::json!({ "read": perms.read, "write": perms.write }),
                )
            })
            .collect();
        serde_json::json!({
            "source": source,
            "scope": role.scope,
            "watch_for": role.watch_for,
            "commands": role.commands,
            "tools": role.tools,
            "paths": paths,
        })
    }

    pub(super) fn log_json(&self, since: Option<u64>) -> String {
        let log = self.log.lock().unwrap();
        let entries: Vec<&LogEntry> = match since {
            Some(s) => log.entries().iter().filter(|e| e.seq >= s).collect(),
            None => log.entries().iter().collect(),
        };
        serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into())
    }
}
