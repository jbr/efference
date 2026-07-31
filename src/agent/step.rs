//! Running one call of a completion: the permission cascade (docs/permissions.md)
//! and the tool run, up to but not including triage.
//!
//! A completion usually makes one call, but `disable_parallel_tool_use` is a request
//! the provider may ignore (docs/README.md), so the loop runs each call here and
//! triages the whole batch at one gate (docs/redaction-fork.md). Splitting execution
//! from triage is what keeps the fork's request valid: a peek fired between calls would
//! carry unanswered `tool_use` blocks and be rejected.

use super::*;

/// What running one call produced, before triage.
pub(super) enum Decision {
    /// A real result to triage at the gate.
    Ran {
        result: RawResult,
        /// The operator's note from a foreground→background handoff, delivered as a
        /// user turn after the step's results (docs/supervisor.md).
        handoff: Option<String>,
    },
    /// Payloads to append verbatim — they never reach the gate, because there is no
    /// world output to curate (`consolidate`) or the call never ran.
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

/// One step of the trajectory: the calls' outcomes, held in order so the loop can append
/// them after the gate has decided every result's wire form.
pub(super) enum Step {
    Direct(Vec<Payload>),
    Ran {
        call: ToolCall,
        result: RawResult,
        handoff: Option<String>,
    },
}

impl Step {
    pub(super) fn direct(payload: Payload) -> Self {
        Step::Direct(vec![payload])
    }
}

/// Take one call through the permission cascade and, if approved, run it
/// (docs/permissions.md, docs/tools.md). Appends only the audit entries the cascade
/// itself owns — the `Spot` verdict and any `ConfigEdit` — never the result, which the
/// caller appends after the gate.
pub(super) async fn run_call<F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Shared>,
    spotter: &S,
    toolbox: &Toolbox,
    turn: Turn,
    call: &ToolCall,
) -> Result<Decision, Error> {
    // An un-asked peek tool (docs/redaction-fork.md) never enters the cascade — there
    // is nothing to judge or run; the paired nudge result tells the model to carry on.
    if is_peek_tool(&call.name) {
        return Ok(Decision::Direct(vec![unasked(call)]));
    }
    // The permission cascade (docs/permissions.md). First the deterministic fast-path: a
    // `Clear` verdict auto-approves with zero model cost. What it can't clear falls to
    // the independent spotter — a fresh-context model rung that re-decides on the
    // residual before the human does, returning a graded stance. Its verdict is logged as
    // a `Spot` audit entry regardless of outcome (fast-path clears stay unlogged), and its
    // usage reads the shared spotter prefix, so it joins the cache telemetry
    // (docs/log-and-projection.md). Classify against the *effective* policy — base role
    // plus the fold of every grant so far: a prior amendment can promote this very call
    // into the fast-path, and the spotter sees the same scope.
    let role = shared.effective_config();
    // The session's own config file is its policy *interface*: a write to it is a proposal
    // that must always reach the operator — never auto-cleared by a path grant, never
    // spotter-cleared (docs/permissions.md). It bypasses the whole cascade straight to the
    // human gate; on approval the write lands and fs-watch folds the diff into effective
    // policy.
    let config_write = shared
        .config_path()
        .is_some_and(|cfg| policy::is_config_write(call, cfg));
    let approval = if config_write {
        frontend.approve(call).await
    } else {
        match policy::classify(&role, call) {
            Verdict::Clear => Approval::Approve,
            // HumanOnly (docs/permissions.md): the human takes the spotter's role
            // *entirely* — skip the spotter (pay no tokens, log no `Spot`) and gate
            // straight to the human, who both decides this call and grows the allowlist by
            // hand. Learning is relocated to the human, not lost.
            Verdict::Gate if shared.review_level() == ReviewLevel::HumanOnly => {
                frontend.approve(call).await
            }
            Verdict::Gate => {
                // Prefix-promotion probe (docs/permissions.md): on a bash move, observe
                // its recurrence and pick the shortest prefix worth asking the spotter to
                // vouch for *for all arguments*. The spotter answers via the stance
                // (`clear_always`), so the probe rides the one spot call.
                let probe = if call.name == bash::NAME {
                    call.input
                        .get("command")
                        .and_then(Value::as_str)
                        .and_then(policy::simple_command_tokens)
                        .and_then(|tokens| shared.observe_and_pick_probe(&role, &tokens))
                } else {
                    None
                };
                let spotting = spotter.assess(&role, call, probe.as_deref()).await;
                if let Some(u) = spotting.usage {
                    shared.record_request(RequestOrigin::Spot, u);
                }
                let stance = spotting.read.stance;
                let reason = spotting.read.reason;
                let origin = spotting.origin;
                shared.append(
                    turn,
                    Payload::Spot(Spot {
                        call_id: call.id.clone(),
                        stance,
                        reason: reason.clone(),
                        origin,
                        probed_prefix: probe.clone(),
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
                    Stance::Clear | Stance::ClearAlways => Approval::Approve,
                    // The autonomous flip (docs/permissions.md): with no synchronous human,
                    // don't block on an absent gate — redirect the model to its config file,
                    // the one policy interface it has. (The config write itself still gates
                    // for the orchestrator-above.) Supervised: route to the human gate.
                    Stance::Watch | Stance::Gate => {
                        if shared.autonomous() {
                            Approval::Redirect(autonomous_gate_message(
                                shared.config_path(),
                                &reason,
                            ))
                        } else {
                            frontend.approve(call).await
                        }
                    }
                }
            }
        }
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
            let upto = consolidate::extent(&shared.snapshot());
            Ok(Decision::Direct(vec![
                Payload::Mark(Mark {
                    kind: MarkKind::Boundary { upto },
                    note,
                }),
                Payload::ToolResult {
                    call_id: call.id.clone(),
                    raw: RawResult {
                        content: "consolidated".into(),
                        is_error: false,
                    },
                    wire: WireState::Raw,
                },
            ]))
        }
        Approval::Approve => {
            // Acquire the result. A backgrounded bash spawn returns immediately — the child
            // runs on under the supervisor and its output surfaces at later gates
            // (docs/supervisor.md); everything else runs to completion through the toolbox.
            // `interrupted()` resolves on a `POST /interrupt` — the tool's kill trigger
            // (kills the process group). Tool output streams to `GET /stream`
            // chunk-by-chunk as it runs (bash; the MCP tools resolve whole), tagged by call id.
            let mut handoff = None;
            let result = if bash::is_background_call(call) {
                match background_command(call) {
                    Some(command) => match shared.spawn_background(command) {
                        Ok(handle) => RawResult {
                            content: format!(
                                "backgrounded as {handle}. its recent output will surface in \
                                 your inbox at each step, and you'll get a notification when \
                                 it exits."
                            ),
                            is_error: false,
                        },
                        Err(e) => RawResult {
                            content: format!("failed to background: {e}"),
                            is_error: true,
                        },
                    },
                    None => RawResult {
                        content: "background bash requires a `command`".into(),
                        is_error: true,
                    },
                }
            } else if let Some(r) = supervisor_tool_result(shared, call) {
                // A supervisor/inbox read/act tool (docs/supervisor.md) — resolves against
                // daemon state synchronously, no world run.
                r
            } else {
                if call.name == bash::NAME {
                    // A detach request that never found a running tool must not label this
                    // fresh command's (potential) handoff — drop any stale note first.
                    shared.take_detach_note();
                }
                let outcome = toolbox
                    .run(call, frontend.interrupted(), |chunk| {
                        shared.publish_delta(StreamDelta::ToolOutput {
                            call_id: call.id.0.clone(),
                            chunk: chunk.to_string(),
                        });
                    })
                    .await;
                match outcome {
                    bash::Outcome::Done(result) => result,
                    // The foreground→background handoff (docs/supervisor.md): the operator
                    // detached the running command, so adopt the live child into the
                    // supervisor — seeded with everything already streamed — and pair the
                    // `tool_use` with a handoff result (`is_error: false`; unlike Ctrl-C, the
                    // command wasn't lost, it moved).
                    bash::Outcome::Detached(detached) => {
                        let handle = shared.adopt_background(*detached);
                        handoff = shared.take_detach_note();
                        RawResult {
                            content: format!(
                                "the user moved this command to the background as {handle}. \
                                 it is still running: its recent output will surface in your \
                                 inbox at each step, you'll get a notification when it exits, \
                                 and you can read_process/kill_process it like any background \
                                 process."
                            ),
                            is_error: false,
                        }
                    }
                }
            };
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
            // The interrupt's *after* juncture (docs/control-channel.md).
            if shared.take_interrupt() {
                return Ok(Decision::Withheld(withheld(call, result)));
            }
            Ok(Decision::Ran { result, handoff })
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
