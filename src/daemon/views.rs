//! The JSON readouts served to clients by `/state` and `/log` (docs/control-api.md).
//!
//! `/state` is decomposed into one function per [`Topic`], and [`Session::state_json`] is
//! their composition over [`Topic::ALL`]. That split is what lets the composite snapshot
//! and the level-triggered `state` frames come from the same source, so `GET /state`
//! equals the fold of every state frame since connect rather than merely agreeing with
//! it by inspection.
//!
//! Each topic function returns a whole value, never a patch: a `state` frame carries a
//! topic's entire current value and `Value::Null` means *cleared*, so nothing here may
//! signal "unchanged" by omission.

use super::*;

impl Session {
    /// One topic's entire current value — the body of a `state` frame, and one field of
    /// the composite snapshot.
    pub(super) fn topic_json(&self, topic: Topic) -> Value {
        match topic {
            Topic::Gate => self.gate_json(),
            Topic::Ask => self.ask_json(),
            Topic::Processes => self.processes_json(),
            Topic::Usage => self.usage_json(),
            Topic::Role => self.role_json(),
            Topic::Proposals => self.proposals_json(),
            Topic::Notifications => self.notifications_json(),
            Topic::Pace => self.pace_json(),
            Topic::ReviewLevel => self.review_level_json(),
            Topic::ResolvedReviewLevel => self.resolved_review_level_json(),
            Topic::Running => self.running_json(),
            Topic::Queued => self.queued_json(),
            Topic::Stall => self.stall_json(),
            Topic::UpstreamFailure => self.upstream_failure_json(),
            Topic::UpstreamRetry => self.upstream_retry_json(),
            Topic::Cold => self.cold_json(),
            Topic::ConfigRepair => self.config_repair_json(),
            Topic::SessionIdentity => self.session_identity_json(),
        }
    }

    /// The composite `GET /state` representation: every topic under its own name, plus
    /// `last_seq`.
    ///
    /// `last_seq` is deliberately **not** a topic — it is the log's cursor rather than
    /// control state, redundant for any client taking `log` frames, and useful to a
    /// `?include=state` subscriber only as a bootstrap hint. It therefore sits outside
    /// the fold identity above.
    pub(crate) fn state_json(&self) -> String {
        let mut out = serde_json::Map::new();
        out.insert("last_seq".into(), self.last_seq_json());
        for topic in Topic::ALL {
            out.insert(topic.as_str().to_string(), self.topic_json(topic));
        }
        Value::Object(out).to_string()
    }

