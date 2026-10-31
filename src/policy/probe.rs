//! The prefix-promotion probe ledger — what's been seen and voted on (docs/permissions.md).

use super::*;

/// The per-prefix tally the prefix-promotion descent reads (docs/permissions.md).
/// Derived state, like the effective config: folded from the log's `Spot` entries at startup
/// ([`probe_ledger`]) and maintained incrementally by the daemon as the loop runs. Never on the wire.
#[derive(Clone, Debug, Default)]
pub struct ProbeLedger {
    /// prefix (joined tokens, e.g. `"git log"`) → how many spotter-assessed bash commands had it as
    /// a token-prefix. The recurrence signal the frequency gate reads.
    seen: BTreeMap<String, u32>,
    /// prefix → (safe votes, not-safe votes). A safe vote is a `clear_always` stance on a call that
    /// probed this prefix; not-safe is any other stance there. The promotion evidence base.
    votes: BTreeMap<String, (u32, u32)>,
}

impl ProbeLedger {
    /// Count one spotter-assessed bash command: bump `seen` for every token-prefix of it. Called
    /// once per assessed command — incrementally in the loop, and replayed by [`probe_ledger`] on
    /// resume — so the two agree (exactly one `Spot` per assessed command).
    pub fn observe(&mut self, tokens: &[String]) {
        for len in 1..=tokens.len() {
            *self.seen.entry(tokens[..len].join(" ")).or_default() += 1;
        }
    }

    /// Record a probe vote for `prefix`: `safe` iff the stance was `clear_always`.
    pub fn vote(&mut self, prefix: &str, safe: bool) {
        let entry = self.votes.entry(prefix.to_string()).or_default();
        if safe {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }

    /// Whether `prefix` is **resolved** and so must not be re-probed. At today's `K = 1` a single
    /// vote resolves it (one safe → promoted/pending; one not-safe → declined). When `K` grows this
    /// becomes "≥ K safe, or any not-safe (dissent breaks consensus)".
    fn resolved(&self, prefix: &str) -> bool {
        self.votes
            .get(prefix)
            .is_some_and(|(safe, not_safe)| *safe >= PROMOTE_AFTER_SAFE_VOTES || *not_safe > 0)
    }

    /// Whether a fresh spotter has voted `prefix` not safe for all arguments: the evidence a
    /// gate's grant descends past to a longer prefix ([`super::gate_grant`]).
    pub fn refuted(&self, prefix: &str) -> bool {
        self.votes
            .get(prefix)
            .is_some_and(|(_, not_safe)| *not_safe > 0)
    }

    /// Whether `prefix`'s safe-votes have reached the promotion threshold `K` — the loop's signal to
    /// file a `Proposed(Command)` for it.
    pub fn ready_to_promote(&self, prefix: &str) -> bool {
        self.votes
            .get(prefix)
            .is_some_and(|(safe, _)| *safe >= PROMOTE_AFTER_SAFE_VOTES)
    }

    /// The prefix to probe on the current bash command, or `None`. The descent
    /// (docs/permissions.md): the **shortest** token-prefix of `tokens` that is
    /// promotable (lexes + not already covered by `[commands]`), seen at least
    /// [`MIN_SIGHTINGS_TO_PROBE`] times, and not yet [`resolved`]. Shortest-first, so the broadest
    /// (most valuable) promotion is tried before descending to a sub-verb; the descent self-terminates
    /// at the first safe prefix because promoting it stops its subtree from reaching the spotter.
    ///
    /// [`resolved`]: ProbeLedger::resolved
    pub fn next_probe(&self, role: &Config, tokens: &[String]) -> Option<String> {
        (1..=tokens.len())
            .map(|len| tokens[..len].join(" "))
            .find(|prefix| {
                promotable_command(role, prefix).is_some() // not already covered by the allowlist
                    && self.seen.get(prefix).copied().unwrap_or(0) >= MIN_SIGHTINGS_TO_PROBE
                    && !self.resolved(prefix)
            })
    }
}

/// Fold the probe ledger from a whole log (startup / resume) — the batch form of the incremental
/// updates the daemon makes as the loop runs (docs/permissions.md). Mirrors
/// [`effective_config`]. Seen-counts come from each `Spot`'s bash command (recovered by `call_id`);
/// votes from each `Spot` that probed a prefix, counted only when the verdict was the model's — a
/// fail-closed `Fallback` didn't actually judge, so it casts no vote.
///
/// Commands are tokenized with [`crate::policy::bash_tokens`], the same lexer the live loop
/// observes with, so a replayed `cd <dir> && cargo test` counts the bare command exactly as it did
/// when it ran. `role` is the effective config the grant check for the `cd` target reads.
pub fn probe_ledger(dirs: &Directories, role: &Config, log: &[LogEntry]) -> ProbeLedger {
    // call_id → the bash command it ran, so a `Spot` can recover the command it judged.
    let mut command_of: HashMap<crate::log::CallId, String> = HashMap::new();
    for entry in log {
        if let Payload::Assistant { output, .. } = &entry.payload {
            for call in output.tool_calls().unwrap_or_default() {
                if call.name == bash::NAME
                    && let Some(cmd) = call.input.get("command").and_then(Value::as_str)
                {
                    command_of.insert(call.id, cmd.to_string());
                }
            }
        }
    }
    let mut ledger = ProbeLedger::default();
    for entry in log {
        let Payload::Spot(spot) = &entry.payload else {
            continue;
        };
        if let Some(cmd) = command_of.get(&spot.call_id)
            && let Some(tokens) = bash_tokens(dirs, role, cmd)
        {
            ledger.observe(&tokens);
        }
        if let Some(prefix) = &spot.probed_prefix
            && spot.origin == Origin::Model
        {
            ledger.vote(prefix, spot.stance == Stance::ClearAlways);
        }
    }
    ledger
}
