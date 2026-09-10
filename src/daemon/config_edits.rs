//! Effective policy, config edits, and pending proposals (docs/permissions.md).

use super::*;

/// One async config edit awaiting an orchestrator decision
/// (docs/permissions.md). Filed by a `Proposed` [`ConfigEdit`] append (the
/// session's `propose_scope`, the spotter's prefix-promotion); the orchestrator-above commits or
/// cancels it, which drops it from the queue. Unlike [`PendingNotification`], the proposal **already
/// has a durable `seq`** (the `Proposed` entry's), so that *is* its id — no queue-local counter.
#[derive(Clone, Debug, Serialize)]
pub(super) struct PendingProposal {
    /// The `seq` of the `Proposed` log entry — the proposal's id, which the orchestrator's
    /// commit/cancel references.
    pub(super) seq: Seq,
    /// The change being proposed; the committing entry copies it self-contained so the fold stays a
    /// pure per-entry filter (docs/permissions.md).
    pub(super) change: crate::log::ConfigChange,
}

/// The notification a **session-visible** config edit raises (docs/permissions.md),
/// as `(body, slot)`, or `None` for an edit the session need not hear about. `scope` and `paths`
/// are the perimeter the acting session should learn moved; `commands`/`tools` are classifier-only.
/// The `slot` lets a later edit of the same key supersede a still-pending notice.
pub(super) fn session_visible_notice(
    change: &crate::log::ConfigChange,
) -> Option<(String, String)> {
    use crate::log::ConfigChange;
    match change {
        ConfigChange::Scope { text } => Some((
            format!("Your session scope was updated by the orchestrator:\n\n{text}"),
            "config:scope".to_string(),
        )),
        ConfigChange::Path { path, read, write } => {
            let perms = match (*read, *write) {
                (_, true) => "read + write",
                (true, false) => "read only",
                (false, false) => "no access",
            };
            Some((
                format!(
                    "Filesystem access changed: `{path}` (and everything beneath it) is now {perms}."
                ),
                format!("config:path:{path}"),
            ))
        }
        ConfigChange::Command { .. } | ConfigChange::Tool { .. } => None,
    }
}

impl Session {
    /// Drop a pending proposal from the queue once an orchestrator resolves it (commit or cancel).
    /// The committing/cancelling entry references the proposal by its `seq`; a direct commit (the
    /// synchronous human gate, fs-watch) has no queued proposal, so this is a no-op there.
    pub(super) fn resolve_proposal(&self, seq: Seq) {
        self.proposals.lock().unwrap().retain(|p| p.seq != seq);
    }

    /// The current pending-proposals queue — async config edits awaiting an orchestrator decision
    /// (docs/permissions.md). Surfaced to the orchestrator-above on `GET
    /// /proposals` and in `/state`, and consumed by the commit/cancel verbs.
    pub(super) fn pending_proposals(&self) -> Vec<PendingProposal> {
        self.proposals.lock().unwrap().clone()
    }

