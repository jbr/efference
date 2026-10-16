//! The detached drain task for one backgrounded process (docs/supervisor.md).

use super::*;

/// The detached drain task for one backgrounded process (docs/supervisor.md): feed every
/// output chunk into the registry's capture buffer until the child exits (EOF —
/// reached naturally, or because a kill closed the pipes), then mark it exited and
/// fire its exit notification into the inbox, where the next triage adjudicates
/// it like any notification. The supervisor lock is taken per chunk and never held
/// across an await.
pub(crate) async fn drain_background(
    shared: Arc<Session>,
    id: u64,
    mut child: tokio::process::Child,
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
) {
    crate::bash::drain_pipes(
        &mut stdout,
        &mut stderr,
        std::future::pending::<()>(),
        |chunk| {
            let hits = shared.supervisor.lock().unwrap().push_chunk(id, chunk);
            // A watch that matched in this chunk (docs/supervisor.md) — posted to the
            // inbox like any ambient event, which is what wakes an idle agent. The
            // registry lock is already gone; `notify` takes the inbox's.
            // No slot: a watch fires once, so it has nothing to supersede — and the
            // process's `p#<id>` slot belongs to its exit notification, which a hit
            // must neither replace nor be replaced by (docs/inbox.md).
            for hit in hits {
                let (source, body) = hit.notification();
                shared.notify(source, body, None, None);
            }
            // The live process tail on the stream's ephemeral `output` class — lossy
            // and best-effort; a client seeds itself from the process resource and
            // layers these on top (docs/control-api.md).
            shared.publish_delta(StreamDelta::ProcessOutput {
                id,
                chunk: String::from_utf8_lossy(chunk).into_owned(),
            });
        },
    )
    .await;
    let status = child.wait().await.ok();
    let summary = shared.supervisor.lock().unwrap().finish(id, status);
    // The child is reaped and out of the running set: teardown waiting on this
    // session's children can re-check (docs/supervisor.md).
    shared.reaped.notify_waiters();
    shared.touch_state();
    if let Some(finished) = summary {
        // The durable half (docs/supervisor.md): the exit is a fact about the
        // session, so it lands in the log where a reader with no daemon can still
        // answer what `p#<id>` did. Appended before the notification, so a reader
        // that sees the exit prose can always resolve the handle beside it.
        shared.append_process_event(crate::log::ProcessEvent {
            id,
            kind: crate::log::ProcessEventKind::Exited(finished.exit),
        });
        let slot = crate::supervisor::handle(id);
        shared.notify(finished.source, finished.body, Some(slot), None);
    }
}

impl Session {
    /// Spawn a backgrounded bash command into the supervisor (docs/supervisor.md). Returns
    /// its `p#<n>` handle immediately; a detached task drains the child's output
    /// into the registry (re-surfaced live at each triage) and fires an exit
    /// notification when it ends. The child leads a fresh process group, so one
    /// `killpg` reaps it and any grandchildren on teardown.
    /// `call_id` is the `tool_use` this spawn answers — the call↔process mapping
    /// (docs/supervisor.md), stamped at the mint so a client holding either half
    /// can find the other.
    pub fn spawn_background(
        self: &Arc<Self>,
        command: String,
        call_id: Option<String>,
    ) -> io::Result<String> {
        let mut cmd = Command::new("bash");
        cmd.current_dir(self.cwd())
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        self.env_policy.child_env().apply_tokio(&mut cmd);
        // The session scratch dir rides `$TMPDIR` into background children too,
        // mirroring the foreground `Bash` spawn (docs/tools.md).
        if let Some(scratch) = &self.scratch {
            cmd.env("TMPDIR", scratch);
        }
        let mut child = crate::spawn::tokio(&mut cmd)?;
        let pgid = child.id();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let id = self
            .supervisor
            .lock()
            .unwrap()
            .register_background(command, pgid, call_id);
        self.record_mint(id);
        self.touch_state();
        tokio::spawn(drain_background(
            Arc::clone(self),
            id,
            child,
            stdout,
            stderr,
        ));
        Ok(crate::supervisor::handle(id))
    }

    /// Register the loop's freshly-spawned *foreground* command in the process
    /// namespace (docs/supervisor.md): addressable — and listed on the `processes`
    /// topic — from spawn, so the handoff is a property change on a resource that
    /// already exists. Returns the spawn-minted id.
    pub fn register_foreground(
        &self,
        command: String,
        pgid: Option<u32>,
        call_id: Option<String>,
    ) -> u64 {
        let id = self
            .supervisor
            .lock()
            .unwrap()
            .register_foreground(command, pgid, call_id);
        self.touch_state();
        id
    }

    /// Feed one raw foreground output chunk into the process's capture, so
    /// `GET /processes/:id/output` can seed a cold-opened client mid-run
    /// (docs/control-api.md). No state touch: buffer content is not topic state.
    /// Watches are scanned here as they are on the background drain — one path, so
    /// a watch registered before a detach keeps matching across the handoff.
    pub fn push_process_chunk(&self, id: u64, chunk: &[u8]) {
        let hits = self.supervisor.lock().unwrap().push_chunk(id, chunk);
        for hit in hits {
            let (source, body) = hit.notification();
            self.notify(source, body, None, None);
        }
    }

