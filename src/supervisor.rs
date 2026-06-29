//! The background-process supervisor (docs/supervisor.md).
//!
//! A backgrounded command is a *live entity*, not a log record: its running tail is
//! re-sampled fresh at each triage gate, never frozen into the log (forcing a live
//! tail through the immutable notification model would pile up stale snapshots,
//! docs/supervisor.md). So this registry is **ephemeral daemon state** — a sibling of the
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
use serde_json::{Value, json};

/// `list_processes` — the directed read of running processes (docs/supervisor.md). Named to
/// avoid colliding with fs-mcp's `list`.
pub const LIST: &str = "list_processes";
/// `read_process` — cursor-based tailing of one process's output (the supervisor's
/// `read_since`, docs/supervisor.md). Named to avoid colliding with fs-mcp's `read`.
pub const READ: &str = "read_process";
/// `kill_process` — terminate a background process's group (docs/supervisor.md).
pub const KILL: &str = "kill_process";
/// `check_inbox` — surface pending notifications on demand (the directed inbox read,
/// docs/inbox.md) so the model needn't wait for them to ride its next action.
pub const CHECK_INBOX: &str = "check_inbox";

/// The harness-level supervisor/inbox tools (docs/supervisor.md). Like `consolidate`, these
/// act on daemon state (the registry, the inbox), not the world, so the agent loop
/// dispatches them structurally — but their results distill through the universal
/// fork like any tool (no bypass). Definitions are constant `json!` (sorted keys),
/// so `tools[]` stays byte-stable for the session (docs/substrate.md).
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": LIST,
            "description": "List the background processes currently running — handle, command, how long each has run, and how long since its last output. Their recent output also surfaces automatically after each of your actions; use this to check on them deliberately.",
            "input_schema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": READ,
            "description": "Read a background process's output. Returns everything from `cursor` (a byte offset returned by a previous read; omit to start from the earliest retained output) to the end, plus a new cursor to pass next time so you don't re-read what you've seen.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "The process handle, e.g. \"bg#3\"." },
                    "cursor": { "type": "integer", "description": "Byte offset from a previous read_process result; omit to start from the earliest retained output." }
                },
                "required": ["handle"]
            }
        }),
        json!({
            "name": KILL,
            "description": "Kill a running background process (SIGKILL to its whole process group). Its exit is then confirmed in your inbox.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "The process handle, e.g. \"bg#3\"." }
                },
                "required": ["handle"]
            }
        }),
        json!({
            "name": CHECK_INBOX,
            "description": "Show your pending notifications now — background-process exits, operator nudges, messages from other agents — so you can triage them on demand instead of waiting for them to surface after your next action.",
            "input_schema": { "type": "object", "properties": {} }
        }),
    ]
}

/// Lines of tail shown per process in the peek (and carried in a pin / exit
/// snapshot). A tuned constant (docs/supervisor.md).
const TAIL_LINES: usize = 12;

/// Bounded per-process capture. Output past this is dropped from the *front*
/// (oldest-first) with a counted breadcrumb — no silent caps (docs/supervisor.md).
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
    /// Muted (the supervisor's `dismiss` for a live process, docs/supervisor.md): kept running, exit still
    /// notifies, but the running tail stops surfacing in the peek.
    muted: bool,
    running: bool,
}

/// A live snapshot of one running process for the peek's ambient `background`
/// section (docs/supervisor.md). Owned (it borrows nothing of the registry) so the loop can
/// build it under the lock and hand it to the fork distiller, which then awaits.
/// `Serialize` so the same snapshot rides `GET /state` to the human clients (the
/// TUI process panel, docs/tui.md) — one view, two consumers.
#[derive(Clone, Debug, Serialize)]
pub struct BgView {
    pub handle: String,
    pub command: String,
    pub tail: String,
    pub running_secs: u64,
    pub idle_secs: u64,
}

