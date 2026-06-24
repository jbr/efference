//! The `Frontend` — the user-facing interaction seam (DESIGN §10).
//!
//! The agent loop and the log know only this trait; whichever backend is mounted
//! (today: stdin line-editor; later: reedline, then ratatui) sits behind it, so
//! swapping the interaction surface never touches the substrate. Phase 0 ships
//! one backend, [`Interactive`].
//!
//! ## Two interaction modes
//!
//! - [`Frontend::read_user_turn`] — a line read (the human's message to the
//!   agent). EOF (`None`) means "done" → the loop quits.
//! - [`Frontend::approve`] — a gate on a pending tool call. Enter (an empty line)
//!   approves; any other text rejects the call and is sent to the model as the
//!   next user message (a *redirect*); C-c bails the turn. This is the Phase-0
//!   guardrail: see the command before it runs, wave it through or steer.
//!
//! ## Why a signal is an approval channel (DESIGN §10)
//!
//! An agent (me, Claude Code, exercising this harness) can background a process
//! and *read* its output, but cannot *write* to its stdin — and shell state
//! doesn't persist between tool calls, so it can't even hold a pipe open. So the
//! interactive backend accepts approval over **three** channels, raced in one
//! `select!`:
//!
//! - stdin Enter — approve; any other line — redirect (reject the call and send
//!   the line to the model as the next user turn);
//! - **SIGUSR1** — approve; drivable with just a pid (`kill -USR1 <pid>`), which
//!   is the one robust channel an automated driver has. Unambiguous in Phase 0
//!   because the loop is sequential — exactly one approval is ever pending.
//! - **SIGINT** (C-c) — abort the turn; doubles as the bash tool's run-abort via
//!   [`Frontend::interrupted`], keeping one coherent "stop the current thing"
//!   signal story.
//!
//! This is dev/automation scaffolding: a process-global signal is too coarse once
//! concurrency arrives (DESIGN §5/§6), where real IPC/TUI supersedes it.

use std::io;

use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;

use crate::log::ToolCall;

/// Where the interactive backend records its pid so an approver can find it.
pub const PIDFILE: &str = ".efference.pid";

/// The decision returned by [`Frontend::approve`] (DESIGN §10). The empty-line
/// default is *approve*; a typed line is a *redirect* — the call is rejected and
/// the human's text is fed to the model as the next user turn, steering it
/// without ending the turn. C-c still bails. Every un-run `tool_use` always gets
/// a paired `is_error` `tool_result` so the wire never orphans a call (the API
/// rejects that); the loop owns that pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    /// Run the call (empty line, or SIGUSR1).
    Approve,
    /// Don't run it; bail the turn back to the user prompt (SIGINT).
    Abort,
    /// Don't run it; reject the call and send this text to the model as the next
    /// user message, then keep going. The human's words land as a real user turn
    /// (after the rejection `tool_result`), so the model reads it as direction,
    /// not tool output.
    Redirect(String),
}

/// The interaction seam. Used generically by the loop (`<F: Frontend>`); not
/// object-safe, and it needn't be — there's one mounted backend at a time.
#[allow(async_fn_in_trait)] // single-threaded interaction; no Send bound needed.
pub trait Frontend {
    /// Read the next user turn. `None` = end of input (the user is done) → quit.
    async fn read_user_turn(&mut self) -> Option<String>;

    /// Gate a pending tool call: approve it, or abort the turn.
    async fn approve(&mut self, call: &ToolCall) -> Approval;

    /// Resolve when the user interrupts (SIGINT) a *running* tool. The loop
    /// passes this as the bash tool's `abort` future so C-c kills the process
    /// group (DESIGN §6).
    async fn interrupted(&mut self);
}

/// Phase 0's backend: stdin line input + SIGUSR1/SIGINT approval.
///
/// A single background thread owns the (blocking) stdin read and forwards lines
/// over a channel. That keeps the input drainable from `select!` without leaking
/// a blocked reader each time a *signal* wins the race — the thread lives on and
/// the next line lands on the next `recv`. The loop's sequential shape means the
/// same channel safely serves both `read_user_turn` and `approve`.
pub struct Interactive {
    lines: mpsc::Receiver<String>,
    sigusr1: Signal,
    sigint: Signal,
}