    /// Resolve a pending proposal by `seq` — the orchestrator-above's commit/cancel decision over
    /// `PATCH /proposals/:seq` (docs/permissions.md). **Commit** appends a
    /// `Committed` [`ConfigEdit`] carrying the proposal's `change`, which `append`'s fold applies to
    /// the effective config (and notifies the session for `scope`/`paths`); **cancel** appends a
    /// `Cancelled` entry (audit-only). Both reference the proposal by `seq`, so the fold drops it
    /// from the pending queue. Returns `false` if no pending proposal has that `seq`. The commit is
    /// session-effective only: unlike the synchronous gate path
    /// ([`apply_config_change`](Self::apply_config_change)), it is not written back to the
    /// config file.
    pub(super) fn resolve_proposal_decision(
        &self,
        seq: Seq,
        commit: bool,
        note: String,
    ) -> io::Result<bool> {
        let Some(change) = self
            .proposals
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.seq == seq)
            .map(|p| p.change.clone())
        else {
            return Ok(false);
        };
        let edit = crate::log::ConfigEdit {
            state: if commit {
                EditState::Committed
            } else {
                EditState::Cancelled
            },
            resolves: Some(seq),
            change,
            note,
        };
        self.append(self.latest_turn(), Payload::ConfigEdit(edit))?;
        Ok(true)
    }

    /// Edit a committed [`ConfigChange`] into the config file, preserving its formatting
    /// (docs/permissions.md). Every committed grant goes through here, so the file
    /// stays a complete record of the perimeter the session ran under — the artifact an operator
    /// reads, and copies grants up to the origin template from. A session with no config file (the
    /// built-in default role) is a no-op: there is no file to diverge from.
    pub fn apply_config_change(&self, change: &crate::log::ConfigChange) -> std::io::Result<()> {
        match self.config_file.lock().unwrap().as_mut() {
            Some(file) => file.apply(change),
            None => Ok(()),
        }
    }

    /// Re-read the config file, adopting it if it parses and reverting it if it does not
    /// (docs/permissions.md). `None` when there's no file. Both callers
    /// — the fs-watch task and the loop, right after an approved config write — go through here,
    /// because the invariant is about the file's state, not about who edited it.
    pub fn refresh_config(&self) -> Option<crate::config::Refreshed> {
        self.config_file
            .lock()
            .unwrap()
            .as_mut()
            .map(|f| f.refresh())
    }

    /// Fold a newly-adopted config's `[env]` table into the session masker
    /// (docs/env.md). Additive only: a variable that stops being masked keeps its
    /// needles for the rest of the session, because a rotated-out secret is still
    /// a secret and still sits in files the session may read. The child-env
    /// policy (strip, and the values spawns inject) is *not* re-derived — it is
    /// fixed at boot with the spawn sites that hold it.
    ///
    /// Any reference the edited table names is re-resolved here, through the
    /// session's cached resolver, so an unchanged reference costs nothing and a
    /// newly-written one resolves while the operator who wrote it is present.
    /// Returns whatever failed to resolve, for the caller to hand back to the
    /// edit's author.
    pub fn absorb_env_table(
        &self,
        config: &crate::config::Config,
    ) -> Vec<crate::env_table::ResolveError> {
        self.env_policy.absorb_config(&config.env)
    }

    /// The resolved role's policy, for the loop's fast-path classifier
    /// (docs/permissions.md). Immutable for the session, so a borrow
    /// is fine — no lock.
    pub fn role(&self) -> &crate::config::Config {
        &self.role.role
    }

    /// The effective policy the cascade should consult: base role + the fold of every grant
    /// appended so far (docs/permissions.md). Returns an `Arc` clone of the
    /// incrementally-maintained role — O(1), and safe to hold across the spotter's await (a
    /// lock guard would not be). Falls back to the base if no grants have landed.
    pub fn effective_config(&self) -> Arc<crate::config::Config> {
        self.effective.lock().unwrap().clone()
    }

    /// The role config file path, if the role resolved from one (`None` for the built-in
    /// default — nothing to persist to). The file every committed grant is written
    /// back to (docs/permissions.md).
    pub fn config_path(&self) -> Option<&std::path::Path> {
        match &self.role.source {
            ConfigSource::Path(p) => Some(p),
            ConfigSource::Default => None,
        }
    }

    /// The current **review level** (docs/permissions.md) — who reviews a
    /// gated call. Runtime-mutable (the `PATCH /loop {review_level}` verb); read by the loop's cascade and
    /// reported on `GET /state`. Relaxed ordering: a lone dial with no happens-before tie to other
    /// state, read on the loop thread and written by a handler thread.
    pub fn review_level(&self) -> crate::policy::ReviewLevel {
        crate::policy::ReviewLevel::from_u8(self.review_level.load(Ordering::Relaxed))
    }

    /// Set the review level — the runtime `PATCH /loop {review_level}` verb, or `serve --autonomous` at
    /// boot via [`set_autonomous`](Self::set_autonomous).
    pub fn set_review_level(&self, level: crate::policy::ReviewLevel) {
        self.review_level.store(level.as_u8(), Ordering::Relaxed);
        self.touch_state();
    }

    /// Whether no synchronous human sits at the gate — derived from the review level
    /// (`Autonomous`). For the loop's callers that only care about the
    /// redirect-vs-block distinction; `GET /state` carries `review_level` itself
    /// rather than restating this as a second field.
    pub fn autonomous(&self) -> bool {
        self.review_level() == crate::policy::ReviewLevel::Autonomous
    }

    /// Set autonomous mode at boot (`serve --autonomous`): maps to the `Autonomous` review level,
    /// else the supervised default (`SpotterHuman`).
    pub fn set_autonomous(&self, on: bool) {
        self.set_review_level(if on {
            crate::policy::ReviewLevel::Autonomous
        } else {
            crate::policy::ReviewLevel::SpotterHuman
        });
    }

    /// Observe one spotter-assessed bash command and pick the prefix to probe for promotion, if any
    /// (docs/permissions.md). Bumps the seen-counts for the command's token-prefixes,
    /// then runs the descent against the current effective policy — one lock acquisition.
    pub fn observe_and_pick_probe(
        &self,
        role: &crate::config::Config,
        tokens: &[String],
    ) -> Option<String> {
        let mut probes = self.probes.lock().unwrap();
        probes.observe(tokens);
        probes.next_probe(role, tokens)
    }

    /// Record a probe's vote (`safe` iff the spotter answered `clear_always`) and report whether the
    /// prefix has now reached the promotion threshold `K` — the loop's signal to file a
    /// `Proposed(Command)` (docs/permissions.md).
    pub fn record_probe_vote(&self, prefix: &str, safe: bool) -> bool {
        let mut probes = self.probes.lock().unwrap();
        probes.vote(prefix, safe);
        probes.ready_to_promote(prefix)
    }
}
