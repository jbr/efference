//! Bringing back a resumed session's pending batch (docs/resumption.md): the calls of the
//! last completion that never ran go through the permission cascade now, and the results
//! captured before the restart join them at the gate.

use super::*;
use crate::record::recovery::{CallState, Completion};

/// Run what `pending` left undone and return the batch in call order. A call that never
/// started runs as it would have in the first place, filed under the completion's turn,
/// so a gate reappears and is judged against current policy. A captured result keeps the
/// entry it was logged at and only waits for the verdict.
pub(super) async fn replay_batch<F: Frontend, S: Spotter>(
    frontend: &mut F,
    shared: &Arc<Session>,
    spotter: &S,
    toolbox: &Toolbox,
    pending: &Completion,
) -> Result<Batch, Error> {
    let never: Vec<ToolCall> = pending
        .calls
        .iter()
        .filter(|(_, state)| *state == CallState::NeverStarted)
        .map(|(call, _)| call.clone())
        .collect();
    let Batch {
        steps: ran,
        abort_turn,
        redirect,
        primer,
        tools_end,
    } = run_batch(
        frontend,
        shared,
        spotter,
        toolbox,
        pending.turn,
        &never,
        None,
    )
    .await?;
    let mut ran = ran.into_iter();
    let mut steps = Vec::new();
    for (call, state) in &pending.calls {
        match state {
            CallState::Decided | CallState::Started => {}
            CallState::NeverStarted => steps.extend(ran.next()),
            CallState::Captured { result_seq } => {
                let Some(Payload::ToolResult { raw, .. }) =
                    shared.entry_at(*result_seq).map(|entry| entry.payload)
                else {
                    log::error!("the result captured at seq {result_seq} is not in the log");
                    return Err(Box::new(std::io::Error::other(
                        "a result captured before the restart is missing from the log",
                    )));
                };
                steps.push(Step::Ran {
                    call: call.clone(),
                    result_seq: *result_seq,
                    result: raw,
                    handoff: None,
                });
            }
        }
    }
    Ok(Batch {
        steps,
        abort_turn,
        redirect,
        primer,
        tools_end,
    })
}
