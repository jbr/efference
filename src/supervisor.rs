//! The background-process supervisor (docs/supervisor.md).
//!
//! A backgrounded command is a *live entity*, not a log record: its running tail is
//! re-sampled fresh at each triage gate, never frozen into the log (forcing a live
//! tail through the immutable notification model would pile up stale snapshots,
//! docs/supervisor.md). So this registry is **ephemeral daemon state** — a sibling of the
//! inbox queue and the cache `Metrics` in [`Shared`](crate::daemon): dropped on
//! restart, where killpg-on-teardown reaps the children it owns.
//!
//! This module holds the state: the registry, its one bounded per-process
//! capture buffer, and the read projections over it — [`Supervisor::views`] (the
//! ambient auto-show the peek renders each gate), [`Supervisor::pin_snapshot`] (a
//! frozen tail when the model pins), and [`Supervisor::finish`] (the exit
//! notification). The async spawn/drain orchestration — which needs `Arc<Shared>`
//! and the `POST /notifications` path — lives in `daemon.rs`.

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
/// so `tools[]` stays byte-stable for the session (docs/log-and-projection.md).
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
                    "handle": { "type": "string", "description": "The process handle, e.g. \"p#3\"." },
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
                    "handle": { "type": "string", "description": "The process handle, e.g. \"p#3\"." }
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

/// Render a process id as its prose handle — the form the model and every human
/// transcript sees (`p#3`). The bare ordinal is what routes and frames carry
/// (docs/supervisor.md): the prefix does its disambiguating work only where the id
/// floats free of context, and `#` is the URL fragment delimiter, so it must never
/// reach a path.
pub fn handle(id: u64) -> String {
    format!("p#{id}")
}

/// Parse a prose handle (`p#3`) back to its id — the model-boundary inverse of
/// [`handle`]. `None` for anything else; an unknown handle and a malformed one
/// answer the same way (the process doesn't exist).
pub fn parse_handle(s: &str) -> Option<u64> {
    s.strip_prefix("p#")?.parse().ok()
}

