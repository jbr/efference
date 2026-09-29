//! Running the calls of one completion (docs/README.md): each goes through the permission
//! cascade and the tool run in call order, and the outcomes are held for the gate.

use super::*;

/// What running a completion's calls left behind: the outcomes in call order, and whether
/// the turn was aborted or redirected along the way.
pub(super) struct Batch {
    pub steps: Vec<Step>,
    pub abort_turn: bool,
    pub redirect: Option<String>,
}

/// Run every call of a completion, holding the results so one gate can triage them
/// together. Once a call aborts or redirects, the rest are paired with stand-ins rather
/// than run.
pub(super) async fn run_batch<F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Session>,
    spotter: &S,
    toolbox: &Toolbox,
    turn: Turn,
    calls: &[ToolCall],
) -> Result<Batch, Error> {
    let mut abort_turn = false;
    let mut redirect: Option<String> = None;
    let mut steps: Vec<Step> = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        // Once we've decided to stop running calls (abort or redirect),
        // every remaining `tool_use` still needs a paired result or the
        // wire orphans it and the API rejects the request (docs/README.md).
        if abort_turn || redirect.is_some() {
            steps.push(Step::direct(aborted(call)));
            continue;
        }
        // The interrupt's *before* juncture (docs/control-channel.md): the operator
        // asked out while the model was generating, so this call never runs — the
        // un-run stand-in already says so, and the turn ends here.
        if shared.take_interrupt() {
            steps.push(Step::direct(aborted(call)));
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
                steps.push(Step::direct(expansion_error(call, &message)));
                continue;
            }
        };
        let at = AskPosition::of_call(calls, index);
        match run_call(frontend, shared, spotter, toolbox, turn, running, at).await? {
            Decision::Ran {
                result,
                telemetry,
                handoff,
                reply,
            } => steps.push(Step::Ran {
                call: call.clone(),
                result,
                telemetry,
                handoff,
                reply,
            }),
            Decision::Direct(payloads) => steps.push(Step::Direct(payloads)),
            Decision::Withheld(payload) => {
                // The interrupt's *after* juncture (docs/control-channel.md). The
                // call finished but its output stops at the log — taken before the
                // gate, so the discarded result is never peeked at and costs no
                // round-trip, which is the point of the verb.
                steps.push(Step::Direct(vec![payload]));
                abort_turn = true;
            }
            Decision::Abort(payload) => {
                steps.push(Step::Direct(vec![payload]));
                abort_turn = true;
            }
            Decision::Redirect { payload, message } => {
                steps.push(Step::Direct(vec![payload]));
                redirect = Some(message);
            }
        }
    }
    Ok(Batch {
        steps,
        abort_turn,
        redirect,
    })
}
