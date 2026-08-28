//! Committing a config edit the session wrote, and the autonomous-mode gate message (docs/permissions.md).

use super::*;

/// The redirect a gated call gets in **autonomous mode**
/// (docs/permissions.md). No synchronous human is
/// present to approve, so rather than block the loop forever the gate hands the decision back to
/// the model — pointing it at its own per-session config file, the single policy interface it has.
/// The session may *edit* that file to request the permission, but that write itself gates for the
/// orchestrator-above to review over the control channel (it can never auto-apply), so enactment
/// still requires an operator: the model only gets to *ask*. The spotter's `reason` carries why the
/// call was out of scope so the model can judge whether it's worth requesting at all.
pub(super) fn autonomous_gate_message(config_path: Option<&Path>, reason: &str) -> String {
    let request = config_request_sentence(config_path);
    format!(
        "Gated, and no synchronous reviewer is available (autonomous mode), so this call did not \
         run.\n\nSpotter: {reason}\n\n{request} Otherwise, find another approach within your \
         current scope."
    )
}

/// The redirect a **denied** call gets in autonomous mode (docs/permissions.md). Same shape as
/// [`autonomous_gate_message`] and the same request paragraph, but it names the config as the
/// author of the refusal rather than the spotter — a `[commands]` key carrying `deny` decided
/// this without a spotter call, so quoting a verdict nobody gave would misdescribe the gate.
pub(super) fn denied_gate_message(config_path: Option<&Path>) -> String {
    let request = config_request_sentence(config_path);
    format!(
        "Denied by a `[commands]` entry in this session's policy, and no synchronous reviewer is \
         available (autonomous mode), so this call did not run.\n\n{request} Otherwise, find \
         another approach within your current scope."
    )
}

/// The shared "you may ask for this" paragraph both redirects end with — the session's one
/// policy interface, and the reminder that editing it is a request, not an enactment.
fn config_request_sentence(config_path: Option<&Path>) -> String {
    match config_path {
        Some(p) => format!(
            "If it's essential to your task, edit your config at `{}` to request the access you \
             need — a `[commands]`/`[paths]`/`tools` entry, or a wider `scope`. That edit is held \
             for the orchestrator to review over the control channel; it won't auto-apply.",
            p.display()
        ),
        None => {
            "If it's essential to your task, request the access you need from the orchestrator."
                .to_string()
        }
    }
}

/// Decide what an approved config write actually *did*
/// (docs/permissions.md). Re-reads the file through the same refresh the
/// fs-watch task uses, so the edit lands in exactly one of two states:
///
/// - **adopted** — it parsed; its deltas are already appended as committed `ConfigEdit`s by the
///   refresh, so the new policy is in force for the very next call. The tool result stands.
/// - **reverted** — it didn't parse; the file has been restored to the last valid config and the
///   result becomes an error naming the parse failure.
///
/// The hidden third outcome — approved, written, and inert — is what this deletes. Note the
/// rejection is phrased as a *policy* outcome, not a tool malfunction: a model that reads "the
/// write failed" invents theories about the tool, where a model that reads "your config didn't
/// parse, here's the error" fixes the TOML.
pub(super) fn config_write_outcome(shared: &Shared, result: RawResult) -> RawResult {
    match crate::daemon::refresh_and_emit(shared) {
        Some(Refreshed::Reverted { error }) => RawResult {
            images: Vec::new(),
            content: format!(
                "Your config edit was not adopted: the file did not parse, so it has been restored \
                 to the last valid config and your effective permissions are unchanged. Nothing \
                 else about the write failed — the tool wrote the bytes you gave it, they just \
                 aren't a valid config. Re-read the file (its header comment carries the schema) \
                 and write a version that parses.\n\nParse error: {error}"
            ),
            is_error: true,
        },
        Some(Refreshed::RevertFailed { error, io }) => RawResult {
            images: Vec::new(),
            content: format!(
                "Your config edit was not adopted: the file did not parse, and restoring the last \
                 valid config also failed, so the file on disk is currently invalid. Your \
                 effective permissions are unchanged. This needs a human to repair the \
                 file.\n\nParse error: {error}\nRestore error: {io}"
            ),
            is_error: true,
        },
        // Adopted, or no config file at all — the write stands as reported.
        Some(Refreshed::Adopted(_)) | None => result,
    }
}

/// Legislate a `ConfigEdit` (docs/permissions.md): persist it if asked, append the
/// `Payload::ConfigEdit` (which folds it into the effective policy), then re-classify the gated call
/// against the *new* policy. Returns `Approve` iff the amended rule now clears the call — the
/// human changes the rule; the mechanism, not the gesture, authorizes the run. If the grant
/// doesn't cover this call, it stays gated (a `Redirect` that tells the model why), so nothing
/// rides along unchecked.
///
/// **The file gets the grant first, and a failed write fails the grant.** The session's
/// `config.toml` is the perimeter's readable form — what an operator inspects to see what the
/// session ran under, and what they copy a useful grant up to the origin template from
/// (docs/permissions.md). A grant that folded into
/// effective policy but never reached the file would be policy in force that the file doesn't
/// show, the divergence [`crate::config::ConfigFile`] exists to prevent. So the write happens
/// before the append, and its failure aborts the commit rather than degrading it to something
/// weaker that still logs a success.
///
/// A session with no config file (the built-in default role) is the one exception: there is
/// nothing to diverge from, so the grant lives in the log fold alone.
pub(super) fn commit_config_edit(
    shared: &Shared,
    turn: Turn,
    call: &ToolCall,
    change: ConfigChange,
    note: String,
) -> std::io::Result<Approval> {
    if let Err(e) = shared.apply_config_change(&change) {
        // Nothing is appended, so effective policy is untouched — the gate stands, and the reason
        // names the file rather than implying the rule itself was refused.
        eprintln!("config edit: could not write to the config file: {e}");
        return Ok(Approval::Redirect(format!(
            "That grant could not be written to the session's config file, so it was not applied \
             and this call is still gated. The config file needs attention before policy can \
             change. Error: {e}"
        )));
    }
    // The human upgrading a gate is a *direct* commit — the gesture is the commit, no prior
    // proposal to resolve (docs/permissions.md).
    shared.append(
        turn,
        Payload::ConfigEdit(ConfigEdit {
            state: EditState::Committed,
            resolves: None,
            change,
            note,
        }),
    )?;
    let role = shared.effective_config();
    Ok(if policy::classify(&role, call) == Verdict::Clear {
        Approval::Approve
    } else {
        Approval::Redirect(
            "Scope updated — but that grant doesn't cover this exact call, so it stays gated. \
             Adjust the call to fit the new scope and retry."
                .into(),
        )
    })
}