/// The ephemeral background-process registry (docs/supervisor.md). Held behind a `Mutex` in
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

    /// Mute a process's running tail (the supervisor's `dismiss` for a live process, docs/supervisor.md).
    pub fn mute(&mut self, handle: &str) {
        if let Some(p) = self.find_mut(handle) {
            p.muted = true;
        }
    }

    /// The ambient auto-show: a snapshot of every *running, un-muted* process for
    /// the peek's `background` section (docs/supervisor.md). The directed `list()` read
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

    /// Snapshot a running process's current tail for a **pin** (docs/supervisor.md): the
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
    /// status + duration + final tail (docs/supervisor.md, requirement #4). `None` if the
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

    /// The directed `list_processes` view (docs/supervisor.md): all *running* processes —
    /// muted ones included and annotated, since the model asked explicitly (the
    /// ambient `views()` auto-show hides muted; this doesn't). Exited ones have
    /// already notified, so they drop off.
    pub fn list_text(&self) -> String {
        let running: Vec<&BgProcess> = self.procs.iter().filter(|p| p.running).collect();
        if running.is_empty() {
            return "No background processes running.".to_string();
        }
        let now = Instant::now();
        let mut out = format!("{} background process(es) running:", running.len());
        for p in running {
            let run = humanize(now.saturating_duration_since(p.spawned_at));
            let idle = humanize(now.saturating_duration_since(p.last_chunk_at));
            let muted = if p.muted { " (muted)" } else { "" };
            out.push_str(&format!(
                "\n[{}] {} · running {run} · idle {idle}{muted}",
                p.handle, p.command
            ));
        }
        out
    }

    /// Cursor-based tailing of one process's output (the supervisor's `read_since`, docs/supervisor.md): return
    /// everything from `cursor` (a byte offset into the *logical* stream; `None` =
    /// from the earliest still-retained byte) to the current end, plus the new
    /// cursor. Bytes dropped off the front at the cap (and thus before the cursor)
    /// are reported, not silently skipped. `None` if the handle is unknown.
    pub fn read_since(&self, handle: &str, cursor: Option<u64>) -> Option<String> {
        let p = self.find(handle)?;
        let dropped = p.dropped as u64;
        let total = dropped + p.buffer.len() as u64;
        // Can't read before what we still hold; track anything lost before the cursor.
        let from = cursor.unwrap_or(dropped).max(dropped);
        let lost = match cursor {
            Some(c) if c < dropped => dropped - c,
            _ => 0,
        };
        let now = Instant::now();
        let status = if p.running {
            format!(
                "running · idle {}",
                humanize(now.saturating_duration_since(p.last_chunk_at))
            )
        } else {
            "exited".to_string()
        };
        if from >= total {
            return Some(format!(
                "{handle}: no new output since cursor {from} ({status}). cursor: {total}"
            ));
        }
        let slice = &p.buffer[(from - dropped) as usize..];
        let mut out = String::new();
        if lost > 0 {
            out.push_str(&format!(
                "… [{lost} bytes before your cursor were dropped at the cap]\n"
            ));
        }
        out.push_str(&format!(
            "{handle} output [bytes {from}–{total}] ({status}):\n"
        ));
        out.push_str(&String::from_utf8_lossy(slice));
        out.push_str(&format!("\ncursor: {total}"));
        Some(out)
    }

    /// The pgid of a *running* process by handle — the lookup `kill_process` and the
    /// daemon's `kill_bg` use. `None` if unknown or already exited.
    pub fn pgid_of(&self, handle: &str) -> Option<u32> {
        self.procs
            .iter()
            .find(|p| p.handle == handle && p.running)
            .and_then(|p| p.pgid)
    }

    /// The pgids of every still-running process — the killpg set for teardown
    /// (docs/supervisor.md). A muted process is still running, so it's included.
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
/// than the tail window) — no silent truncation (docs/supervisor.md).
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
        assert_eq!(
            s.running_pgids(),
            vec![7],
            "muted ≠ killed: still in the teardown set"
        );
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
        assert!(
            s.running_pgids().is_empty(),
            "exited → out of the teardown set"
        );
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
        s.register("slow".into(), None);
        assert!(s.views()[0].tail.contains("no output yet"));
    }

    #[test]
    fn list_text_shows_running_and_marks_muted() {
        let mut s = Supervisor::new();
        s.register("cargo watch".into(), Some(1));
        let b = s.register("sleep 9".into(), Some(2));
        s.mute(&b);
        let text = s.list_text();
        assert!(text.contains("[bg#1] cargo watch"), "{text}");
        assert!(
            text.contains("[bg#2] sleep 9") && text.contains("(muted)"),
            "{text}"
        );
    }

    #[test]
    fn read_since_advances_a_cursor_and_reports_no_new() {
        let mut s = Supervisor::new();
        let h = s.register("x".into(), None);
        s.push_chunk(&h, b"hello world\n");
        let first = s.read_since(&h, None).unwrap();
        assert!(first.contains("hello world"), "{first}");
        // Pull the cursor out of the footer and read again — nothing new.
        let cursor: u64 = first
            .rsplit("cursor: ")
            .next()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            s.read_since(&h, Some(cursor))
                .unwrap()
                .contains("no new output")
        );
        // More output → only the delta comes back from that cursor.
        s.push_chunk(&h, b"more\n");
        let third = s.read_since(&h, Some(cursor)).unwrap();
        assert!(
            third.contains("more") && !third.contains("hello"),
            "{third}"
        );
    }

    #[test]
    fn read_since_unknown_handle_is_none() {
        assert!(Supervisor::new().read_since("bg#99", None).is_none());
    }

    #[test]
    fn pgid_of_only_running() {
        let mut s = Supervisor::new();
        let h = s.register("x".into(), Some(42));
        assert_eq!(s.pgid_of(&h), Some(42));
        s.finish(&h, None);
        assert_eq!(s.pgid_of(&h), None, "exited → no kill target");
    }
}