/// The highest `p#<n>` ordinal mentioned in `text`, or 0. Boot seeds the mint
/// counter with the max over the resumed log's serialized entries so a restarted
/// daemon can never re-mint an ordinal the log already refers to — the same
/// durability argument that keeps OS pids out of the identifier
/// (docs/supervisor.md). Over-matching is safe (a skipped ordinal
/// costs nothing); under-matching is the collision.
pub fn max_handle_mention(text: &str) -> u64 {
    let mut max = 0;
    for (idx, _) in text.match_indices("p#") {
        let digits: String = text[idx + 2..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(n) = digits.parse() {
            max = std::cmp::max(max, n);
        }
    }
    max
}

/// Lines of tail shown per process in the peek (and carried in a pin / exit
/// snapshot). A tuned constant (docs/supervisor.md).
const TAIL_LINES: usize = 12;

/// Bounded per-process capture. Output past this is dropped from the *front*
/// (oldest-first) with a counted breadcrumb — no silent caps (docs/supervisor.md).
const BUFFER_CAP: usize = 256 * 1024;

/// One backgrounded process. `buffer` is the single bounded capture both read
/// projections derive from (the tail view, and the cursor `read_since` of Slice 2).
struct Process {
    /// The spawn-minted ordinal — the durable identity that survives every property
    /// change (docs/supervisor.md). Rendered `p#<n>` in prose by
    /// [`handle`]; carried bare in routes and frames.
    id: u64,
    command: String,
    /// Whether the process runs detached under the supervisor (`true`) or is the
    /// loop's in-flight foreground tool (`false`). A **property, not an identity**:
    /// the foreground→background handoff flips it in place
    /// (docs/supervisor.md).
    background: bool,
    /// The child's process-group id (== child pid; it leads a fresh group), for
    /// `killpg` teardown. `None` only if the OS didn't report a pid.
    pgid: Option<u32>,
    buffer: Vec<u8>,
    /// Bytes dropped off the front of `buffer` at the cap — surfaced so truncation
    /// is never silent.
    dropped: usize,
    spawned_at: Instant,
    last_chunk_at: Instant,
    /// The same event as `spawned_at`, on the wall clock. Two clocks, two jobs: the
    /// [`Instant`] drives the elapsed figures the *model* reads (monotonic, so an NTP
    /// step can't make a process look younger), and this absolute stamp is what the
    /// control API's `processes` topic carries so clients compute elapsed themselves
    /// (docs/control-api.md).
    spawned_wall: crate::log::Timestamp,
    /// Muted (the supervisor's `dismiss` for a live process, docs/supervisor.md): kept running, exit still
    /// notifies, but the running tail stops surfacing in the peek.
    muted: bool,
    running: bool,
}

/// A live snapshot of one running process for the peek's ambient `background`
/// section (docs/supervisor.md). Owned (it borrows nothing of the registry) so the loop can
/// build it under the lock and hand it to the fork distiller, which then awaits.
///
/// **Model-facing, and that is why it looks like this.** A peek is synchronous and
/// point-in-time — the model has no connection to subscribe to — so the tail must be
/// inline or it does not exist, and the times are humanized elapsed counts because
/// absolute Unix millis are unreadable in a prompt. The control API's view of the same
/// processes is [`ProcessState`], which diverges on both fields for the opposite reason
/// (docs/control-api.md). One registry, two genuinely different consumers.
#[derive(Clone, Debug)]
pub struct BgView {
    pub handle: String,
    pub command: String,
    pub tail: String,
    pub running_secs: u64,
    pub idle_secs: u64,
}

/// One running process as `GET /processes` and the `processes` state topic carry it —
/// one representation for both, so the level-triggered topic and the resource listing
/// cannot drift (docs/control-api.md).
///
/// **What is absent is the design.** No `tail` and no last-chunk stamp: both change with
/// every output chunk, and a level-triggered topic that changes per chunk re-emits a
/// whole-topic replacement per chunk — worse than the poll it replaces. That content
/// rides the ephemeral `output` class as `process_output` deltas instead, where
/// high-rate and lossy is the contract; a client opening cold seeds itself from
/// `GET /processes/:id/output` rather than from this topic.
///
/// What remains changes only when a process spawns, exits, is muted, or is
/// backgrounded, which is what makes `?include=state:processes` viable on a
/// battery-powered client.
#[derive(Clone, Debug, Serialize)]
pub struct ProcessState {
    /// The spawn-minted ordinal (rendered `p#<n>` in prose; bare here and in routes).
    pub id: u64,
    pub command: String,
    /// The OS pid — an *attribute*, never the identifier (pids are recycled; the log
    /// is durable, docs/supervisor.md). Carried so a client or the
    /// agent can interact with the process directly (signals, `lsof`) when the
    /// supervisor doesn't model what it needs. `None` if the OS didn't report one.
    pub pid: Option<u32>,
    /// Absolute Unix millis. The client subtracts from its own clock; the daemon never
    /// re-emits merely because time passed.
    pub spawned_at: crate::log::Timestamp,
    /// `false` while the process is the loop's in-flight foreground tool; flipped in
    /// place by the handoff (docs/supervisor.md).
    pub background: bool,
    /// Dismissed-from-view (still running, still notifies on exit). Carried rather
    /// than filtered so the listing is the whole truth; clients that hide muted rows
    /// do so themselves.
    pub muted: bool,
}

/// One cursor read of a process's retained output — the structured core shared by the
/// model's `read_process` prose ([`Supervisor::read_since`]) and
/// `GET /processes/:id/output` (docs/control-api.md).
pub struct OutputRead {
    /// The retained bytes from the caller's cursor to the end, lossily decoded.
    /// Empty when the cursor is already at the end.
    pub content: String,
    /// The logical start of `content` — the caller's cursor, clamped forward past
    /// anything dropped at the cap.
    pub from: u64,
    /// Pass back as the next cursor to resume after what this read returned.
    pub cursor: u64,
    /// Bytes between the caller's cursor and the earliest retained byte — dropped at
    /// the cap, reported rather than silently skipped.
    pub lost: u64,
    pub running: bool,
}

/// The ephemeral background-process registry (docs/supervisor.md). Held behind a `Mutex` in
/// [`Shared`](crate::daemon); never logged and never projected, which is what keeps it
/// clear of the cache prefix. (Its stamps *do* reach the control wire — see
/// [`ProcessState`] — which is harmless: the control API and the projection are
/// different wires, and only the projection has to stay byte-stable.)
#[derive(Default)]
pub struct Supervisor {
    next: u64,
    procs: Vec<Process>,
}

impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry whose mint counter starts past every ordinal the resumed log
    /// already mentions (see [`max_handle_mention`]).
    pub fn with_minted(next: u64) -> Self {
        Self {
            next,
            procs: Vec::new(),
        }
    }

    /// Register a freshly-spawned *background* child and mint its id. The caller (the
    /// daemon) spawns the drain task that feeds [`push_chunk`](Self::push_chunk) and
    /// ends in [`finish`](Self::finish).
    pub fn register_background(&mut self, command: String, pgid: Option<u32>) -> u64 {
        self.mint(command, pgid, true)
    }

    /// Register the loop's freshly-spawned *foreground* command. Same namespace, same
    /// mint (docs/supervisor.md): the id exists from spawn, so a
    /// client can address the process — and the handoff is a property change on it —
    /// without a provisional identity. The foreground path feeds
    /// [`push_chunk`](Self::push_chunk) as it drains, and either
    /// [`remove`](Self::remove)s the entry when the call completes (the `tool_result`
    /// is the canonical output) or [`make_background`](Self::make_background)s it on
    /// detach.
    pub fn register_foreground(&mut self, command: String, pgid: Option<u32>) -> u64 {
        self.mint(command, pgid, false)
    }

    fn mint(&mut self, command: String, pgid: Option<u32>, background: bool) -> u64 {
        self.next += 1;
        let id = self.next;
        let now = Instant::now();
        self.procs.push(Process {
            id,
            command,
            background,
            pgid,
            buffer: Vec::new(),
            dropped: 0,
            spawned_at: now,
            last_chunk_at: now,
            spawned_wall: crate::log::Timestamp::now(),
            muted: false,
            running: true,
        });
        id
    }

    /// The foreground→background handoff, on the registry: flip the property in
    /// place (docs/supervisor.md). The id, the capture buffer, and every cursor a
    /// reader holds survive unchanged — which is what makes backgrounding a `PATCH`
    /// rather than the appearance of a new resource. `false` if the id is unknown.
    pub fn make_background(&mut self, id: u64) -> bool {
        match self.find_mut(id) {
            Some(p) => {
                p.background = true;
                true
            }
            None => false,
        }
    }

    /// Drop a completed *foreground* entry: its canonical output is the paired
    /// `tool_result`, and retaining every finished foreground command would grow the
    /// registry with the session. (Background entries instead stay through
    /// [`finish`](Self::finish) so cursor reads survive the exit.)
    pub fn remove(&mut self, id: u64) {
        self.procs.retain(|p| p.id != id);
    }

    /// Append a freshly-read output chunk to a process's bounded capture and stamp
    /// its activity clock (the source of the peek's `idle` figure).
    pub fn push_chunk(&mut self, id: u64, chunk: &[u8]) {
        if let Some(p) = self.find_mut(id) {
            p.buffer.extend_from_slice(chunk);
            if p.buffer.len() > BUFFER_CAP {
                let excess = p.buffer.len() - BUFFER_CAP;
                p.buffer.drain(..excess);
                p.dropped += excess;
            }
            p.last_chunk_at = Instant::now();
        }
    }

    /// Set a process's muted flag (the supervisor's `dismiss` for a live process,
    /// docs/supervisor.md — and its `PATCH`-able inverse).
    pub fn set_muted(&mut self, id: u64, muted: bool) -> bool {
        match self.find_mut(id) {
            Some(p) => {
                p.muted = muted;
                true
            }
            None => false,
        }
    }

    /// Every *running* process, muted and foreground included, flagged rather than
    /// filtered — the one representation `GET /processes` serves and the `processes`
    /// state topic carries (docs/control-api.md). A process leaving the list is how
    /// an exit is represented, so the list cannot grow without bound over a long
    /// session.
    pub fn process_states(&self) -> Vec<ProcessState> {
        self.procs
            .iter()
            .filter(|p| p.running)
            .map(state_of)
            .collect()
    }

    /// One *running* process's representation, or `None` — the `GET`/`PATCH`
    /// target check for `/processes/:id` (docs/control-api.md). Exited entries
    /// answer `None` here (they have left the listing) while their retained output
    /// stays readable through [`read_output`](Self::read_output).
    pub fn running_state(&self, id: u64) -> Option<ProcessState> {
        self.procs
            .iter()
            .find(|p| p.id == id && p.running)
            .map(state_of)
    }

    /// The ambient auto-show: a snapshot of every *running, un-muted background*
    /// process for the peek's `background` section (docs/supervisor.md). Foreground
    /// entries are excluded — the model reads a peek between calls, when its own
    /// in-flight command is not a thing to report to it.
    pub fn views(&self) -> Vec<BgView> {
        let now = Instant::now();
        self.procs
            .iter()
            .filter(|p| p.running && p.background && !p.muted)
            .map(|p| BgView {
                handle: handle(p.id),
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
    /// id is unknown.
    pub fn pin_snapshot(&self, id: u64) -> Option<(String, String)> {
        let p = self.find(id)?;
        let running = Instant::now().saturating_duration_since(p.spawned_at);
        let body = format!(
            "{} · running {}\n{}",
            p.command,
            humanize(running),
            tail_text(&p.buffer, p.dropped)
        );
        Some((handle(p.id), body))
    }

    /// Mark a process exited and produce its exit notification `(source, body)` —
    /// status + duration + final tail (docs/supervisor.md, requirement #4). `None` if the
    /// id is unknown.
    pub fn finish(&mut self, id: u64, status: Option<ExitStatus>) -> Option<(String, String)> {
        let ran = {
            let p = self.find(id)?;
            Instant::now().saturating_duration_since(p.spawned_at)
        };
        let p = self.find_mut(id)?;
        p.running = false;
        let body = format!(
            "{} after {}; final output:\n{}",
            describe_status(status),
            humanize(ran),
            tail_text(&p.buffer, p.dropped)
        );
        Some((handle(p.id), body))
    }

    /// The directed `list_processes` view (docs/supervisor.md): all *running background*
    /// processes — muted ones included and annotated, since the model asked explicitly
    /// (the ambient `views()` auto-show hides muted; this doesn't). Exited ones have
    /// already notified, so they drop off; a foreground entry is the model's own
    /// in-flight command, never listed to it.
    pub fn list_text(&self) -> String {
        let running: Vec<&Process> = self
            .procs
            .iter()
            .filter(|p| p.running && p.background)
            .collect();
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
                handle(p.id),
                p.command
            ));
        }
        out
    }

    /// One cursor read of a process's retained output, structured — the shared core
    /// of the model's `read_process` prose ([`read_since`](Self::read_since)) and
    /// `GET /processes/:id/output` (docs/control-api.md). `cursor` is a byte offset
    /// into the *logical* stream; `None` reads from the earliest still-retained byte —
    /// which is what seeds a cold-opened client's tail. Bytes dropped off the front at
    /// the cap (and thus before the cursor) are counted in `lost`, not silently
    /// skipped. `None` if the id is unknown.
    pub fn read_output(&self, id: u64, cursor: Option<u64>) -> Option<OutputRead> {
        let p = self.find(id)?;
        let dropped = p.dropped as u64;
        let total = dropped + p.buffer.len() as u64;
        // Can't read before what we still hold; track anything lost before the cursor.
        let from = cursor.unwrap_or(dropped).max(dropped);
        let lost = match cursor {
            Some(c) if c < dropped => dropped - c,
            _ => 0,
        };
        let content = match from < total {
            true => String::from_utf8_lossy(&p.buffer[(from - dropped) as usize..]).into_owned(),
            false => String::new(),
        };
        Some(OutputRead {
            content,
            from,
            cursor: total,
            lost,
            running: p.running,
        })
    }

    /// Cursor-based tailing of one process's output as `read_process` prose
    /// (docs/supervisor.md) — [`read_output`](Self::read_output) rendered for the
    /// model. `None` if the id is unknown.
    pub fn read_since(&self, id: u64, cursor: Option<u64>) -> Option<String> {
        let read = self.read_output(id, cursor)?;
        let p = self.find(id)?;
        let handle = handle(p.id);
        let status = if read.running {
            format!(
                "running · idle {}",
                humanize(Instant::now().saturating_duration_since(p.last_chunk_at))
            )
        } else {
            "exited".to_string()
        };
        let OutputRead {
            content,
            from,
            cursor,
            lost,
            ..
        } = read;
        if content.is_empty() {
            return Some(format!(
                "{handle}: no new output since cursor {from} ({status}). cursor: {cursor}"
            ));
        }
        let mut out = String::new();
        if lost > 0 {
            out.push_str(&format!(
                "… [{lost} bytes before your cursor were dropped at the cap]\n"
            ));
        }
        out.push_str(&format!(
            "{handle} output [bytes {from}–{cursor}] ({status}):\n"
        ));
        out.push_str(&content);
        out.push_str(&format!("\ncursor: {cursor}"));
        Some(out)
    }

    /// The pgid of a *running* process by id — the lookup `kill_process` and the
    /// daemon's `kill_bg` use. `None` if unknown or already exited.
    pub fn pgid_of(&self, id: u64) -> Option<u32> {
        self.procs
            .iter()
            .find(|p| p.id == id && p.running)
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

    fn find(&self, id: u64) -> Option<&Process> {
        self.procs.iter().find(|p| p.id == id)
    }
    fn find_mut(&mut self, id: u64) -> Option<&mut Process> {
        self.procs.iter_mut().find(|p| p.id == id)
    }
}

/// One entry's wire representation — shared by the listing and the single-resource
/// read so they cannot drift.
fn state_of(p: &Process) -> ProcessState {
    ProcessState {
        id: p.id,
        command: p.command.clone(),
        pid: p.pgid,
        spawned_at: p.spawned_wall,
        background: p.background,
        muted: p.muted,
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
mod test;
