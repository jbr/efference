//! The loop's two phases (docs/README.md): idling until a user turn or an event wakes it,
//! then running a trajectory — model request, calls, triage, landing — until the model
//! stops calling tools.

use std::collections::VecDeque;

use super::*;
use crate::inference::api::Completion;
use crate::record::recovery;

/// What waiting for work came to.
pub(super) enum Idle {
    /// The daemon is tearing down.
    Shutdown,
    /// A trajectory starts; `replay` is a resumed session's pending batch, which its
    /// first step brings back.
    Trajectory {
        replay: Option<recovery::Completion>,
    },
}

/// One session's loop: the collaborators every step borrows, and the numbering and the
/// queue that carry from one trajectory to the next.
pub(super) struct AgentLoop<'a, F, S> {
    client: &'a Client,
    frontend: &'a mut F,
    shared: Arc<Session>,
    triager: Triager,
    spotter: S,
    toolbox: Toolbox,
    /// World-facing tools plus the harness tools (`consolidate`, the process-monitor/inbox
    /// tools (docs/process-monitor.md)) handled structurally in the loop, not by a
    /// `Provider`. The defs are constant `json!`, so `tools[]` stays byte-stable across
    /// requests (docs/log-and-projection.md).
    tools: Vec<Value>,
    /// The title ask's owned request surface, built once; each title check hands its
    /// detached task a clone (docs/session-title.md).
    title_ask: TitleAsk,
    consolidation: consolidation::ConsolidationCheck,
    /// Continues the log's numbering: `0` for a new session, the last entry's turn for a
    /// resumed one, so a resumed session's new turns do not reuse a recorded number.
    turn: Turn,
    /// Turns submitted while a resumed session's pending batch waited for the operator's
    /// first action (docs/resumption.md). Delivered after the batch, the way a
    /// turn submitted during a live gate is; one still held when the trajectory ends
    /// before that delivery starts the next.
    held: VecDeque<SubmittedTurn>,
}

impl<'a, F: Frontend, S: Spotter> AgentLoop<'a, F, S> {
    pub(super) fn new(
        client: &'a Client,
        frontend: &'a mut F,
        shared: Arc<Session>,
        triager: Triager,
        spotter: S,
        toolbox: Toolbox,
    ) -> Self {
        let tools = tool_definitions(&toolbox);
        let title_ask = TitleAsk::new(client, &tools, &triager);
        let consolidation = consolidation::ConsolidationCheck::new(client, &tools);
        let turn = shared.latest_turn();
        Self {
            client,
            frontend,
            shared,
            triager,
            spotter,
            toolbox,
            tools,
            title_ask,
            consolidation,
            turn,
            held: VecDeque::new(),
        }
    }

    /// Wait for work, and take in the user turn that is it. Idle: race the next user turn
    /// against a notification wake (docs/process-monitor.md — wake-on-notification). A user
    /// turn drives a normal trajectory; an event wakes a standalone triage that starts a
    /// trajectory only if it pins something. The first turn arrives here too.
    pub(super) async fn idle(&mut self) -> Result<Idle, Error> {
        loop {
            log::debug!("idle — awaiting a user turn or an event on the control socket");
            // A resumed session's unfinished batch, derived from the log each time
            // (docs/resumption.md). While it waits, the wire ends in unanswered `tool_use`
            // blocks, so no triage may project it: an event stays in the inbox and rides the
            // batch's own triage.
            let replay = self.shared.pending_batch();
            match self.next_wake(replay.is_some()).await {
                IdleWake::Shutdown => return Ok(Idle::Shutdown), // daemon tearing down.
                IdleWake::Blank => continue,                     // blank turn: keep idling.
                IdleWake::Event => {
                    // Standalone triage. Start a trajectory only if it changed the
                    // wire (pinned something); otherwise back to idle, the prefix still hot.
                    // A request either way, so the loop reads `requesting` throughout
                    // (docs/control-api.md).
                    self.shared.idle_triage_started();
                    if !run_idle_triage(
                        self.client,
                        &self.tools,
                        &self.shared,
                        &self.triager,
                        &mut self.turn,
                    )
                    .await?
                    {
                        self.shared.idle_triage_settled();
                        continue;
                    }
                    // Triage pinned something: fall into the trajectory with no new user turn — the model
                    // responds to the pinned event at the wire tail.
                }
                IdleWake::Continue => {
                    // The zero-token continue (docs/control-api.md): the operator handed
                    // control back with no input, so nothing is appended and nothing on the
                    // wire moves, and the next request re-sends a byte-identical prefix.
                    // Consuming the signal here is what keeps one gesture to one extra turn.
                    self.shared.take_continue();
                }
                // The batch comes first: a `User` entry after unanswered `tool_use` blocks
                // breaks the wire, so the turn waits until after the results.
                IdleWake::Turn(submitted) if replay.is_some() => self.held.push_back(submitted),
                IdleWake::Turn(submitted) => {
                    self.turn += 1;
                    self.shared.append_submission(
                        self.turn,
                        Payload::User(submitted.blocks()),
                        submitted.submission_id,
                        submitted.source,
                    )?;
                }
            }
            return Ok(Idle::Trajectory { replay });
        }
    }

