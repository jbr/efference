//! Running one call of a completion: the permission cascade (docs/permissions.md)
//! and the tool run, up to but not including triage.
//!
//! A completion may make several calls (docs/README.md), so the loop runs each call
//! here and triages the whole batch at one gate (docs/redaction-fork.md). Splitting execution
//! from triage is what keeps the fork's request valid: a peek fired between calls would
//! carry unanswered `tool_use` blocks and be rejected.

use super::*;

use crate::log::ToolTelemetry;

/// What running one call produced, before triage.
pub(super) enum Decision {
    /// A real result to triage at the gate.
    Ran {
        result: RawResult,
        /// What the call cost and how it came out (docs/tools.md) — measured
        /// here, appended by the caller beside the result it describes. Boxed
        /// because it is carried through two enums whose other variants are
        /// small, and an unboxed field would size every one of them to it.
        telemetry: Box<ToolTelemetry>,
        /// The operator's note from a foreground→background handoff, delivered as a
        /// user turn after the step's results (docs/supervisor.md).
        handoff: Option<String>,
        /// Who answered, when the call was an `ask` (docs/supervisor.md).
        reply: Option<Box<Reply>>,
    },
    /// Payloads to append as they stand — they never reach the gate, because there is
    /// no world output to curate (`consolidate`) or the call never ran. The caller
    /// appends them, so each is durable before the next call runs.
    Direct(Vec<Payload>),
    /// The operator interrupted while this call ran (docs/control-channel.md): it
    /// finished, but its output stops at the log. Taken *before* the gate, so the
    /// discarded result is never peeked at and costs no round-trip.
    Withheld(Payload),
    /// The human aborted at the gate; the turn ends.
    Abort(Payload),
    /// The human replied instead of approving; their message becomes the next user turn.
    Redirect { payload: Payload, message: String },
}

/// One step of the trajectory, in call order: a call's outcome once it is on the log.
/// A captured result is already durable and waits only for the verdict that names it,
/// so what a step carries is that verdict's address rather than its output.
pub(super) enum Step {
    /// Nothing left to land: the step's payloads are on the log already, and there is
    /// no world output for a gate to curate.
    Direct,
    Ran {
        call: ToolCall,
        /// The seq the result was captured at, which the verdict it is still waiting
        /// for names (docs/redaction-fork.md).
        result_seq: crate::log::Seq,
        /// What the log holds for that result — masked on the way in (docs/env.md),
        /// so triage reads the durable bytes rather than the tool's own output.
        result: RawResult,
        handoff: Option<String>,
    },
}

impl Step {
    /// Append a payload that needs no verdict and record the step that says so.
    pub(super) fn direct(
        shared: &Session,
        turn: Turn,
        payload: Payload,
    ) -> Result<Self, std::io::Error> {
        shared.append(turn, payload)?;
        Ok(Step::Direct)
    }
}