impl Interactive {
    /// Spawn the stdin reader, register the signal listeners, drop a pidfile, and
    /// print how to drive approvals.
    pub fn new() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<String>(8);
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut buf = String::new();
            loop {
                buf.clear();
                match stdin.read_line(&mut buf) {
                    Ok(0) => break, // EOF → drop tx → channel closes → recv None.
                    Ok(_) => {
                        if tx.blocking_send(buf.trim_end().to_string()).is_err() {
                            break; // receiver gone; nothing left to feed.
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let me = Self::with_lines(rx)?;
        me.announce();
        Ok(me)
    }

    /// Register the signal listeners around a given line source. Shared by
    /// [`Interactive::new`] and the tests (which feed `lines` directly instead of
    /// spawning the real stdin reader).
    fn with_lines(lines: mpsc::Receiver<String>) -> io::Result<Self> {
        Ok(Self {
            lines,
            sigusr1: signal(SignalKind::user_defined1())?,
            sigint: signal(SignalKind::interrupt())?,
        })
    }

    fn announce(&self) {
        let pid = std::process::id();
        let _ = std::fs::write(PIDFILE, pid.to_string());
        eprintln!("efference pid {pid}  (wrote {PIDFILE})");
        eprintln!("  approve a tool call:  kill -USR1 {pid}   (or type y + Enter)");
        eprintln!("  abort:                C-c");
    }
}

impl Frontend for Interactive {
    async fn read_user_turn(&mut self) -> Option<String> {
        self.lines.recv().await
    }

    async fn approve(&mut self, call: &ToolCall) -> Approval {
        eprintln!("\n▶ tool call: {}", call.name);
        eprintln!("  input: {}", call.input);
        eprintln!(
            "  Enter to approve · type a message to redirect · C-c to abort   (or kill -USR1 {})",
            std::process::id()
        );

        // Destructure so the three branches borrow disjoint fields of `self`.
        let Self {
            lines,
            sigusr1,
            sigint,
        } = self;
        let mut stdin_open = true;
        loop {
            tokio::select! {
                // Only poll stdin while it's open. A *closed* stdin — a
                // backgrounded, signal-driven run with no human at the keyboard —
                // must NOT resolve, or every call would decide itself before an
                // approver could send SIGUSR1. On EOF we stop polling it and
                // wait on the signals alone.
                line = lines.recv(), if stdin_open => match line {
                    // Empty line (just Enter) approves; any other text redirects.
                    Some(answer) if answer.trim().is_empty() => return Approval::Approve,
                    Some(answer) => return Approval::Redirect(answer),
                    None => stdin_open = false,        // EOF: signals only now.
                },
                _ = sigusr1.recv() => return Approval::Approve,
                _ = sigint.recv() => return Approval::Abort,
            }
        }
    }

    async fn interrupted(&mut self) {
        self.sigint.recv().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::CallId;
    use serde_json::json;
    use std::time::Duration;

    fn a_call() -> ToolCall {
        ToolCall {
            id: CallId("toolu_1".into()),
            name: "bash".into(),
            input: json!({ "command": "echo hi" }),
        }
    }

    /// Empty input (just Enter) approves — the default.
    #[tokio::test]
    async fn empty_line_approves() {
        let (tx, rx) = mpsc::channel(1);
        let mut f = Interactive::with_lines(rx).unwrap();
        tx.send("".to_string()).await.unwrap();
        assert_eq!(f.approve(&a_call()).await, Approval::Approve);
    }

    /// Whitespace-only input still counts as empty → approve.
    #[tokio::test]
    async fn blank_line_approves() {
        let (tx, rx) = mpsc::channel(1);
        let mut f = Interactive::with_lines(rx).unwrap();
        tx.send("   ".to_string()).await.unwrap();
        assert_eq!(f.approve(&a_call()).await, Approval::Approve);
    }

    /// Any other text rejects the call and rides back as a redirect, verbatim.
    #[tokio::test]
    async fn typed_text_redirects() {
        let (tx, rx) = mpsc::channel(1);
        let mut f = Interactive::with_lines(rx).unwrap();
        tx.send("use ripgrep instead".to_string()).await.unwrap();
        assert_eq!(
            f.approve(&a_call()).await,
            Approval::Redirect("use ripgrep instead".to_string())
        );
    }

    /// Closed stdin at an approval must NOT auto-resolve — a backgrounded,
    /// signal-driven run has no human typing, so `approve` waits on the signals
    /// instead of reading EOF as a deny. (We can't assert the SIGUSR1 → Approve
    /// path here without a process-global signal, which is parallel-flaky; that
    /// path is exercised live. Here we just pin that it *blocks*.)
    #[tokio::test]
    async fn closed_stdin_waits_for_a_signal_rather_than_aborting() {
        let (tx, rx) = mpsc::channel::<String>(1);
        let mut f = Interactive::with_lines(rx).unwrap();
        drop(tx); // stdin closed, no signal incoming.
        let pending = tokio::time::timeout(Duration::from_millis(50), f.approve(&a_call())).await;
        assert!(pending.is_err(), "must block waiting for a signal, not resolve");
    }

    /// `read_user_turn` yields the line, then `None` once stdin closes.
    #[tokio::test]
    async fn read_user_turn_then_eof() {
        let (tx, rx) = mpsc::channel(1);
        let mut f = Interactive::with_lines(rx).unwrap();
        tx.send("do the thing".to_string()).await.unwrap();
        assert_eq!(f.read_user_turn().await.as_deref(), Some("do the thing"));
        drop(tx);
        assert_eq!(f.read_user_turn().await, None);
    }
}
