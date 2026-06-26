//! The background-process supervisor (DESIGN §6 — Background processes, Slice 1).
//!
//! A backgrounded command is a *live entity*, not a log record: its running tail is
//! re-sampled fresh at each triage gate, never frozen into the log (forcing a live
//! tail through the immutable notification model would pile up stale snapshots,
//! DESIGN §6). So this registry is **ephemeral daemon state** — a sibling of the
//! inbox queue and the cache `Metrics` in [`Shared`](crate::daemon): dropped on
//! restart, where killpg-on-teardown reaps the children it owns.
//!
//! This module is the pure substrate: the registry, its one bounded per-process
//! capture buffer, and the read projections over it — [`Supervisor::views`] (the
//! ambient auto-show the peek renders each gate), [`Supervisor::pin_snapshot`] (a
//! frozen tail when the model pins), and [`Supervisor::finish`] (the exit
//! notification). The async spawn/drain orchestration — which needs `Arc<Shared>`
//! and the `POST /notify` path — lives in `daemon.rs`.

use std::process::ExitStatus;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Lines of tail shown per process in the peek (and carried in a pin / exit
/// snapshot). A tuned constant (DESIGN §6).
const TAIL_LINES: usize = 12;

/// Bounded per-process capture. Output past this is dropped from the *front*
/// (oldest-first) with a counted breadcrumb — no silent caps (DESIGN §6).
const BUFFER_CAP: usize = 256 * 1024;

/// One backgrounded process. `buffer` is the single bounded capture both read
/// projections derive from (the tail view, and the cursor `read_since` of Slice 2).
struct BgProcess {
    handle: String,
    command: String,
    /// The child's process-group id (== child pid; it leads a fresh group), for
    /// `killpg` teardown. `None` only if the OS didn't report a pid.
    pgid: Option<u32>,
    buffer: Vec<u8>,
    /// Bytes dropped off the front of `buffer` at the cap — surfaced so truncation
    /// is never silent.
    dropped: usize,
    spawned_at: Instant,
    last_chunk_at: Instant,
    /// Muted (the §6 `dismiss` for a live process): kept running, exit still
    /// notifies, but the running tail stops surfacing in the peek.
    muted: bool,
    running: bool,
}

/// A live snapshot of one running process for the peek's ambient `background`
/// section (DESIGN §6). Owned (it borrows nothing of the registry) so the loop can
/// build it under the lock and hand it to the fork distiller, which then awaits.
/// `Serialize` so the same snapshot rides `GET /state` to the human clients (the
/// TUI process panel, DESIGN §10.5) — one view, two consumers.
#[derive(Clone, Debug, Serialize)]
pub struct BgView {
    pub handle: String,
    pub command: String,
    pub tail: String,
    pub running_secs: u64,
    pub idle_secs: u64,
}

/// The ephemeral background-process registry (DESIGN §6). Held behind a `Mutex` in
/// [`Shared`](crate::daemon); never logged, never projected — its `Instant`s can't
/// reach the wire, so it's cache-safe by construction.
#[derive(Default)]
pub struct Supervisor {
    next: u64,
    procs: Vec<BgProcess>,
}

impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-spawned child and mint its `bg#<n>` handle. The caller
    /// (the daemon) spawns the drain task that feeds [`push_chunk`](Self::push_chunk)
    /// and ends in [`finish`](Self::finish).
    pub fn register(&mut self, command: String, pgid: Option<u32>) -> String {
        self.next += 1;
        let handle = format!("bg#{}", self.next);
        let now = Instant::now();
        self.procs.push(BgProcess {
            handle: handle.clone(),
            command,
            pgid,
            buffer: Vec::new(),
            dropped: 0,
            spawned_at: now,
            last_chunk_at: now,
            muted: false,
            running: true,
        });
        handle
    }

    /// Append a freshly-read output chunk to a process's bounded capture and stamp
    /// its activity clock (the source of the peek's `idle` figure).
    pub fn push_chunk(&mut self, handle: &str, chunk: &[u8]) {
        if let Some(p) = self.find_mut(handle) {
            p.buffer.extend_from_slice(chunk);
            if p.buffer.len() > BUFFER_CAP {
                let excess = p.buffer.len() - BUFFER_CAP;
                p.buffer.drain(..excess);
                p.dropped += excess;
            }
            p.last_chunk_at = Instant::now();
        }
    }

    /// Mute a process's running tail (the §6 `dismiss` for a live process).
    pub fn mute(&mut self, handle: &str) {
        if let Some(p) = self.find_mut(handle) {
            p.muted = true;
        }
    }

    /// The ambient auto-show: a snapshot of every *running, un-muted* process for
    /// the peek's `background` section (DESIGN §6). The directed `list()` read
    /// (Slice 2) will reuse this.
    pub fn views(&self) -> Vec<BgView> {
        let now = Instant::now();
        self.procs
            .iter()
            .filter(|p| p.running && !p.muted)
            .map(|p| BgView {
                handle: p.handle.clone(),
                command: p.command.clone(),
                tail: tail_text(&p.buffer, p.dropped),
                running_secs: now.saturating_duration_since(p.spawned_at).as_secs(),
                idle_secs: now.saturating_duration_since(p.last_chunk_at).as_secs(),
            })
            .collect()
    }

    /// Snapshot a running process's current tail for a **pin** (DESIGN §6): the
    /// model chose to freeze this moment onto the canonical wire. Returns
    /// `(source, body)` ready to wrap in a [`Notification`](crate::log::Notification)
    /// that the pinning `Triage` projects as `⟢ <source>: <body>`. `None` if the
    /// handle is unknown.
    pub fn pin_snapshot(&self, handle: &str) -> Option<(String, String)> {
        let p = self.find(handle)?;
        let running = Instant::now().saturating_duration_since(p.spawned_at);
        let body = format!(
            "{} · running {}\n{}",
            p.command,
            humanize(running),
            tail_text(&p.buffer, p.dropped)
        );
        Some((p.handle.clone(), body))
    }

    /// Mark a process exited and produce its exit notification `(source, body)` —
    /// status + duration + final tail (DESIGN §6, requirement #4). `None` if the
    /// handle is unknown.
    pub fn finish(&mut self, handle: &str, status: Option<ExitStatus>) -> Option<(String, String)> {
        let ran = {
            let p = self.find(handle)?;
            Instant::now().saturating_duration_since(p.spawned_at)
        };
        let p = self.find_mut(handle)?;
        p.running = false;
        let body = format!(
            "{} after {}; final output:\n{}",
            describe_status(status),
            humanize(ran),
            tail_text(&p.buffer, p.dropped)
        );
        Some((p.handle.clone(), body))
    }

    /// The pgids of every still-running process — the killpg set for teardown
    /// (DESIGN §6 ownership). A muted process is still running, so it's included.
    pub fn running_pgids(&self) -> Vec<u32> {
        self.procs
            .iter()
            .filter(|p| p.running)
            .filter_map(|p| p.pgid)
            .collect()
    }

    fn find(&self, handle: &str) -> Option<&BgProcess> {
        self.procs.iter().find(|p| p.handle == handle)
    }
    fn find_mut(&mut self, handle: &str) -> Option<&mut BgProcess> {
        self.procs.iter_mut().find(|p| p.handle == handle)
    }
}

