//! Running an approved call (docs/tools.md): the tool run, its process-monitor
//! bookkeeping, the result cap, and the interrupt's *after* juncture
//! (docs/control-channel.md). Reached from [`run_call`] once the permission cascade
//! has said yes.

use super::*;
use crate::log::Telemetry;

/// Run `call` and capture what it produced as a [`Decision`]. `config_write` marks the
/// call that writes the session's own config file, whose effect is decided after it
/// runs (docs/permissions.md).
pub(super) async fn run_approved<F: Frontend>(
    frontend: &mut F,
    shared: &Arc<Session>,
    toolbox: &Toolbox,
    turn: Turn,
    call: &ToolCall,
    config_write: bool,
    primer_go: Option<&tokio::sync::watch::Sender<bool>>,
) -> Result<Decision, Error> {
    // From here the call may touch the world, so the log says so before it
    // does (docs/log-and-projection.md): a call with this marker and no
    // result is the one a resumed session drops rather than reruns.
    shared.append(
        turn,
        Payload::Telemetry(Telemetry::call_started(call.id.clone())),
    )?;
    // Approved, so a result triage handles will follow short of an
    // interrupt — fire the primer over the execution window
    // (docs/transport.md).
    if let Some(go) = primer_go {
        let _ = go.send(true);
    };
    // Acquire the result. A backgrounded bash spawn returns immediately — the child
    // runs on under the process monitor and its output surfaces at later triages
    // (docs/process-monitor.md); everything else runs to completion through the toolbox.
    // `interrupted()` resolves on a `POST /loop/interrupt` — the tool's kill trigger
    // (kills the process group). Tool output streams to the control stream's `output` class
    // chunk-by-chunk as it runs (bash; the MCP tools resolve whole), tagged by call id.
    let mut handoff = None;
    // The `p#<n>` the process monitor minted for this call, if it reached a
    // spawn — recorded on the telemetry row so a reader can join the
    // call to the process namespace (docs/process-monitor.md).
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
            // transcript entry can each find the other (docs/process-monitor.md).
            Some(command) => {
                match shared.spawn_background(command, Some(call.id.0.clone())) {
                    Ok(handle) => {
                        // The async pattern is stated here, where the shape gets
                        // chosen — the watch_process result says it too, but that
                        // is read only after a compressed start→watch→kill turn
                        // has already been attempted, and a short process can exit
                        // before the turn composing follow-up calls reaches it
                        // (docs/process-monitor.md).
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
    } else if let Some(r) = process_monitor_tool_result(shared, call).await {
        // A process-monitor/inbox read/act tool (docs/process-monitor.md) — resolves against
        // daemon state; `wait` yields on the inbox signal while the other tools return.
        r
    } else {
        if call.name == bash::NAME {
            // A detach request that never found a running tool must not label this
            // fresh command's (potential) handoff — drop any stale note first.
            shared.take_detach_note();
        }
        // A foreground bash command joins the process namespace the moment it
        // spawns (docs/process-monitor.md): the spawn hook mints its id, and the
        // chunk sink feeds the process monitor's capture alongside the live stream —
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
                    *fg_id.lock().unwrap() =
                        Some(shared.register_foreground(command, pgid, Some(spawning_call)));
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
                    shared.notify(notification);
                },
            )
            .await;
        match outcome {
            bash::Outcome::Done(result) => {
                // The paired `tool_result` is the canonical output now; the
                // registry entry has done its job. Its handle stays on the
                // telemetry row, which is where a foreground process's
                // ordinal survives the removal (docs/process-monitor.md).
                if let Some(id) = fg_id.lock().unwrap().take() {
                    process_handle = Some(process_monitor::handle(id));
                    shared.conclude_foreground(id);
                }
                result
            }
            // The foreground→background handoff (docs/process-monitor.md): the operator
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
                // failure was silence (docs/process-monitor.md).
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
    // triage, the wire, and the logged `raw`. An over-cap result becomes a hard error
    // so the model narrows the call.
    let result = cap_tool_result(shared, call, result);
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
        let interruption = shared.interruption_after(elapsed, &result);
        return Ok(Decision::Withheld(withheld(call, result, interruption)));
    }
    Ok(Decision::Ran {
        result,
        telemetry,
        handoff,
        reply: None,
    })
}
