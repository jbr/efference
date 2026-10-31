//! The permission cascade for one call (docs/permissions.md): the fast path, the
//! spotter, and the human gate.

use super::*;
use crate::names::tools::BASH;
use crate::spotter::{Read, Spotting};

/// The permission cascade for one call (docs/permissions.md). `bound` is the leaf
/// action an allowing `agents__decide` authorizes, when the call is one: a granted
/// `agents__decide` clears blind — the config's statement of trust in the
/// coordinator — and otherwise the leaf, not the handle, is what the fast-path, the
/// spotter, and the human gate judge. Returns the decision; the caller commits
/// any `ConfigEdit` it carries.
pub(in crate::agent) async fn judge<F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Session>,
    spotter: &S,
    toolbox: &Toolbox,
    turn: Turn,
    call: &ToolCall,
    bound: Option<&crate::protocol::agents::BoundAction>,
) -> Result<Approval, Error> {
    // Classify against the *effective* policy — base role plus the fold of every
    // grant so far: a prior amendment can promote this very call into the
    // fast-path, and the spotter sees the same scope.
    let role = shared.effective_config();
    // A granted `agents__decide` clears blind (docs/permissions.md): the doc
    // states that as trust in the coordinator, so the child's leaf is not
    // re-litigated on this path.
    if bound.is_some() && policy::tool_granted(&role, &call.name) {
        return Ok(Approval::Approve);
    }
    // A successor archiving its own predecessor is the hand-off's completion
    // (docs/permissions.md): the host wrote the link at `hand_off`, so the call clears on
    // the fast path, before the spotter or the human is asked. A denial or an escalation
    // at a child's gate authorizes nothing, and clears the same way.
    if bound.is_none() && (toolbox.completes_hand_off(call) || toolbox.authorizes_nothing(call)) {
        return Ok(Approval::Approve);
    }
    // The judged call: the child's leaf when bound, else the parent's own call.
    // The id stays the parent's, so the `Spot` audit entry and the gate correlate
    // to the call actually held. The leaf's own directories classify it; the parent's are used otherwise.
    let leaf;
    let (judged, dirs): (&ToolCall, policy::Directories) = match bound {
        Some(bound) => {
            leaf = ToolCall::new(bound.name.clone(), call.id.clone(), bound.input.clone());
            (&leaf, bound.directories())
        }
        None => (call, shared.directories()),
    };
    match policy::classify(&dirs, &role, judged) {
        Verdict::Clear => Ok(Approval::Approve),
        // A `[commands]` deny (docs/permissions.md): the operator already wrote the answer
        // for this prefix, so the spotter is not asked — the move goes straight to whoever
        // gates. Autonomous has no synchronous human, so it takes the same redirect a
        // spotter `Gate` takes there, carrying the reason the config gave it.
        Verdict::Deny => {
            // No grant is offered: the operator's deny is the answer, which an offer
            // would reverse (docs/permissions.md).
            if shared.autonomous() {
                Ok(shared.review_redirect(&call.id, denied_gate_message(shared.config_path())))
            } else {
                Ok(frontend
                    .approve(call, GateOptions::default().with_bound(bound))
                    .await)
            }
        }
        // HumanOnly (docs/permissions.md): the human takes the spotter's role
        // *entirely* — skip the spotter (pay no tokens, log no `Spot`) and gate
        // straight to the human, who both decides this call and grows the allowlist by
        // committing the grant the gate offers.
        Verdict::Gate if shared.review_level() == ReviewLevel::HumanOnly => {
            let grant = gate_grant(shared, &role, call, bound);
            Ok(frontend
                .approve(
                    call,
                    GateOptions::default().with_bound(bound).with_grant(grant),
                )
                .await)
        }
        Verdict::Gate => {
            let probe = promotion_probe(shared, &role, judged, bound);
            let spotting = consult_spotter(
                shared,
                spotter,
                toolbox,
                &role,
                judged,
                bound,
                probe.as_deref(),
            )
            .await;
            let Read { stance, reason } =
                record_spot(shared, turn, call, &role, spotting, probe.as_deref())?;
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
                        Ok(shared.review_redirect(
                            &call.id,
                            match policy::missing_grants(&dirs, &role, judged) {
                                Some(missing) => {
                                    missing_grant_message(shared.config_path(), &reason, &missing)
                                }
                                None => autonomous_gate_message(shared.config_path(), &reason),
                            },
                        ))
                    } else {
                        // Asked after the `Spot` and its vote are recorded, so the probe
                        // this very call carried is evidence for the grant it offers.
                        let grant = gate_grant(shared, &role, call, bound);
                        Ok(frontend
                            .approve(
                                call,
                                GateOptions::default().with_bound(bound).with_grant(grant),
                            )
                            .await)
                    }
                }
            }
        }
    }
}

