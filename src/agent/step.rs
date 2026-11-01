//! Running one call of a completion: the permission cascade (docs/permissions.md)
//! and the tool run, up to but not including triage.
//!
//! A completion may make several calls (docs/README.md), so the loop runs each call
//! here and triages the whole batch at one triage (docs/triage.md). Splitting execution
//! from triage is what keeps the fork's request valid: a triage fired between calls would
//! carry unanswered `tool_use` blocks and be rejected.
//!
//! This file holds the outcome types and [`run_call`], the order in which the cascade's
//! answers are applied; `judge` is the cascade itself, `execute` the tool run behind an
//! approval, and `consolidate_call` the one call answered from the log alone.

use super::*;
use crate::record::log::ToolTelemetry;
use crate::record::names::tools::{ASK, CONSOLIDATE, is_triage_tool};

mod consolidate_call;
mod execute;
mod judge;

use execute::run_approved;
pub(super) use judge::judge;

/// What running one call produced, before triage.
pub(super) enum Decision {
    /// A real result to triage at the triage.
    Ran {
        result: RawResult,
        /// What the call cost and how it came out (docs/tools.md) — measured
        /// here, appended by the caller beside the result it describes. Boxed
        /// because it is carried through two enums whose other variants are
        /// small, and an unboxed field would size every one of them to it.
        telemetry: Box<ToolTelemetry>,
        /// The operator's note from a foreground→background handoff, delivered as a
        /// user turn after the step's results (docs/process-monitor.md).
        handoff: Option<String>,
        /// Who answered, when the call was an `ask` (docs/process-monitor.md).
        reply: Option<Box<Reply>>,
    },
    /// Payloads to append as they stand — they never reach the triage, because there is
    /// no world output to curate (`consolidate`) or the call never ran. The caller
    /// appends them, so each is durable before the next call runs.
    Direct(Vec<Payload>),
    /// The operator interrupted while this call ran (docs/control-channel.md): it
    /// finished, but its output stops at the log. Taken *before* the triage, so the
    /// discarded result is never triaged at and costs no round-trip.
    Withheld(Payload),
    /// The human aborted at the gate; the turn ends.
    Abort(Payload),
    /// The decider replied instead of approving; their message becomes the next user
    /// turn, under `source`: the decider's, unless that is the operator or the loop's own
    /// permission review, whose turns open with no label (docs/permissions.md).
    Redirect {
        payload: Payload,
        message: String,
        source: Option<String>,
    },
}

/// One step of the trajectory, in call order: a call's outcome once it is on the log.
/// A captured result is already durable and waits only for the verdict that names it,
/// so what a step carries is that verdict's address rather than its output.
pub(super) enum Step {
    /// Nothing left to land: the step's payloads are on the log already, and there is
    /// no world output for a triage to curate.
    Direct,
    Ran {
        call: ToolCall,
        /// The seq the result was captured at, which the verdict it is still waiting
        /// for names (docs/triage.md).
        result_seq: crate::record::log::Seq,
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

/// `ask` puts a question to the operator (docs/process-monitor.md): there is nothing in the
/// world to authorize, so it skips the cascade, and the wait for the reply is the
/// call. The interrupt's *after* juncture applies as for any call.
async fn run_ask(
    shared: &Arc<Session>,
    call: &ToolCall,
    ask_at: AskPosition,
    primer_go: Option<&tokio::sync::watch::Sender<bool>>,
) -> Decision {
    // An answered ask lands in the triage, so its triage is certain short of an
    // interrupt — fire the primer over the operator's deliberation window
    // (docs/transport.md).
    if let Some(go) = primer_go {
        let _ = go.send(true);
    }
    let started = std::time::Instant::now();
    let (result, reply) = ask_result(shared, call, ask_at).await;
    let result = cap_tool_result(shared, call, result);
    let telemetry = Box::new(telemetry::observe(
        call,
        &result,
        started.elapsed(),
        result.is_error,
        None,
    ));
    if shared.take_interrupt() {
        let interruption = shared.interruption_after(started.elapsed(), &result);
        return Decision::Withheld(withheld(call, result, interruption));
    }
    Decision::Ran {
        result,
        telemetry,
        handoff: None,
        reply,
    }
}

/// Take one call through the permission cascade and, if approved, run it
/// (docs/permissions.md, docs/tools.md). Appends only the audit entries the cascade
/// itself owns — the `Spot` verdict and any `ConfigEdit` — never the result, which the
/// caller captures the moment this returns (docs/triage.md).
// One production caller, `run_calls`, which holds the loop's collaborators and the call's
// inputs as separate borrows and passes each once; none is optional but the primer's signal.
#[allow(clippy::too_many_arguments)]
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
    // An un-asked triage tool (docs/triage.md) never enters the cascade — there
    // is nothing to judge or run; the paired nudge result tells the model to carry on.
    if is_triage_tool(&call.name) {
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
    // verbatim: a `Direct` payload never enters the triage, so no summary stands between
    // the correction and the model.
    let callable = callable_names(toolbox);
    if !callable.contains(&call.name) {
        return Ok(Decision::Direct(vec![unknown_tool(call, &callable)]));
    }
    if call.name == ASK {
        return Ok(run_ask(shared, call, ask_at, primer_go).await);
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
    // An allowing `agents__decide` authorizes the target's held move, so the parent's
    // permission cascade judges that move rather than the handle (docs/permissions.md).
    // Resolve it first: a moved gate or a refused target is the whole answer, and
    // nothing should be classified. The logged `ToolCall` is untouched — the wire
    // carries what the model wrote. A denial authorizes nothing and is not bound.
    let bound = if crate::protocol::agents::allows(call) {
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
        .is_some_and(|cfg| policy::is_config_write(&shared.directories(), call, cfg));
    // A hand-off gives this session's place to a new one (docs/tools.md): the operator's
    // decision, like a policy edit, so it takes the same route past the cascade.
    let hand_off = call.name == crate::protocol::agents::HAND_OFF_WIRE_NAME;
    // Neither offers a grant: no change to the policy table would clear either, since
    // both bypass the cascade that reads it.
    let approval = if config_write || hand_off {
        frontend
            .approve(call, GateOptions::default().with_bound(bound.as_ref()))
            .await
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
        // summary + pair the `tool_use`. No world-run, no triage — there is no tool
        // output to curate, so it never reaches the triage.
        Approval::Approve if call.name == CONSOLIDATE => Ok(consolidate_call::record(shared, call)),
        Approval::Approve => {
            run_approved(
                frontend,
                shared,
                toolbox,
                turn,
                call,
                config_write,
                primer_go,
            )
            .await
        }
        Approval::Abort => Ok(Decision::Abort(aborted(call))),
        // Reject this call but keep the turn alive: the decider's message becomes the next
        // user turn so the model can course-correct instead of bailing (docs/README.md).
        // Who decided is on the log already, as the call's gate decision.
        Approval::Redirect(message) => {
            let decider = shared.gate_decider(&call.id);
            Ok(Decision::Redirect {
                payload: rejected(call, decider.as_deref()),
                message,
                source: decider
                    .filter(|source| !crate::record::names::sources::is_review_source(source)),
            })
        }
        // Resolved into Approve/Redirect by `commit_config_edit` above before this match.
        Approval::ConfigEdit { .. } => unreachable!("grant is legislated above"),
    }
}