    /// Register a one-shot output watch by the model's `p#<n>` vocabulary
    /// (`watch_process`, docs/supervisor.md). `false` if the handle is unparseable,
    /// unknown, or already exited; the count is what the process is now waiting on.
    pub fn watch_bg(&self, handle: &str, pattern: String) -> Option<usize> {
        let id = crate::supervisor::parse_handle(handle)?;
        let mut supervisor = self.supervisor.lock().unwrap();
        supervisor
            .watch(id, pattern)
            .then(|| supervisor.watch_count(id))
    }

    /// Drop a completed foreground command from the registry: its canonical output
    /// is the paired `tool_result` (docs/supervisor.md).
    pub fn conclude_foreground(&self, id: u64) {
        self.supervisor.lock().unwrap().remove(id);
        self.touch_state();
    }

    /// Adopt a *detached foreground* command into the background (the
    /// foreground→background handoff, docs/supervisor.md). `id` is the entry the
    /// loop registered at spawn: the flip keeps the id and the capture — every
    /// pre-detach byte was already fed live by the foreground chunk sink, so the
    /// surrendered `seed` is deliberately not re-pushed — and from here the same
    /// drain task and exit notification as a spawned background process take over,
    /// resuming from exactly the byte boundary the foreground drain stopped at.
    pub fn adopt_background(
        self: &Arc<Self>,
        detached: crate::bash::DetachedBash,
        id: u64,
    ) -> String {
        let crate::bash::DetachedBash {
            child,
            stdout,
            stderr,
            ..
        } = detached;
        self.supervisor.lock().unwrap().make_background(id);
        // The handle becomes a durable fact here, not at the foreground spawn:
        // this is where it enters prose the model and the operator read
        // (docs/supervisor.md). The record's spawn stamp is the original spawn's,
        // so the adoption doesn't backdate the process.
        self.record_mint(id);
        self.touch_state();
        tokio::spawn(drain_background(
            Arc::clone(self),
            id,
            child,
            stdout,
            stderr,
        ));
        crate::supervisor::handle(id)
    }

    /// `PATCH /processes/:id {background: true}` (docs/supervisor.md): ask the loop to detach the
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

    /// What `GET /processes` and the `processes` state topic carry — structure only,
    /// no tail (docs/control-api.md). Distinct from
    /// [`bg_snapshot`](Self::bg_snapshot), the model-facing triage view; see
    /// [`ProcessState`](crate::supervisor::ProcessState) for why they diverge.
    pub(crate) fn process_states(&self) -> Vec<crate::supervisor::ProcessState> {
        self.supervisor.lock().unwrap().process_states()
    }

    /// The triage's ambient `background` section (docs/supervisor.md) — a snapshot of
    /// running, un-muted processes, taken under the lock so the loop can hand it to
    /// the fork, which then awaits.
    pub fn bg_snapshot(&self) -> Vec<BgView> {
        self.supervisor.lock().unwrap().views()
    }

    /// Snapshot a process's current tail for a pin — `(source, body)` ready to wrap
    /// in a [`Notification`](crate::log::Notification) (docs/supervisor.md). The
    /// handle string is the model's vocabulary; an unparseable one answers like an
    /// unknown process.
    pub fn bg_pin_snapshot(&self, handle: &str) -> Option<(String, String)> {
        let id = crate::supervisor::parse_handle(handle)?;
        self.supervisor.lock().unwrap().pin_snapshot(id)
    }

    /// Mute a process's running tail (`dismiss` for a live process, docs/supervisor.md — it keeps
    /// running and still notifies on exit).
    pub fn mute_bg(&self, handle: &str) {
        if let Some(id) = crate::supervisor::parse_handle(handle) {
            self.supervisor.lock().unwrap().set_muted(id, true);
            self.touch_state();
        }
    }

    /// Whether a running background process can produce the next notification that
    /// wakes the loop-owned `wait` tool (docs/supervisor.md).
    pub fn has_running_background(&self) -> bool {
        self.supervisor.lock().unwrap().has_running_background()
    }

    /// The directed `list_processes` view (docs/supervisor.md).
    pub fn bg_list_text(&self) -> String {
        self.supervisor.lock().unwrap().list_text()
    }

    /// Cursor-based read of one process's output (`read_since`, docs/supervisor.md). `None` if the
    /// handle is unknown.
    pub fn bg_read_since(&self, handle: &str, cursor: Option<u64>) -> Option<String> {
        let id = crate::supervisor::parse_handle(handle)?;
        self.supervisor.lock().unwrap().read_since(id, cursor)
    }

    /// One *running* process's representation — the `PATCH`/`DELETE` target check
    /// for `/processes/:id` (docs/control-api.md).
    pub fn running_process(&self, id: u64) -> Option<crate::supervisor::ProcessState> {
        self.supervisor.lock().unwrap().running_state(id)
    }