    /// The loop resource (`GET /loop`, docs/control-channel.md): the operator dials
    /// and the read-only `running` flag, each field the same value — and the same
    /// name — as its state topic, so the resource and the topics cannot drift.
    pub(super) fn loop_json(&self) -> String {
        serde_json::json!({
            "pace": self.topic_json(Topic::Pace),
            "review_level": self.topic_json(Topic::ReviewLevel),
            "running": self.topic_json(Topic::Running),
        })
        .to_string()
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

    /// The pending `ask` (docs/supervisor.md), or `null` when none is open.
    fn ask_json(&self) -> Value {
        match self.asking() {
            Some(ask) => serde_json::json!({
                "call_id": ask.call_id,
                "question": ask.question,
                "n": ask.n,
                "of": ask.of,
                "recalled": ask.recalled,
            }),
            None => Value::Null,
        }
    }

    /// The live background processes (docs/supervisor.md), as structure only — no tail
    /// and no last-chunk stamp, both of which change per output chunk and would make
    /// this topic re-emit at the rate of the output it describes. That content rides
    /// `process_output` on the ephemeral `output` class instead (docs/control-api.md).
    fn processes_json(&self) -> Value {
        serde_json::to_value(self.process_states()).unwrap_or(Value::Null)
    }

    /// Token/cost telemetry (docs/transport.md).
    fn usage_json(&self) -> Value {
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
        let context = self.capabilities.lock().unwrap().as_ref().map(|caps| {
            serde_json::json!({
                "limit": caps.context_length,
                "used": metrics.last_turn_usage.as_ref().map(Usage::prompt_tokens),
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
            // Absolute, not elapsed (docs/control-api.md): a ticking field would make
            // this topic re-emit on every read under level-triggered emission. Clients
            // subtract from their own clock.
            "last_request_at": metrics.last_request_wall,
            "context": context,
            "cost": cost,
            // The live end of the empirical prefix oracle
            // (docs/log-and-projection.md): consecutive main-loop requests that read
            // nothing from the cache and had no legitimate reason to (not boot, not a
            // model change, not idle past the TTL). A number, not a flag, so the
            // threshold stays a client decision; 0 whenever the last turn read warm.
            "cold_streak": metrics.cold_streak.streak(),
            // The Codex backend's subscription windows (docs/transport.md): the
            // plan and its two usage windows as of the last response that named
            // them, resets as absolute millis. null on every other provider.
            "plan": *self.plan.lock().unwrap(),
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
    /// alongside the gate; resolved over `PATCH /proposals/:seq`.
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
    /// `autonomous` / `upstream` / `spotter_human` / `human_only`. Runtime-settable.
    /// This is the dial's own value — what a client set and reads back — which is not
    /// always who answers; see [`resolved_review_level_json`](Self::resolved_review_level_json).
    fn review_level_json(&self) -> Value {
        Value::from(self.review_level().as_str())
    }

    /// The review position **in force right now** (docs/permissions.md). Equal to the
    /// dial except for `upstream` with no live upstream — a root session, or a creator
    /// that has ended — which reviews as `spotter_human`. Reported beside the dial so
    /// "who answers this gate" is answerable while one is pending, without the client
    /// reconstructing the creation tree.
    fn resolved_review_level_json(&self) -> Value {
        Value::from(self.resolved_review_level().as_str())
    }

    /// Whether the loop is executing a turn (vs. parked idle) — the "is something
    /// happening" signal the TUI's graduated Esc reads to decide pause-vs-swallow
    /// (docs/tui.md). Unlike streaming deltas, this stays true through a *silent*
    /// long-running tool, so Esc can still reach it.
    fn running_json(&self) -> Value {
        Value::from(self.running.load(Ordering::SeqCst))
    }

    /// The pending turn queue in submission order: `[{"submission_id":…,"text":…}]`, `[]` when
    /// nothing is queued (docs/control-api.md). A pending turn is ephemeral control state — never
    /// logged, never projected — so this is the only place it is readable, and it is what a
    /// client's optimistic echo and its recall affordance both address.
    fn queued_json(&self) -> Value {
        Value::Array(
            self.queued_turns()
                .into_iter()
                .map(|t| t.pending_json())
                .collect(),
        )
    }

    /// A trajectory that gave up (docs/transport.md): why, how many attempts it
    /// spent, when it stopped, and a lead line naming what recovers it. `null`
    /// once a trajectory starts again, so the topic reports the condition now
    /// rather than one already recovered from. It is the difference between the
    /// two idle states `running: false` collapses — finished, and stopped short.
    fn stall_json(&self) -> Value {
        serde_json::to_value(self.stall()).unwrap_or(Value::Null)
    }

    /// The last non-success upstream response (docs/transport.md). Cleared by the
    /// next 2xx, which is the recovery boundary the retry topic below shares.
    fn upstream_failure_json(&self) -> Value {
        serde_json::to_value(*self.upstream_failure.lock().unwrap()).unwrap_or(Value::Null)
    }

    /// The retry in flight (docs/transport.md): which surface is retrying, which
    /// attempt of what budget, when the next attempt fires, and a lead line that
    /// says what is happening rather than a bare status code. `null` — the whole
    /// value, as every topic's cleared state is — once an attempt finalizes or the
    /// budget is exhausted.
    fn upstream_retry_json(&self) -> Value {
        serde_json::to_value(self.upstream_retry.get()).unwrap_or(Value::Null)
    }

    /// The config-unparseable park (docs/permissions.md): which file, when, why it
    /// did not parse, and why the restore failed too. `null` whenever the file
    /// parses — so a client that sees a value knows the loop is stopped on it and
    /// that editing the file is what clears it.
    fn config_repair_json(&self) -> Value {
        serde_json::to_value(self.config_repair()).unwrap_or(Value::Null)
    }

    /// What this session is (docs/control-api.md): the configured model id and the
    /// working directory. Set once at boot, so this value does not move during a
    /// session; `null` where no session booted (tests, offline verbs).
    pub(super) fn session_identity_json(&self) -> Value {
        serde_json::to_value(self.identity.lock().unwrap().clone()).unwrap_or(Value::Null)
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
        // The live policy, not the frozen base: a session grant or an fs-watch edit moves the
        // real perimeter, and reading `self.role` here would report the boot config instead.
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
        // `commands` is an **object** (key → stance word), not a list of keys: a `[commands]`
        // entry can carry `watch`/`gate`/`deny` as well as `clear` (docs/permissions.md), and a
        // reader that saw only the keys would read a carve-out as a grant.
        let commands: serde_json::Map<String, Value> = role
            .commands
            .iter()
            .map(|(key, stance)| (key.clone(), Value::String(stance.as_str().to_string())))
            .collect();
        serde_json::json!({
            "source": source,
            "scope": role.scope,
            "watch_for": role.watch_for,
            "commands": commands,
            "tools": role.tools,
            "paths": paths,
        })
    }

    /// The selected window of the log, as a JSON array (docs/control-api.md). Each
    /// element is the entry's [`EntryView::new`] — image `data` omitted, resolvable
    /// through [`Self::image_bytes`], and an oversized result's `content` truncated and
    /// tagged, resolvable through [`Self::entry_json`].
    pub(super) fn log_json(&self, select: &LogSelect) -> String {
        let log = self.log.lock().unwrap();
        let entries = log.entries();
        let views: Vec<_> = window(entries, select)
            .iter()
            .map(|entry| EntryView::new(entry).with_result_raw(result_raw(entry, entries)))
            .collect();
        serde_json::to_string(&views).unwrap_or_else(|_| "[]".into())
    }

    /// The selected window of the log at reading grain (`GET /transcript`,
    /// docs/control-api.md): one text block per entry, bodies left behind
    /// [`Self::entry_json`]. Same window selection as [`Self::log_json`].
    pub(super) fn transcript(&self, select: &LogSelect) -> String {
        let log = self.log.lock().unwrap();
        super::transcript::render(window(log.entries(), select))
    }

    /// One entry by `seq`, or `None` if the log has no such position.
    ///
    /// The general form of reference resolution: every reference a windowed client can
    /// hold is ultimately to an entry or to something inside one. Immutably cacheable —
    /// see [`Self::tool_call_json`]. Served as the [`EntryView`], like every
    /// control-channel read of an entry.
    pub(crate) fn entry_json(&self, seq: u64) -> Option<String> {
        let log = self.log.lock().unwrap();
        let entries = log.entries();
        let entry = entries.iter().find(|e| e.seq == seq)?;
        let view = EntryView::full(entry).with_result_raw(result_raw(entry, entries));
        serde_json::to_string(&view).ok()
    }

    /// The decoded bytes of one logged result image, addressed by its entry's `seq` and
    /// its index within that result — the dereference target for the image references
    /// the views carry (`GET /log/:seq/images/:idx`, docs/control-api.md).
    pub(crate) fn image_bytes(&self, seq: u64, idx: usize) -> Option<(String, Vec<u8>)> {
        use base64::Engine;
        let log = self.log.lock().unwrap();
        let entry = log.entries().iter().find(|e| e.seq == seq)?;
        let Payload::ToolResult { raw, .. } = &entry.payload else {
            return None;
        };
        let image = raw.images.get(idx)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .ok()?;
        Some((image.media_type.clone(), bytes))
    }

    /// One tool call by its `call_id`, with the `seq` of the entry carrying it.
    ///
    /// A call is not its own log entry — it is embedded in an `Assistant` payload — so it
    /// needs its own address, and `call_id` is the one already in the log, in
    /// `tool_output` frames, and in the hand of any client holding an orphaned
    /// `tool_result`. `seq` rides along because a client that resolved a call this way
    /// usually wants the entry it came from next, and it costs nothing to say now.
    ///
    /// Both this and [`Self::entry_json`] are immutably cacheable: a `LogEntry` is
    /// written once and never mutated — redaction chooses `wire` at append time from the
    /// verdict rather than flipping it afterward (docs/log-and-projection.md) — so a
    /// returning client never refetches what it has seen.
    pub(super) fn tool_call_json(&self, call_id: &str) -> Option<String> {
        let log = self.log.lock().unwrap();
        let (seq, call) = log.entries().iter().find_map(|entry| {
            let Payload::Assistant { output, .. } = &entry.payload else {
                return None;
            };
            let calls = output.tool_calls().ok()?;
            let call = calls.into_iter().find(|c| c.id.0 == call_id)?;
            Some((entry.seq, call))
        })?;
        serde_json::to_string(&serde_json::json!({ "seq": seq, "call": call })).ok()
    }
}

/// The entries a [`LogSelect`] names, as a sub-slice of the log.
///
/// A slice, never a copy: `since`/`before` are positions in a `seq`-ordered log, so both
/// bounds are binary searches, and `tail` is the last N of what they left. The log is
/// dense today (`seq` == index) but this does not assume it.
fn window<'a>(entries: &'a [LogEntry], select: &LogSelect) -> &'a [LogEntry] {
    let start = match select.since {
        Some(since) => entries.partition_point(|e| e.seq < since),
        None => 0,
    };
    let end = match select.before {
        Some(before) => entries.partition_point(|e| e.seq < before),
        None => entries.len(),
    };
    // `before <= since` names an empty range rather than an error: it is what a client
    // paging backward past the start of the log naturally sends on its last page.
    let selected = &entries[start..end.max(start)];
    match select.tail {
        // Exactly N — or the whole range when it holds fewer, which is the same answer a
        // client gets at the start of a short session.
        Some(n) => &selected[selected.len().saturating_sub(n as usize)..],
        None => selected,
    }
}
