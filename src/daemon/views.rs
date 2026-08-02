//! The JSON readouts served to clients by `/state` and `/log` (docs/control-api.md).

use super::*;

impl Shared {
    pub(super) fn state_json(&self) -> String {
        let pending = self.pending.lock().unwrap().clone();
        let (pace_speed, pace_held) = self.pace_snapshot();
        let last_seq = self
            .log
            .lock()
            .unwrap()
            .entries()
            .len()
            .checked_sub(1)
            .map(|i| i as u64);
        // Taken before `metrics` is locked: it locks the log, and nothing else in the
        // daemon acquires the two in the other order.
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
        let usage = serde_json::json!({
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
        });
        serde_json::json!({
            "last_seq": last_seq,
            "pending_approval": pending,
            // Whether the loop is executing a turn (vs. parked idle) — the "is something happening"
            // signal the TUI's graduated Esc reads to decide pause-vs-swallow
            // (docs/tui.md). Unlike streaming deltas, this stays true
            // through a *silent* long-running tool, so Esc can still reach it.
            "running": self.running.load(Ordering::SeqCst),
            // Autonomous = no synchronous human at the gate (the orchestrator-above wants to know
            // a subordinate runs unsupervised before it trusts the perimeter it reads below).
            // Derived from `review_level`; kept for back-compat.
            "autonomous": self.autonomous(),
            // The review-level dial (docs/permissions.md): who reviews a
            // gated call — `autonomous` / `spotter_human` / `human_only`. Runtime-settable.
            "review_level": self.review_level().as_str(),
            // The operator's pace dial (docs/control-channel.md) — loud to the
            // operator's clients, silent to the agent (never logged/projected). `speed_ms` is the
            // between-turns delay; `held` is the pause overlay.
            "pace": {
                "speed_ms": pace_speed.as_millis() as u64,
                "held": pace_held,
            },
            "role": self.role_json(),
            "pending_notifications": self.pending_notifications(),
            // Async config edits awaiting an orchestrator decision (the prefix-promotion
            // proposal, the session's config-write) — polled by the client's policy surface
            // alongside the gate; resolved over `POST /proposals/:seq`.
            "proposals": self.pending_proposals(),
            // The live background processes (docs/supervisor.md) — the same `views()` snapshot
            // the peek auto-shows the model, surfaced here for the human clients (the
            // TUI process panel, docs/tui.md). Ephemeral, like the rest of `/state`.
            "processes": self.bg_snapshot(),
            "usage": usage,
            "upstream_failure": *self.upstream_failure.lock().unwrap(),
        })
        .to_string()
    }

    /// The **effective** policy report for `GET /state` — the base config plus every committed
    /// config edit folded in, i.e. the *live* perimeter, not the boot config
    /// (docs/permissions.md). It carries full **contents**, not counts: an
    /// orchestrator inspecting a subordinate (or a human checking "what am I running?") needs the
    /// actual commands/paths/scope in force — and the config is bounded-small by design, never
    /// projected, with no secret to withhold. `scope` and `watch-for` are surfaced here because the
    /// effective scope can drift from the frozen boot preamble (a live scope edit) and `watch-for`
    /// is on no other wire. `source` is the base config's provenance (path, or `null` for the
    /// built-in default).
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
