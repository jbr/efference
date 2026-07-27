//! Approval-gate handling and the `.allow` staging flow (docs/permissions.md).

use super::*;

impl<'a> InteractiveState<'a> {
    pub(in crate::tui) fn submit_composer(&mut self, text: String) {
        if let Some(gate) = self.pending_gate.take() {
            let trimmed = text.trim();
            let decision = if trimmed.is_empty() {
                Approval::Approve
            } else if trimmed == ".abort" {
                Approval::Abort
            } else if trimmed == ".allow" || trimmed.starts_with(".allow ") {
                // Add a rule, then run the call iff it now clears (docs/permissions.md).
                // An explicit prefix (`.allow git log`) grants that bash command prefix;
                // bare `.allow` derives the grant from the gated call (the bash program,
                // or a structured tool's name). The mechanism re-checks — nothing rides
                // along unchecked. Written to the session's config file like any commit.
                let spec = trimmed.strip_prefix(".allow").unwrap_or("").trim();
                match Self::allow_change_impl(&gate, spec) {
                    Some(change) => Approval::ConfigEdit {
                        change,
                        note: "allowed at the gate".into(),
                    },
                    // Couldn't form a grant (e.g. bare `.allow` on a path-only tool) —
                    // fall back to steering the model rather than guessing a rule.
                    None => Approval::Redirect(trimmed.to_string()),
                }
            } else {
                Approval::Redirect(trimmed.to_string())
            };
            let label = match &decision {
                Approval::Approve => "approved".to_string(),
                Approval::Abort => "aborted".to_string(),
                Approval::Redirect(_) => "redirected".to_string(),
                // The composer doesn't author grants (that needs a richer surface); kept
                // exhaustive so a future grant-capable client labels cleanly.
                Approval::ConfigEdit { .. } => "granted".to_string(),
            };
            let _ = self.approve_tx.send((gate.call_id, decision));
            self.composer.clear();
            self.ui.debug_message = label.into();
            self.viewport.stuck_to_bottom = true;
            return;
        }

        if self.submit_tx.send(text.clone()).is_ok() {
            // Echo the message immediately as a pending transcript unit
            // (docs/tui.md): it's handed to the daemon but not yet a
            // committed `User` entry, so it renders dimmed until its log entry streams back
            // via `/events`. Reproject now so the echo appears on this keypress rather than
            // waiting for the next daemon event.
            self.pending_turns.push(text);
            self.composer.clear();
            self.ui.debug_message = "sent".into();
            // Follow the new turn as it streams in.
            self.viewport.stuck_to_bottom = true;
            self.mark_reproject();
        } else {
            self.ui.debug_message = "could not send (daemon gone?)".into();
        }
    }

    /// Handle a composer submission. When a gate is pending, the text resolves it
    /// with the same three-valued semantics as the `attach` REPL — empty approves,
    /// `.abort` aborts, anything else redirects the model. Otherwise it's a normal
    /// user turn (`POST /turn`).
    /// The `ConfigChange` a `.allow` at the gate proposes (docs/permissions.md). An
    /// explicit `spec` is taken as a bash command-prefix to allow; bare `.allow`
    /// derives it from the gated call — the program for a `bash` call, else the tool's
    /// own name. `None` when nothing sensible can be formed (the caller falls back to a
    /// redirect). Path/scope grants need the richer surface; this is the common case.
    fn allow_change_impl(gate: &PendingGate, spec: &str) -> Option<crate::log::ConfigChange> {
        use crate::log::ConfigChange;
        if !spec.is_empty() {
            return Some(ConfigChange::Command {
                command: spec.to_string(),
            });
        }
        if gate.name == "bash" {
            let cmd = gate.input.get("command").and_then(|c| c.as_str())?;
            let program = cmd.split_whitespace().next()?;
            Some(ConfigChange::Command {
                command: program.to_string(),
            })
        } else {
            Some(ConfigChange::Tool {
                name: gate.name.clone(),
            })
        }
    }

    /// Approve the pending gate without touching the composer — the control
    /// strip's `approve` verb (docs/tui.md). Unlike an empty-composer `⏎` submit,
    /// this leaves any drafted text in place (the draft may be a redirect the
    /// operator decided against sending).
    pub(in crate::tui) fn approve_gate(&mut self) {
        if let Some(gate) = self.pending_gate.take() {
            let _ = self.approve_tx.send((gate.call_id, Approval::Approve));
            self.ui.debug_message = "approved".into();
            self.viewport.stuck_to_bottom = true;
        }
    }

    /// Abort the pending gate — the strip's `abort` verb, same decision the typed
    /// `.abort` sentinel produces, draft likewise left in place.
    pub(in crate::tui) fn abort_gate(&mut self) {
        if let Some(gate) = self.pending_gate.take() {
            let _ = self.approve_tx.send((gate.call_id, Approval::Abort));
            self.ui.debug_message = "aborted".into();
            self.viewport.stuck_to_bottom = true;
        }
    }
}