    /// One process's record for `GET /processes/:id` — **any handle this session
    /// ever minted into the background**, running or exited (docs/supervisor.md).
    /// The live registry answers first (it is the richer source and holds the
    /// capture the sibling output route reads); a log fold answers for a recorded
    /// session, or for a handle this daemon minted before a restart dropped the
    /// registry. `None` only when nothing anywhere names the id.
    pub fn process_record(&self, id: u64) -> Option<crate::supervisor::ProcessRecord> {
        let live = self.supervisor.lock().unwrap().record(id);
        if live.is_some() {
            return live;
        }
        crate::supervisor::from_log(self.log.lock().unwrap().entries(), id)
    }

    /// Append the **mint** half of a process's durable record, read from the live
    /// registry (docs/supervisor.md). Called where a handle enters the background
    /// namespace — a background spawn, or the handoff's adoption.
    fn record_mint(&self, id: u64) {
        // The registry lock is dropped before the append, which writes to disk:
        // every drain task takes this lock per output chunk.
        let event = self.supervisor.lock().unwrap().spawn_event(id);
        if let Some(event) = event {
            self.append_process_event(event);
        }
    }

    /// Append one [`ProcessEvent`](crate::log::ProcessEvent) (docs/supervisor.md).
    /// It projects to nothing, so its turn is audit grouping only and the
    /// in-flight turn is the honest placement — the same reasoning a `ConfigEdit`
    /// appended out of band uses. A failed write is reported and dropped: the
    /// record is worth having, and losing it must not fail the spawn or the exit
    /// path that was writing it.
    pub(crate) fn append_process_event(&self, event: crate::log::ProcessEvent) {
        let turn = self.latest_turn();
        if let Err(e) = self.append(turn, crate::log::Payload::ProcessEvent(event)) {
            log::warn!("process record: append failed: {e}");
        }
    }

    /// One cursor read of a process's retained output, structured — the body of
    /// `GET /processes/:id/output` (docs/control-api.md). With no cursor it returns
    /// everything retained: the seed a cold-opened client renders before its first
    /// `process_output` delta.
    pub fn process_output_read(
        &self,
        id: u64,
        cursor: Option<u64>,
    ) -> Option<crate::supervisor::OutputRead> {
        self.supervisor.lock().unwrap().read_output(id, cursor)
    }

    /// Set a running process's muted flag (`PATCH /processes/:id {muted}`,
    /// docs/control-api.md). `false` if no such running process.
    pub fn set_process_muted(&self, id: u64, muted: bool) -> bool {
        let set = self.supervisor.lock().unwrap().set_muted(id, muted);
        if set {
            self.touch_state();
        }
        set
    }

    /// Kill a running process by id (`DELETE /processes/:id`, and the model's
    /// `kill_process`): `killpg` its group; the drain task then sees EOF and fires
    /// the exit notification. `false` if no such running process. The lock is
    /// dropped before the FFI kill (never held across it).
    pub fn kill_process(&self, id: u64) -> bool {
        let pgid = self.supervisor.lock().unwrap().pgid_of(id);
        match pgid {
            Some(pgid) => {
                crate::bash::kill_group(pgid);
                true
            }
            None => false,
        }
    }

    /// [`kill_process`](Self::kill_process) addressed by the model's `p#<n>`
    /// vocabulary; an unparseable handle answers like an unknown process.
    pub fn kill_bg(&self, handle: &str) -> bool {
        crate::supervisor::parse_handle(handle).is_some_and(|id| self.kill_process(id))
    }

    /// `killpg` every still-running background child — daemon teardown (docs/supervisor.md
    /// ownership). Best-effort: a child that already exited is a benign `ESRCH`.
    ///
    /// Signalling only. `killpg` returns once the signal is queued, and a killed
    /// process stays in the table — answering `killpg(pgid, 0)` — until its
    /// parent waits it, so this returning does not mean the children are gone.
    /// Teardown wants [`reap_all_bg`](Self::reap_all_bg).
    pub fn kill_all_bg(&self) {
        for pgid in self.supervisor.lock().unwrap().running_pgids() {
            crate::bash::kill_group(pgid);
        }
    }

    /// Kill every background child **and wait for it to be reaped**, which is
    /// what session teardown means by "the session's processes are gone"
    /// (docs/supervisor.md).
    ///
    /// The wait is on the reap itself, not a clock: `drain_background` is
    /// already awaiting `child.wait()` on every background child, and clears it
    /// from the running set when that returns. So there is no interval to tune
    /// and no window where scratch cleanup runs against a process still holding
    /// a file open under it.
    ///
    /// SIGKILL has already been sent, so there is no further escalation to
    /// offer: a group that never leaves the table is blocked in the kernel, and
    /// no second signal from anywhere can move it. The operator's own escalation
    /// is the host's — a second shutdown signal ends the process outright.
    pub async fn reap_all_bg(&self) {
        loop {
            // Registered before the check: `notify_waiters` stores no permit, so
            // a reap landing between the two would otherwise be missed and this
            // would wait for a child that is already gone.
            let reaped = self.reaped.notified();
            self.kill_all_bg();
            if self.supervisor.lock().unwrap().running_pgids().is_empty() {
                return;
            }
            reaped.await;
        }
    }
}