    /// What wakes the idle loop. Turns held for a pending batch come first once the batch
    /// is gone; while a batch waits, no event wakes a triage.
    async fn next_wake(&mut self, replaying: bool) -> IdleWake {
        if !replaying && let Some(submitted) = self.held.pop_front() {
            return IdleWake::Turn(submitted);
        }
        tokio::select! {
            line = self.frontend.read_user_turn() => IdleWake::from_turn(line),
            _ = self.shared.inbox_wake_while_warm(), if !replaying => IdleWake::Event,
            _ = self.shared.continue_wake() => IdleWake::Continue,
        }
    }

    /// Keep round-tripping while the model calls tools, starting from the wake that
    /// [`idle`](Self::idle) took in.
    pub(super) async fn trajectory(
        &mut self,
        mut replay: Option<recovery::Completion>,
    ) -> Result<(), Error> {
        // A trajectory is starting: the upstream's turn-routing token, if the
        // provider issues one, belongs to the previous trajectory and must not be
        // replayed into this one (docs/transport.md).
        self.client.begin_turn();
        self.triager.begin_turn();

        // A trajectory is starting, so the previous one's give-up is not the current
        // condition (docs/transport.md): the `stall` topic names what the daemon is
        // *now*, and a supervisor that answered a stall by sending a turn sees it clear
        // as that turn begins rather than only when it finishes.
        self.shared.clear_stall();

        // A fresh turn starts un-interrupted: an interrupt fired while the loop was idle has
        // nothing to stop, and a latch left set would silently stop the *next* turn's first
        // call instead (docs/control-channel.md).
        self.shared.take_interrupt();

        // `first_step` skips the pace sleep on the turn that responds to the just-read user
        // message / pinned event — pacing is a *between-turns* delay, so it applies to the tool
        // round-trips that follow, not the first response (docs/control-channel.md).
        let mut first_step = true;
        loop {
            // The config-unparseable park (docs/permissions.md): the file fails to parse
            // *and* the restore fails too, leaving policy in memory that the file no longer
            // shows, so the loop stops here until the file parses again. Checked at every
            // step, not only at the trajectory's start, because an approved config write
            // can land the condition mid-trajectory.
            self.shared.await_config_repair().await;
            let ended = match replay.take() {
                Some(pending) => self.resume_batch(&pending).await?,
                None => self.step(first_step).await?,
            };
            first_step = false;
            if ended {
                return Ok(());
            }
        }
    }

    /// A resumed session's first action brings its pending batch back
    /// (docs/resumption.md): the calls that never ran go through the permission
    /// cascade, and everything captured is triaged with them at one triage, with no
    /// model request first. Returns whether the trajectory ends.
    async fn resume_batch(&mut self, pending: &recovery::Completion) -> Result<bool, Error> {
        let Batch {
            steps,
            abort_turn,
            redirect,
            primer,
            ..
        } = replay_batch(
            self.frontend,
            &self.shared,
            &self.spotter,
            &self.toolbox,
            pending,
        )
        .await?;
        land(
            self.client,
            &self.tools,
            &self.shared,
            &self.triager,
            &mut self.turn,
            steps,
            primer,
        )
        .await?;
        Ok(after_landing(
            &self.shared,
            &mut self.turn,
            abort_turn,
            redirect,
        )?)
    }

    /// One round-trip: deliver what was queued, ask the model, and run and land the calls
    /// it made. Returns whether the trajectory ends.
    async fn step(&mut self, first_step: bool) -> Result<bool, Error> {
        // Pace (docs/control-channel.md): the operator's between-turns wall-clock
        // dial. An interruptible daemon-side sleep at this same juncture, transparent to the
        // agent (never logged/projected) — a submitted turn or a release ends it immediately,
        // so the drain below picks up anything that arrived during the wait.
        if !first_step {
            self.shared.pace_at_juncture().await;
        }
        // The cold check (docs/transport.md): no request reads a cache
        // believed expired until a person has acted. Parks here, a decision
        // point, until a turn, continue, decision or answer arrives; before the
        // drain, so a turn sent meanwhile is delivered at this juncture.
        self.shared.await_warm_or_person().await;
        let landed = self.deliver_queued_turns()?;
        // A user turn just landed — at the turn's start, or delivered here
        // mid-task: the moment the title peek reads the message and the
        // prefix behind it (docs/session-title.md). Detached, so the ask
        // never delays the response to the message.
        if first_step || landed > 0 {
            self.consolidation.check(&self.shared);
            solicit_title_detached(&self.shared, &self.title_ask);
        }
        let Some(completion) = self.request().await? else {
            // The trajectory is ending without an answer (a stall, a
            // refusal, retries exhausted): a title check, as at the clean
            // end (docs/session-title.md).
            solicit_title_detached(&self.shared, &self.title_ask);
            return Ok(true);
        };
        self.turn += 1;
        let calls = commit_completion(&self.shared, completion, self.turn)?;
        if calls.is_empty() {
            self.consolidation.check(&self.shared);
            // The trajectory is ending: a title check, the moment the title peek reads the
            // whole of it (docs/session-title.md). Detached, so the triage reads the prefix
            // this turn just cached while the loop idles.
            solicit_title_detached(&self.shared, &self.title_ask);
            return Ok(true); // model is done with this turn → wait for the next.
        }
        self.run_and_land(&calls).await
    }

