//! The detached drain task for one backgrounded process (docs/supervisor.md).

use super::*;

/// The detached drain task for one backgrounded process (docs/supervisor.md): feed every
/// output chunk into the registry's capture buffer until the child exits (EOF —
/// reached naturally, or because a kill closed the pipes), then mark it exited and
/// fire its exit notification into the inbox, where the next triage gate adjudicates
/// it like any notification. The supervisor lock is taken per chunk and never held
/// across an await.
pub(crate) async fn drain_background(
    shared: Arc<Shared>,
    handle: String,
    mut child: tokio::process::Child,
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
) {
    crate::bash::drain_pipes(
        &mut stdout,
        &mut stderr,
        std::future::pending::<()>(),
        |chunk| {
            shared.supervisor.lock().unwrap().push_chunk(&handle, chunk);
            // Smooth the live process panel between `/state` polls (TUI-bg-2). Lossy
            // and best-effort — the poll snapshot remains the authoritative tail.
            shared.publish_delta(StreamDelta::BgOutput {
                handle: handle.clone(),
                chunk: String::from_utf8_lossy(chunk).into_owned(),
            });
        },
    )
    .await;
    let status = child.wait().await.ok();
    let summary = shared.supervisor.lock().unwrap().finish(&handle, status);
    if let Some((source, body)) = summary {
        shared.notify(source, body, Some(handle), None);
    }
}

impl Shared {
    /// Spawn a backgrounded bash command into the supervisor (docs/supervisor.md). Returns
    /// its `bg#<n>` handle immediately; a detached task drains the child's output
    /// into the registry (re-surfaced live at each triage gate) and fires an exit
    /// notification when it ends. The child leads a fresh process group, so one
    /// `killpg` reaps it and any grandchildren on teardown.
    pub fn spawn_background(self: &Arc<Self>, command: String) -> io::Result<String> {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // The session scratch dir rides `$TMPDIR` into background children too,
        // mirroring the foreground `Bash` spawn (docs/tools.md).
        if let Some(scratch) = &self.scratch {
            cmd.env("TMPDIR", scratch);
        }
        let mut child = cmd.spawn()?;
        let pgid = child.id();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let handle = self.supervisor.lock().unwrap().register(command, pgid);
        tokio::spawn(drain_background(
            Arc::clone(self),
            handle.clone(),
            child,
            stdout,
            stderr,
        ));
        Ok(handle)
    }

    /// Adopt a *detached foreground* command into the supervisor (the
    /// foreground→background handoff, docs/supervisor.md). Mirrors
    /// [`spawn_background`](Self::spawn_background) from the registration on —
    /// same drain task, same exit notification — but the child is already
    /// running: the capture is seeded with everything the foreground path
    /// drained before the detach, and the drain resumes from that byte boundary.
    pub fn adopt_background(self: &Arc<Self>, detached: crate::bash::DetachedBash) -> String {
        let crate::bash::DetachedBash {
            command,
            child,
            stdout,
            stderr,
            seed,
        } = detached;
        let pgid = child.id();
        let handle = self.supervisor.lock().unwrap().adopt(command, pgid, &seed);
        tokio::spawn(drain_background(
            Arc::clone(self),
            handle.clone(),
            child,
            stdout,
            stderr,
        ));
        handle
    }

    /// `POST /background` (docs/supervisor.md): ask the loop to detach the
    /// running foreground tool into the supervisor, optionally attaching an
    /// operator note the handoff delivers as the next user message. A no-op if
    /// nothing is mid-tool (no waiter → the wake is lost; the note is cleared
    /// when the next foreground bash starts).
    pub fn request_detach(&self, note: Option<String>) {
        *self.detach_note.lock().unwrap() = note.filter(|n| !n.trim().is_empty());
        self.detach.notify_waiters();
    }

    /// Consume the pending detach note — called by the loop when a detach lands.
    pub fn take_detach_note(&self) -> Option<String> {
        self.detach_note.lock().unwrap().take()
    }

    /// The peek's ambient `background` section (docs/supervisor.md) — a snapshot of running,
    /// un-muted processes, taken under the lock so the loop can hand it to the fork.
    /// The control API's `processes` topic (docs/control-api.md) — structure only, no
    /// tail. Distinct from [`bg_snapshot`](Self::bg_snapshot), which is the model-facing
    /// peek view; see [`ProcessState`](crate::supervisor::ProcessState) for why they
    /// diverge.
    pub(super) fn process_states(&self) -> Vec<crate::supervisor::ProcessState> {
        self.supervisor.lock().unwrap().process_states()
    }

    pub fn bg_snapshot(&self) -> Vec<BgView> {
        self.supervisor.lock().unwrap().views()
    }

    /// Snapshot a process's current tail for a pin — `(source, body)` ready to wrap
    /// in a [`Notification`](crate::log::Notification) (docs/supervisor.md).
    pub fn bg_pin_snapshot(&self, handle: &str) -> Option<(String, String)> {
        self.supervisor.lock().unwrap().pin_snapshot(handle)
    }

    /// Mute a process's running tail (`dismiss` for a live process, docs/supervisor.md — it keeps
    /// running and still notifies on exit).
    pub fn mute_bg(&self, handle: &str) {
        self.supervisor.lock().unwrap().mute(handle);
    }

    /// The directed `list_processes` view (docs/supervisor.md).
    pub fn bg_list_text(&self) -> String {
        self.supervisor.lock().unwrap().list_text()
    }

    /// Cursor-based read of one process's output (`read_since`, docs/supervisor.md). `None` if the
    /// handle is unknown.
    pub fn bg_read_since(&self, handle: &str, cursor: Option<u64>) -> Option<String> {
        self.supervisor.lock().unwrap().read_since(handle, cursor)
    }

    /// Kill a running background process by handle (docs/supervisor.md — `kill_process`):
    /// `killpg` its group; the drain task then sees EOF and fires the exit
    /// notification. `false` if no such running process. The lock is dropped before
    /// the FFI kill (never held across it).
    pub fn kill_bg(&self, handle: &str) -> bool {
        let pgid = self.supervisor.lock().unwrap().pgid_of(handle);
        match pgid {
            Some(pgid) => {
                crate::bash::kill_group(pgid);
                true
            }
            None => false,
        }
    }

    /// `killpg` every still-running background child — daemon teardown (docs/supervisor.md
    /// ownership). Best-effort: a child that already exited is a benign `ESRCH`.
    pub fn kill_all_bg(&self) {
        for pgid in self.supervisor.lock().unwrap().running_pgids() {
            crate::bash::kill_group(pgid);
        }
    }
}
