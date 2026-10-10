//! Running the calls of one completion (docs/README.md): each goes through the permission
//! cascade and the tool run in call order, and the outcomes are held for the gate.

use super::*;

/// What running a completion's calls left behind: the outcomes in call order, and whether
/// the turn was aborted or redirected along the way.
pub(super) struct Batch {
    pub steps: Vec<Step>,
    pub abort_turn: bool,
    pub redirect: Option<String>,
    /// What the triage-lineage primer did while the tools ran
    /// (backlog/separate-triage-cold-prefix.md), when one was armed.
    pub primer: Option<TriagePrimerOutcome>,
    /// When the last tool resolved, for the primer's serial residue.
    pub tools_end: std::time::Instant,
}

/// Run every call of a completion, capturing each result as it exists (docs/redaction-fork.md)
/// so one gate can triage them together. Once a call aborts or redirects, the rest are paired
/// with stand-ins rather than run.
///
/// When `primer` is armed, its send is fired at the first execution start and runs
/// against the batch's tools: the peek's warmup hides behind tool runtime instead of
/// landing on the gate path (backlog/separate-triage-cold-prefix.md). The sender rides
/// inside the batch future, so every exit — including an early `?` — drops it and lets
/// an unfired primer resolve to nothing sent.
pub(super) async fn run_batch<'a, F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Session>,
    spotter: &S,
    toolbox: &Toolbox,
    turn: Turn,
    calls: &[ToolCall],
    primer: Option<TriagePrimerInput<'a>>,
) -> Result<Batch, Error> {
    let (primer_go, mut primer_rx) = tokio::sync::watch::channel(false);
    let primer_fut = async {
        if primer_rx.wait_for(|fired| *fired).await.is_err() {
            return None;
        }
        let input = primer.as_ref()?;
        // Stamped before the send: a fold that lands during the flight moves the prefix
        // the primer read, and the triage keepalive must not replay it (docs/transport.md).
        let sent = crate::log::Timestamp::now();
        let outcome = input.distiller.prime(input.turns).await?;
        // A primer that planted (it has usage) is what the triage keepalive re-sends.
        if let Some(usage) = &outcome.usage {
            shared.set_triage_primed(input.turns, sent, usage);
        }
        Some(outcome)
    };
    let batch_fut = async move {
        run_calls(
            frontend,
            shared,
            spotter,
            toolbox,
            turn,
            calls,
            Some(&primer_go),
        )
        .await
    };
    let (batch, primer) = tokio::join!(batch_fut, primer_fut);
    let mut batch = batch?;
    if let Some(mut outcome) = primer {
        outcome.residue_ms = outcome
            .done_at
            .saturating_duration_since(batch.tools_end)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        batch.primer = Some(outcome);
    }
    Ok(batch)
}