    /// Mid-task delivery of queued turns (docs/control-channel.md): deliver any user turns
    /// submitted while this trajectory was in flight, at the juncture — after the
    /// last tool's results landed, before the next model call. Each lands as a
    /// canonical `Payload::User` (never the inbox/triage path — a user turn is
    /// undismissable), which `project` merges into the trailing tool_result message
    /// as `[tool_result…, text]`. A turn arriving while the loop is idle still takes
    /// the fast path ([`idle`](Self::idle)'s `select!`); this only catches mid-trajectory
    /// ones. Returns how many landed.
    fn deliver_queued_turns(&mut self) -> std::io::Result<usize> {
        let injected = self.frontend.drain_queued_turns();
        let mut landed = 0;
        for submitted in self.held.drain(..).chain(injected) {
            landed += 1;
            self.turn += 1;
            // A user turn lands mid-trajectory: to the upstream that is a new
            // turn, so its routing token starts over (docs/transport.md).
            self.client.begin_turn();
            self.triager.begin_turn();
            self.shared.append_submission(
                self.turn,
                Payload::User(submitted.blocks()),
                submitted.submission_id,
                submitted.source,
            )?;
        }
        Ok(landed)
    }

    /// Project the log and ask the model for the next turn. `None` is a trajectory ending
    /// without an answer.
    async fn request(&mut self) -> Result<Option<Completion>, Error> {
        // Project, then place the single trailing cache breakpoint just before
        // sending — projection stays pure; the breakpoint walks the tail each
        // request (docs/log-and-projection.md).
        let mut turns = self.shared.project_turns();
        turns.push(WireTurn::Breakpoint);
        // One tool call per turn (docs/README.md) — the agent picks its next single
        // action; the fork's `retain` is the only forced call.
        // Supersede the idle line before blocking on the model (docs/transport.md): a
        // stale `idle` during a live generation reads as *finished* and invites
        // interrupting or abandoning the session on false signal.
        log::debug!("generating — turn {}", self.turn);
        self.shared.set_phase(LoopPhase::Requesting);
        let completion = complete_with_retries(
            self.client,
            &self.shared,
            &self.tools,
            &turns,
            &mut self.toolbox,
            &mut self.turn,
        )
        .await?;
        // The all-clear, on every exit from the retry loop: the attempt that finalized,
        // the exhausted budget, or a failed request (docs/transport.md).
        self.client.upstream_retry().clear(RetrySurface::Turn);
        Ok(completion)
    }

    /// Run all of a completion's calls, holding the results, and triage them together in
    /// one request (docs/README.md). Triaging between calls would build a triage request
    /// whose trailing assistant message still carries unanswered `tool_use` blocks, which
    /// the API rejects (docs/triage.md). Returns whether the trajectory ends.
    async fn run_and_land(&mut self, calls: &[ToolCall]) -> Result<bool, Error> {
        // The triage-lineage primer plants the text view's history through this
        // completion on the own triage's client (docs/transport.md), so the
        // triage's request extends one that ended there. Armed only where the own
        // client plants one; shared triages skip it without spending a request.
        let primer_turns = self.triager.primer_turns(&self.shared.snapshot(), calls);
        let primer = primer_turns.as_deref().map(|turns| TriagePrimerInput {
            triager: &self.triager,
            turns,
        });
        let Batch {
            steps,
            abort_turn,
            redirect,
            primer,
            ..
        } = run_batch(
            self.frontend,
            &self.shared,
            &self.spotter,
            &self.toolbox,
            self.turn,
            calls,
            primer,
        )
        .await?;

        land(
            self.client,
            &self.tools,
            &self.shared,
            &self.triager,
            &mut self.turn,
            steps,
            primer,
        )
        .await?;

        if calls.iter().any(|call| call.name == "ask") {
            self.consolidation.check(&self.shared);
        }
        let ended = after_landing(&self.shared, &mut self.turn, abort_turn, redirect)?;
        if ended {
            // An aborted trajectory ends without an answer: a title check, as
            // at the clean end (docs/session-title.md).
            solicit_title_detached(&self.shared, &self.title_ask);
        }
        Ok(ended)
    }
}
