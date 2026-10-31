//! The permission cascade for one call (docs/permissions.md): the fast path, the
//! spotter, and the human gate.

use super::*;

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
                Ok(frontend.approve(call, bound, None).await)
            }
        }
        // HumanOnly (docs/permissions.md): the human takes the spotter's role
        // *entirely* — skip the spotter (pay no tokens, log no `Spot`) and gate
        // straight to the human, who both decides this call and grows the allowlist by
        // committing the grant the gate offers.
        Verdict::Gate if shared.review_level() == ReviewLevel::HumanOnly => {
            let grant = gate_grant(shared, &role, call, bound);
            Ok(frontend.approve(call, bound, grant).await)
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
                    .and_then(|command| policy::bash_tokens(&shared.directories(), &role, command))
                    .and_then(|tokens| shared.observe_and_pick_probe(&role, &tokens))
            } else {
                None
            };
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
            let spotting = spotter
                .assess(
                    &role,
                    judged,
                    brief.as_ref(),
                    chain,
                    probe.as_deref(),
                    &precedents,
                )
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
                        Ok(frontend.approve(call, bound, grant).await)
                    }
                }
            }
        }
    }
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