/// Prefix-promotion probe (docs/permissions.md): on a bash move, observe
/// its recurrence and pick the shortest prefix worth asking the spotter to
/// vouch for *for all arguments*. The spotter answers via the stance
/// (`clear_always`), so the probe rides the one spot call. A bound leaf's
/// command belongs to the child's own ledger, so no probe rides an approval.
fn promotion_probe(
    shared: &Session,
    role: &crate::config::Config,
    judged: &ToolCall,
    bound: Option<&crate::protocol::agents::BoundAction>,
) -> Option<String> {
    if bound.is_none() && judged.name == BASH {
        judged
            .input
            .get("command")
            .and_then(Value::as_str)
            .and_then(|command| policy::bash_tokens(&shared.directories(), role, command))
            .and_then(|tokens| shared.observe_and_pick_probe(role, &tokens))
    } else {
        None
    }
}

/// Ask the spotter about `judged`, with the context its rung weighs.
async fn consult_spotter<S: Spotter>(
    shared: &Session,
    spotter: &S,
    toolbox: &Toolbox,
    role: &crate::config::Config,
    judged: &ToolCall,
    bound: Option<&crate::protocol::agents::BoundAction>,
    probe: Option<&str>,
) -> Spotting {
    // What the turn's whole `tools[]` knows about this tool — providers
    // plus the loop-owned family (docs/permissions.md). `None` for bash,
    // whose command text is its own description. Built from the leaf's name
    // when bound, so the spotter reads the child's tool.
    let brief = brief(toolbox, &judged.name);
    let chain: &[String] = bound.map_or(&[], |b| b.chain.as_slice());
    // A bound leaf's history is the child's, not this log's, so it has no
    // precedents here (docs/permissions.md).
    let precedents = if bound.is_none() {
        shared.consented_calls()
    } else {
        Vec::new()
    };
    spotter
        .assess(role, judged, brief.as_ref(), chain, probe, &precedents)
        .await
}

/// Record what the spotter answered: its request's usage and the `Spot` entry, then the
/// vote the probe casts. Returns the read the move is decided on.
fn record_spot(
    shared: &Session,
    turn: Turn,
    call: &ToolCall,
    role: &crate::config::Config,
    spotting: Spotting,
    probe: Option<&str>,
) -> std::io::Result<Read> {
    // What actually answered, read off the response rather than the
    // configured id (docs/permissions.md). Taken before the usage moves
    // into the ledger; `None` when no request happened at all.
    let model = spotting.usage.as_ref().and_then(|u| u.model.clone());
    if let Some(u) = spotting.usage {
        shared.record_request(RequestOrigin::Spot, u);
    }
    let Read { stance, reason } = spotting.read;
    let origin = spotting.origin;
    shared.append(
        turn,
        Payload::Spot(Spot {
            call_id: call.id.clone(),
            stance,
            reason: reason.clone(),
            origin,
            model,
            probed_prefix: probe.map(str::to_string),
            axes: spotting.axes,
        }),
    )?;
    // The probe's answer *is* the stance: `clear_always` is the safe-vote for
    // the probed prefix, any other stance is not-safe (a fail-closed fallback
    // didn't judge, so it casts no vote). When the safe-votes cross the
    // consensus threshold, file a non-blocking `Proposed(Command)` — inert
    // until an orchestrator-above commits it. Nothing rides along: the proposal
    // can't clear anything until committed, and this very call is still decided
    // by the caller.
    if let Some(prefix) = probe
        && origin == Origin::Model
        && shared.record_probe_vote(prefix, stance == Stance::ClearAlways)
        && let Some(command) = policy::promotable_command(role, prefix)
    {
        shared.append(
            turn,
            Payload::ConfigEdit(
                ConfigEdit::new(EditState::Proposed, ConfigChange::Command { command }).with_note(
                    format!(
                        "spotter: `{prefix}` judged safe for all arguments — \
                         proposed for the allowlist"
                    ),
                ),
            ),
        )?;
    }
    Ok(Read { stance, reason })
}

/// The grant a gate on `call` offers (docs/permissions.md). A bound gate offers none:
/// what a supervisor's grant should cover is not settled, and the leaf's policy is the
/// child's, not this session's.
fn gate_grant(
    shared: &Session,
    role: &crate::config::Config,
    call: &ToolCall,
    bound: Option<&crate::protocol::agents::BoundAction>,
) -> Option<policy::GateGrant> {
    bound
        .is_none()
        .then(|| shared.gate_grant(role, call))
        .flatten()
}