/// The last [`TAIL_LINES`] lines of a capture, lossily decoded, with a leading
/// breadcrumb when content was dropped (off the front at the cap, or just older
/// than the tail window) — no silent truncation (DESIGN §6).
fn tail_text(buffer: &[u8], dropped: usize) -> String {
    let decoded = String::from_utf8_lossy(buffer);
    let lines: Vec<&str> = decoded.lines().collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    let mut out = String::new();
    if dropped > 0 {
        out.push_str(&format!("… [{dropped} earlier bytes dropped at the cap]\n"));
    } else if start > 0 {
        out.push_str(&format!("… [{start} earlier lines]\n"));
    }
    out.push_str(&lines[start..].join("\n"));
    if out.is_empty() {
        out.push_str("(no output yet)");
    }
    out
}

/// One-line exit status: clean, a non-zero code, or killed by a signal (no code).
fn describe_status(status: Option<ExitStatus>) -> String {
    match status {
        Some(s) if s.success() => "exited 0".to_string(),
        Some(s) => match s.code() {
            Some(c) => format!("exited {c}"),
            None => "killed by signal".to_string(),
        },
        None => "exited (status unavailable)".to_string(),
    }
}

/// Compact human duration: `8s`, `4m12s`, `1h3m`. For the live tail timing and the
/// pin/exit snapshots — display only, never projected.
fn humanize(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_mints_handles_and_tails_recent_output() {
        let mut s = Supervisor::new();
        let h = s.register("cargo watch".into(), Some(123));
        assert_eq!(h, "bg#1");
        s.push_chunk(&h, b"line one\nline two\n");

        let views = s.views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].command, "cargo watch");
        assert!(views[0].tail.contains("line two"), "{:?}", views[0].tail);
    }

    #[test]
    fn mute_drops_from_the_peek_but_keeps_it_running() {
        let mut s = Supervisor::new();
        let h = s.register("x".into(), Some(7));
        s.mute(&h);
        assert!(s.views().is_empty(), "muted → off the ambient show");
        assert_eq!(s.running_pgids(), vec![7], "muted ≠ killed: still in the teardown set");
    }

    #[test]
    fn finish_marks_exited_summarizes_and_leaves_the_show() {
        let mut s = Supervisor::new();
        let h = s.register("x".into(), Some(7));
        s.push_chunk(&h, b"done\n");
        let (source, body) = s.finish(&h, None).unwrap();
        assert_eq!(source, "bg#1");
        assert!(body.contains("done"), "exit carries the final tail: {body}");
        assert!(s.views().is_empty(), "exited → off the ambient show");
        assert!(s.running_pgids().is_empty(), "exited → out of the teardown set");
    }

    #[test]
    fn pin_snapshot_carries_command_and_current_tail() {
        let mut s = Supervisor::new();
        let h = s.register("npm test".into(), None);
        s.push_chunk(&h, b"PASS\n");
        let (source, body) = s.pin_snapshot(&h).unwrap();
        assert_eq!(source, "bg#1");
        assert!(body.contains("npm test") && body.contains("PASS"), "{body}");
    }

    #[test]
    fn front_drop_is_counted_not_silent() {
        let mut s = Supervisor::new();
        let h = s.register("noisy".into(), None);
        // Overflow the cap so the front is dropped.
        let chunk = vec![b'.'; BUFFER_CAP + 10];
        s.push_chunk(&h, &chunk);
        s.push_chunk(&h, b"\nlatest\n");
        let tail = &s.views()[0].tail;
        assert!(tail.contains("earlier bytes dropped"), "{tail}");
        assert!(tail.contains("latest"), "{tail}");
    }

    #[test]
    fn empty_capture_reads_as_no_output_yet() {
        let mut s = Supervisor::new();
        let h = s.register("slow".into(), None);
        assert!(s.views()[0].tail.contains("no output yet"));
    }
}