/// Take one call through the permission cascade and, if approved, run it
/// (docs/permissions.md, docs/tools.md). Appends only the audit entries the cascade
/// itself owns — the `Spot` verdict and any `ConfigEdit` — never the result, which the
/// caller captures the moment this returns (docs/redaction-fork.md).
#[allow(clippy::too_many_arguments)] // each is a distinct party to the one call
pub(super) async fn run_call<F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Session>,
    spotter: &S,
    toolbox: &Toolbox,
    turn: Turn,
    call: &ToolCall,
    ask_at: AskPosition,
    primer_go: Option<&tokio::sync::watch::Sender<bool>>,
) -> Result<Decision, Error> {
    // An un-asked peek tool (docs/redaction-fork.md) never enters the cascade — there
    // is nothing to judge or run; the paired nudge result tells the model to carry on.
    if is_peek_tool(&call.name) {
        return Ok(Decision::Direct(vec![unasked(call)]));
    }
    // A call naming no tool this turn advertised is answered here, ahead of the
    // permission cascade (docs/permissions.md). There is nothing to decide: the call
    // cannot run whatever the answer, so no policy question arises and no rung should
    // be asked one. The spotter, asked anyway, returned `clear` for a nonexistent tool
    // with a rationale it invented for the call it imagined
    // (journal/2026-08-07-session-key-review.md) — a confabulated verdict that
    // `spotter-log` then counted as a clean clear. The corrective result text
    // (`tools::unknown_tool_message`) is the whole answer, and it reaches the wire
    // verbatim: a `Direct` payload never enters the gate, so no summary stands between
    // the correction and the model.
    let callable = super::callable_names(toolbox);
    if !callable.contains(&call.name) {
        return Ok(Decision::Direct(vec![unknown_tool(call, &callable)]));
    }
    // `ask` puts a question to the operator (docs/supervisor.md): there is nothing in the
    // world to authorize, so it skips the cascade, and the wait for the reply is the
    // call. The interrupt's *after* juncture applies as for any call.
    if call.name == supervisor::ASK {
        // An answered ask lands in the gate, so its triage is certain short of an
        // interrupt — fire the primer over the operator's deliberation window
        // (backlog/separate-triage-cold-prefix.md).
        if let Some(go) = primer_go {
            let _ = go.send(true);
        }
        let started = std::time::Instant::now();
        let (result, reply) = ask_result(shared, call, ask_at).await;
        let result = cap_tool_result(call, result);
        let telemetry = Box::new(telemetry::observe(
            call,
            &result,
            started.elapsed(),
            result.is_error,
            None,
        ));
        if shared.take_interrupt() {
            return Ok(Decision::Withheld(withheld(call, result)));
        }
        return Ok(Decision::Ran {
            result,
            telemetry,
            handoff: None,
            reply,
        });
    }
    // The guide overlay's one closed door (docs/context.md): a call that names an
    // `efference://` path with anything but `read` is answered here, ahead of the cascade.
    // The prefix addresses documents compiled into the binary, not files, so no rung's
    // answer could make the call run — and a gate spent on it would ask the operator a
    // question with no second option. The corrective text names the paths that do work,
    // and rides to the wire verbatim as a `Direct` payload.
    if let Some(result) = crate::guide::reject_non_read(call) {
        return Ok(Decision::Direct(vec![Payload::tool_result(
            call.id.clone(),
            result,
            WireState::Raw,
        )]));
    }
    // A `sessions__approve` call authorizes the target's held move, so the parent's
    // permission cascade judges that move rather than the id (docs/permissions.md).
    // Resolve it first: a moved gate or a refused target is the whole answer, and
    // nothing should be classified. The logged `ToolCall` is untouched — the wire
    // carries what the model wrote.
    let bound = if call.name == crate::control_tools::APPROVE_WIRE_NAME {
        match toolbox.resolve_bound_action(call) {
            Ok(bound) => Some(bound),
            Err(e) => {
                return Ok(Decision::Direct(vec![Payload::tool_result(
                    call.id.clone(),
                    RawResult::error(format!("not run — {e}")),
                    WireState::Raw,
                )]));
            }
        }
    } else {
        None
    };
    // The session's own config file is its policy *interface*: a write to it is a proposal
    // that must always reach the operator — never auto-cleared by a path grant, never
    // spotter-cleared (docs/permissions.md). It bypasses the whole cascade straight to the
    // human gate; on approval the write lands and fs-watch folds the diff into effective
    // policy.
    let config_write = shared
        .config_path()
        .is_some_and(|cfg| policy::is_config_write(shared.cwd(), call, cfg));
    // A hand-off gives this session's place to a new one (docs/tools.md): the operator's
    // decision, like a policy edit, so it takes the same route past the cascade.
    let hand_off = call.name == crate::control_tools::HAND_OFF_WIRE_NAME;
    let approval = if config_write || hand_off {
        frontend.approve(call, bound.as_ref()).await
    } else {
        judge(
            frontend,
            shared,
            spotter,
            toolbox,
            turn,
            call,
            bound.as_ref(),
        )
        .await?
    };
    // A `ConfigEdit` is the orchestrator legislating policy (docs/permissions.md): append
    // the change (which folds into the effective config), then re-classify *this* call
    // against the new policy. The call runs only if the changed rule now clears it — it
    // never rides along unchecked.
    let approval = match approval {
        Approval::ConfigEdit { change, note } => {
            commit_config_edit(shared, turn, call, change, note)?
        }
        other => other,
    };

    match approval {
        // `consolidate` acts on the log, not the world (docs/consolidate.md): record the
        // boundary mark + pair the `tool_use`. No world-run, no distill — there is no tool
        // output to curate, so it never reaches the gate.
        Approval::Approve if call.name == consolidate::NAME => {
            let note = consolidate::summary(&call.input).unwrap_or_default();
            let log = shared.snapshot();
            let upto = consolidate::extent(&log);
            // The boundary is checked where it is made, not where it is projected: a
            // mark is append-only, and a fold that cut a call from its result would
            // replay an unpaired item on every later request (docs/consolidate.md).
            // `extent` never produces one in ordinary use; this refuses the write if
            // an append ordering ever changes that.
            let split = consolidate::split_calls(&log, upto);
            if !split.is_empty() {
                let ids: Vec<&str> = split.iter().map(|id| id.0.as_str()).collect();
                log::error!("consolidate refused: a boundary at {upto} would split {ids:?}");
                return Ok(Decision::Direct(vec![Payload::tool_result(
                    call.id.clone(),
                    RawResult::error(format!(
                        "consolidate refused: the boundary would separate a call from \
                         its result ({}); nothing was folded",
                        ids.join(", ")
                    )),
                    WireState::Raw,
                )]));
            }
            Ok(Decision::Direct(vec![
                Payload::Mark(Mark::boundary(upto, note)),
                Payload::tool_result(
                    call.id.clone(),
                    RawResult::text("consolidated"),
                    WireState::Raw,
                ),
            ]))
        }
        Approval::Approve => {
            // From here the call may touch the world, so the log says so before it
            // does (docs/log-and-projection.md): a call with this marker and no
            // result is the one a resumed session drops rather than reruns.
            shared.append(
                turn,
                Payload::CallStarted {
                    call_id: call.id.clone(),
                },
            )?;
            // Approved, so a result the gate triages will follow short of an
            // interrupt — fire the primer over the execution window
            // (backlog/separate-triage-cold-prefix.md).
            if let Some(go) = primer_go {
                let _ = go.send(true);
            };
            // Acquire the result. A backgrounded bash spawn returns immediately — the child
            // runs on under the supervisor and its output surfaces at later gates
            // (docs/supervisor.md); everything else runs to completion through the toolbox.
            // `interrupted()` resolves on a `POST /loop/interrupt` — the tool's kill trigger
            // (kills the process group). Tool output streams to the control stream's `output` class
            // chunk-by-chunk as it runs (bash; the MCP tools resolve whole), tagged by call id.
            let mut handoff = None;
            // The `p#<n>` the supervisor minted for this call, if it reached a
            // spawn — recorded on the telemetry row so a reader can join the
            // call to the process namespace (docs/supervisor.md).
            let mut process_handle: Option<String> = None;
            // The telemetry clock starts here, after the cascade: a call that
            // waited at a human gate did not *run* for that long (docs/tools.md).
            let started = std::time::Instant::now();
            // Set by the arms that construct a harness-authored failure, so the
            // outcome class can tell "the harness could not run this" from "the
            // tool ran and declined" (`telemetry::observe`).
            let mut harness_error = false;
            let result = if bash::is_background_call(call) {
                match background_command(call) {
                    // The spawning `tool_use` id rides along, so the process and the
                    // transcript entry can each find the other (docs/supervisor.md).
                    Some(command) => {
                        match shared.spawn_background(command, Some(call.id.0.clone())) {
                            Ok(handle) => {
                                // The async pattern is stated here, where the shape gets
                                // chosen — the watch_process result says it too, but that
                                // is read only after a compressed start→watch→kill turn
                                // has already been attempted, and a short process can exit
                                // before the turn composing follow-up calls reaches it
                                // (docs/supervisor.md).
                                let content = format!(
                                    "backgrounded as {handle}. its recent output will surface in \
                                     your inbox at each step, and you'll get a notification when \
                                     it exits. to react to specific output, register a \
                                     watch_process and end your turn — the match arrives in your \
                                     inbox, and a short process can exit before further calls \
                                     composed this turn reach it."
                                );
                                process_handle = Some(handle);
                                RawResult::text(content)
                            }
                            Err(e) => {
                                harness_error = true;
                                RawResult::error(format!("failed to background: {e}"))
                            }
                        }
                    }
                    None => {
                        harness_error = true;
                        RawResult::error("background bash requires a `command`")
                    }
                }
            } else if let Some(r) = crate::guide::read_documents(call) {
                // The guide overlay (docs/context.md): a `read` of the virtual mount is
                // answered from the binary, before the real `read` sees a path it could
                // not stat. What comes back is an ordinary `RawResult` on an ordinary
                // `tool_result`, so triage, the log, and `project` never learn it was
                // served from anywhere unusual.
                r
            } else if let Some(r) = supervisor_tool_result(shared, call).await {
                // A supervisor/inbox read/act tool (docs/supervisor.md) — resolves against
                // daemon state; `wait` yields on the inbox signal while the other tools return.
                r
            } else {
                if call.name == bash::NAME {
                    // A detach request that never found a running tool must not label this
                    // fresh command's (potential) handoff — drop any stale note first.
                    shared.take_detach_note();
                }
                // A foreground bash command joins the process namespace the moment it
                // spawns (docs/supervisor.md): the spawn hook mints its id, and the
                // chunk sink feeds the supervisor's capture alongside the live stream —
                // which is what lets `GET /processes/:id/output` seed a cold-opened
                // client mid-run, and what makes the handoff below a property flip
                // rather than a re-registration. A `Mutex` (not a `Cell`) so the loop
                // future stays `Send`.
                let fg_id = std::sync::Mutex::new(None::<u64>);
                let command = call
                    .input
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let spawning_call = call.id.0.clone();
                let outcome = toolbox
                    .run(
                        call,
                        frontend.interrupted(),
                        |pgid| {
                            *fg_id.lock().unwrap() = Some(shared.register_foreground(
                                command,
                                pgid,
                                Some(spawning_call),
                            ));
                        },
                        |chunk| {
                            if let Some(id) = *fg_id.lock().unwrap() {
                                shared.push_process_chunk(id, chunk);
                            }
                            shared.publish_delta(StreamDelta::ToolOutput {
                                call_id: call.id.0.clone(),
                                chunk: String::from_utf8_lossy(chunk).into_owned(),
                            });
                        },
                        // A connected server's async speech (docs/inbox.md):
                        // the same enqueue every other notification source
                        // uses, superseding on the server's slot.
                        |notification| {
                            shared.notify(
                                notification.source,
                                notification.body,
                                notification.slot,
                                None,
                            );
                        },
                    )
                    .await;
                match outcome {
                    bash::Outcome::Done(result) => {
                        // The paired `tool_result` is the canonical output now; the
                        // registry entry has done its job. Its handle stays on the
                        // telemetry row, which is where a foreground process's
                        // ordinal survives the removal (docs/supervisor.md).
                        if let Some(id) = fg_id.lock().unwrap().take() {
                            process_handle = Some(crate::supervisor::handle(id));
                            shared.conclude_foreground(id);
                        }
                        result
                    }
                    // The foreground→background handoff (docs/supervisor.md): the operator
                    // detached the running command, so flip its registry entry to
                    // background — the capture already spans the whole run — and pair the
                    // `tool_use` with a handoff result (`is_error: false`; unlike Ctrl-C, the
                    // command wasn't lost, it moved).
                    bash::Outcome::Detached(detached) => {
                        let id = fg_id
                            .lock()
                            .unwrap()
                            .take()
                            .expect("a detach only reaches a spawned (registered) command");
                        let auto_after = detached.auto_after;
                        let handle = shared.adopt_background(*detached, id);
                        process_handle = Some(handle.clone());
                        handoff = shared.take_detach_note();
                        // The two detaches read differently to the agent. An operator
                        // detach is something that happened *to* the call; an auto-detach
                        // is the deadline the call itself ran past, and is the one moment
                        // `background_after` is worth knowing about — three sessions hit
                        // the pre-timer hang and none learned anything, because the
                        // failure was silence (docs/supervisor.md).
                        let content = match auto_after {
                            Some(after) => format!(
                                "this command was still running after {}s, so it moved to \
                                 the background on its own as {handle}. nothing was killed \
                                 and no output was lost. if you expected it to run this \
                                 long, pass `background_after` (seconds) on calls like it, \
                                 or `background: true` to skip blocking altogether; if you \
                                 expected it to finish, read_process {handle} to see what \
                                 it is doing. note that starting a process with a shell \
                                 `&` inside the command keeps this call's output open, so \
                                 the call cannot return on its own.",
                                after.as_secs()
                            ),
                            None => format!(
                                "the user moved this command to the background as {handle}. \
                                 it is still running: its recent output will surface in your \
                                 inbox at each step, you'll get a notification when it exits, \
                                 and you can read_process/kill_process it like any background \
                                 process."
                            ),
                        };
                        RawResult::text(content)
                    }
                }
            };
            let elapsed = started.elapsed();
            // Bound the result before it touches anything (defense in depth): the fork
            // peek, the wire, and the logged `raw`. An over-cap result becomes a hard error
            // so the model narrows the call.
            let result = cap_tool_result(call, result);
            // A config write is the one call whose *effect* is decided after it runs
            // (docs/permissions.md): the file must always parse, so we re-read it here —
            // synchronously, before the result is logged — and either adopt the edit or
            // revert it. Reverting turns the result into an error carrying the parse
            // failure, so the author learns its edit didn't take *in the tool result it's
            // already reading*, rather than believing an approved write took effect and
            // thrashing against a wall it thinks it cleared. The fs-watch task calls the
            // same refresh, so an edit that arrives any other way gets identical treatment.
            let result = if config_write {
                config_write_outcome(shared, result)
            } else {
                result
            };
            // Measured after the cap and the config-write re-read, so
            // `result_bytes` and the outcome class describe the result that is
            // actually logged rather than an intermediate one.
            let telemetry = Box::new(telemetry::observe(
                call,
                &result,
                elapsed,
                harness_error,
                process_handle,
            ));
            // The interrupt's *after* juncture (docs/control-channel.md).
            if shared.take_interrupt() {
                return Ok(Decision::Withheld(withheld(call, result)));
            }
            Ok(Decision::Ran {
                result,
                telemetry,
                handoff,
                reply: None,
            })
        }
        Approval::Abort => Ok(Decision::Abort(aborted(call))),
        // Reject this call but keep the turn alive: the human's message becomes the next
        // user turn so the model can course-correct instead of bailing (docs/README.md).
        Approval::Redirect(message) => Ok(Decision::Redirect {
            payload: rejected(call),
            message,
        }),
        // Resolved into Approve/Redirect by `commit_config_edit` above before this match.
        Approval::ConfigEdit { .. } => unreachable!("grant is legislated above"),
    }
}
/// The permission cascade for one call (docs/permissions.md). `bound` is the leaf
/// action a `sessions__approve` authorizes, when the call is one: a granted
/// `sessions__approve` clears blind — the config's statement of trust in the
/// coordinator — and otherwise the leaf, not the id, is what the fast-path, the
/// spotter, and the human gate judge. Returns the decision; the caller commits
/// any `ConfigEdit` it carries.
pub(super) async fn judge<F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Session>,
    spotter: &S,
    toolbox: &Toolbox,
    turn: Turn,
    call: &ToolCall,
    bound: Option<&crate::control_tools::BoundAction>,
) -> Result<Approval, Error> {
    // Classify against the *effective* policy — base role plus the fold of every
    // grant so far: a prior amendment can promote this very call into the
    // fast-path, and the spotter sees the same scope.
    let role = shared.effective_config();
    // A granted `sessions__approve` clears blind (docs/permissions.md): the doc
    // states that as trust in the coordinator, so the child's leaf is not
    // re-litigated on this path.
    if bound.is_some() && policy::tool_granted(&role, &call.name) {
        return Ok(Approval::Approve);
    }
    // The judged call: the child's leaf when bound, else the parent's own call.
    // The id stays the parent's, so the `Spot` audit entry and the gate correlate
    // to the call actually held. The leaf's cwd is what its relative paths resolve
    // against; the parent's is used otherwise.
    let leaf;
    let (judged, cwd): (&ToolCall, &std::path::Path) = match bound {
        Some(bound) => {
            leaf = ToolCall::new(bound.name.clone(), call.id.clone(), bound.input.clone());
            (&leaf, bound.cwd.as_path())
        }
        None => (call, shared.cwd()),
    };
    match policy::classify(cwd, &role, judged) {
        Verdict::Clear => Ok(Approval::Approve),
        // A `[commands]` deny (docs/permissions.md): the operator already wrote the answer
        // for this prefix, so the spotter is not asked — the move goes straight to whoever
        // gates. Autonomous has no synchronous human, so it takes the same redirect a
        // spotter `Gate` takes there, carrying the reason the config gave it.
        Verdict::Deny => {
            if shared.autonomous() {
                Ok(Approval::Redirect(denied_gate_message(
                    shared.config_path(),
                )))
            } else {
                Ok(frontend.approve(call, bound).await)
            }
        }
        // HumanOnly (docs/permissions.md): the human takes the spotter's role
        // *entirely* — skip the spotter (pay no tokens, log no `Spot`) and gate
        // straight to the human, who both decides this call and grows the allowlist by
        // hand. Learning is relocated to the human, not lost.
        Verdict::Gate if shared.review_level() == ReviewLevel::HumanOnly => {
            Ok(frontend.approve(call, bound).await)
        }
        Verdict::Gate => {
            // Prefix-promotion probe (docs/permissions.md): on a bash move, observe
            // its recurrence and pick the shortest prefix worth asking the spotter to
            // vouch for *for all arguments*. The spotter answers via the stance
            // (`clear_always`), so the probe rides the one spot call. A bound leaf's
            // command belongs to the child's own ledger, so no probe rides an approval.
            let probe = if bound.is_none() && judged.name == bash::NAME {
                judged
                    .input
                    .get("command")
                    .and_then(Value::as_str)
                    .and_then(|command| policy::bash_tokens(shared.cwd(), &role, command))
                    .and_then(|tokens| shared.observe_and_pick_probe(&role, &tokens))
            } else {
                None
            };
            // What the turn's whole `tools[]` knows about this tool — providers
            // plus the loop-owned family (docs/permissions.md). `None` for bash,
            // whose command text is its own description. Built from the leaf's name
            // when bound, so the spotter reads the child's tool.
            let brief = super::brief(toolbox, &judged.name);
            let chain: &[String] = bound.map(|b| b.chain.as_slice()).unwrap_or(&[]);
            let spotting = spotter
                .assess(&role, judged, brief.as_ref(), chain, probe.as_deref())
                .await;
            // What actually answered, read off the response rather than the
            // configured id (docs/permissions.md). Taken before the usage moves
            // into the ledger; `None` when no request happened at all.
            let model = spotting.usage.as_ref().and_then(|u| u.model.clone());
            if let Some(u) = spotting.usage {
                shared.record_request(RequestOrigin::Spot, u);
            }
            let stance = spotting.read.stance;
            let reason = spotting.read.reason;
            let origin = spotting.origin;
            let axes = spotting.axes;
            shared.append(
                turn,
                Payload::Spot(Spot {
                    call_id: call.id.clone(),
                    stance,
                    reason: reason.clone(),
                    origin,
                    model,
                    probed_prefix: probe.clone(),
                    axes,
                }),
            )?;
            // The probe's answer *is* the stance: `clear_always` is the safe-vote for
            // the probed prefix, any other stance is not-safe (a fail-closed fallback
            // didn't judge, so it casts no vote). When the safe-votes cross the
            // consensus threshold, file a non-blocking `Proposed(Command)` — inert
            // until an orchestrator-above commits it. Nothing rides along: the proposal
            // can't clear anything until committed, and this very call is still decided
            // below.
            if let Some(prefix) = &probe
                && origin == Origin::Model
                && shared.record_probe_vote(prefix, stance == Stance::ClearAlways)
                && let Some(command) = policy::promotable_command(&role, prefix)
            {
                shared.append(
                    turn,
                    Payload::ConfigEdit(ConfigEdit {
                        state: EditState::Proposed,
                        resolves: None,
                        change: ConfigChange::Command { command },
                        note: format!(
                            "spotter: `{prefix}` judged safe for all arguments — \
                             proposed for the allowlist"
                        ),
                    }),
                )?;
            }
            match stance {
                // Clear (and its promotion refinement) auto-approve the move, no human.
                Stance::Clear | Stance::ClearAlways => Ok(Approval::Approve),
                // The autonomous flip (docs/permissions.md): with no synchronous human,
                // don't block on an absent gate — redirect the model to its config file,
                // the one policy interface it has. (The config write itself still gates
                // for the orchestrator-above.) Supervised: route to the human gate.
                Stance::Watch | Stance::Gate => {
                    if shared.autonomous() {
                        // A structured call whose missing grants the fast path can name
                        // gets the diagnosis, not just the verdict (docs/permissions.md):
                        // the failure it answers is a model reading a gated `edit` as a
                        // broken tool and rewriting the file through bash instead.
                        Ok(Approval::Redirect(
                            match policy::missing_grants(cwd, &role, judged) {
                                Some(missing) => {
                                    missing_grant_message(shared.config_path(), &reason, &missing)
                                }
                                None => autonomous_gate_message(shared.config_path(), &reason),
                            },
                        ))
                    } else {
                        Ok(frontend.approve(call, bound).await)
                    }
                }
            }
        }
    }
}