/// The calls loop of [`run_batch`], factored so the batch future can own the primer's
/// go-signal: dropping it on any exit resolves an unfired primer to nothing sent.
async fn run_calls<F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Session>,
    spotter: &S,
    toolbox: &Toolbox,
    turn: Turn,
    calls: &[ToolCall],
    primer_go: Option<&tokio::sync::watch::Sender<bool>>,
) -> Result<Batch, Error> {
    let mut abort_turn = false;
    let mut redirect: Option<String> = None;
    let mut steps: Vec<Step> = Vec::new();
    // Every result captured so far, so a call that fails leaves none of them waiting
    // for a verdict that will never come (`undecided` below).
    let mut captured: Vec<crate::log::Seq> = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        // Once we've decided to stop running calls (abort or redirect),
        // every remaining `tool_use` still needs a paired result or the
        // wire orphans it and the API rejects the request (docs/README.md).
        if abort_turn || redirect.is_some() {
            steps.push(Step::direct(shared, turn, aborted(call))?);
            continue;
        }
        // The interrupt's *before* juncture (docs/control-channel.md): the operator
        // asked out while the model was generating, so this call never runs — the
        // un-run stand-in already says so, and the turn ends here.
        if shared.take_interrupt() {
            steps.push(Step::direct(shared, turn, aborted(call))?);
            abort_turn = true;
            continue;
        }
        // Path env-var expansion (docs/tools.md): resolve `$TMPDIR` in the
        // call's path fields at one choke point, before anything judges or
        // runs the call — the fast-path, the spotter, and the executor all
        // see the same expanded literal (an unexpanded variable is opaque
        // to the spotter, so this is what makes the path judgeable). A
        // failed expansion (unlisted/undefined name) pairs the call with an
        // error result and never reaches the cascade — a hard stop, not
        // expand-to-empty. The logged `tool_use` keeps the variable form
        // the model wrote; only the in-flight call is rewritten.
        let expanded;
        let running = match shared.expand_call(call) {
            Ok(None) => call,
            Ok(Some(rewritten)) => {
                expanded = rewritten;
                &expanded
            }
            Err(message) => {
                steps.push(Step::direct(shared, turn, expansion_error(call, &message))?);
                continue;
            }
        };
        let at = AskPosition::of_call(calls, index);
        let decision = match run_call(
            frontend, shared, spotter, toolbox, turn, running, at, primer_go,
        )
        .await
        {
            Ok(decision) => decision,
            // A call that could not finish ends the batch, so nothing triages what
            // earlier calls already captured. Resolve those here rather than leaving
            // them pending: the turn is over either way, and a result with no verdict
            // projects to nothing, which orphans its `tool_use` on the next request
            // (docs/README.md).
            Err(error) => {
                undecided(shared, turn, &captured)?;
                return Err(error);
            }
        };
        match decision {
            Decision::Ran {
                result,
                telemetry,
                handoff,
                reply,
            } => {
                // The capture (docs/redaction-fork.md): the result is durable before
                // anything judges it, as `Pending` — on the wire it is nothing yet, so
                // no request built before the gate's verdict can carry a partial batch.
                let payload = Payload::tool_result(call.id.clone(), result, WireState::Pending);
                // An `ask`'s result is a submitted turn's words, so its entry names the
                // submission and the submitter like a `User` entry does
                // (docs/supervisor.md).
                let result_seq = match reply {
                    Some(reply) => shared.append_submission(
                        turn,
                        payload,
                        reply.submission_id,
                        reply.source,
                    )?,
                    None => shared.append(turn, payload)?,
                };
                captured.push(result_seq);
                // Triage reads what the log holds, which is the masked form of the
                // output (docs/env.md) — the same bytes any later reader of the entry
                // sees, and the ones a verdict is rendered against. Reading the entry
                // back rather than carrying `result` forward is what makes that hold.
                let Some(Payload::ToolResult { raw, .. }) =
                    shared.entry_at(result_seq).map(|entry| entry.payload)
                else {
                    log::error!("the result captured at seq {result_seq} is not in the log");
                    return Err(Box::new(std::io::Error::other(
                        "the result captured at this seq is missing from the log",
                    )));
                };
                // What the call cost and how it came out (docs/tools.md), beside the
                // result it measures. It projects to nothing, so appending it here
                // rather than after the verdict cannot move the wire, and no reader
                // reads it as a verdict: it is a measurement of the call, not a
                // decision about its output.
                shared.append(turn, Payload::ToolTelemetry(*telemetry))?;
                steps.push(Step::Ran {
                    call: call.clone(),
                    result_seq,
                    result: raw,
                    handoff,
                });
            }
            Decision::Direct(payloads) => {
                for payload in payloads {
                    shared.append(turn, payload)?;
                }
                steps.push(Step::Direct);
            }
            Decision::Withheld(payload) => {
                // The interrupt's *after* juncture (docs/control-channel.md). The
                // call finished but its output stops at the log — taken before the
                // gate, so the discarded result is never peeked at and costs no
                // round-trip, which is the point of the verb. Its verdict is already
                // final, so the entry goes down as it stands.
                steps.push(Step::direct(shared, turn, payload)?);
                abort_turn = true;
            }
            Decision::Abort(payload) => {
                steps.push(Step::direct(shared, turn, payload)?);
                abort_turn = true;
            }
            Decision::Redirect { payload, message } => {
                steps.push(Step::direct(shared, turn, payload)?);
                redirect = Some(message);
            }
        }
    }
    Ok(Batch {
        steps,
        abort_turn,
        redirect,
        primer: None,
        tools_end: std::time::Instant::now(),
    })
}

/// Resolve results that were captured but that no gate will ever see: the batch ended
/// in an error, so the turn ends with it and there is no verdict to wait for.
///
/// Withheld rather than dropped, because a result with no verdict projects to nothing
/// and so orphans its `tool_use` on the next request (docs/README.md) — and the log
/// keeps the bytes either way, which is the whole of what capture changed about them.
/// `Elided` says what every other undecided path says (the interrupt's *after*
/// juncture, an interrupted gate): the output is not on the wire, and nothing about
/// triage or the tool failed.
fn undecided(shared: &Session, turn: Turn, captured: &[crate::log::Seq]) -> std::io::Result<()> {
    for seq in captured {
        shared.append(
            turn,
            Payload::ResultVerdict {
                for_result: *seq,
                wire: WireState::Elided,
            },
        )?;
    }
    Ok(())
}
