//! The background-process process monitor (docs/process-monitor.md).
//!
//! A backgrounded command is a *live entity*, not a log record: its running tail is
//! re-sampled fresh at each triage, never frozen into the log (forcing a live
//! tail through the immutable notification model would pile up stale snapshots,
//! docs/process-monitor.md). So this registry is **ephemeral daemon state** — a sibling of the
//! inbox queue and the cache `Metrics` in [`Session`](crate::daemon): dropped on
//! restart, where killpg-on-teardown reaps the children it owns.
//!
//! This module holds the state: the registry, its one bounded per-process
//! capture buffer, and the read projections over it — [`ProcessMonitor::views`] (the
//! ambient auto-show the triage renders each triage), [`ProcessMonitor::pin_snapshot`] (a
//! frozen tail when the model pins), and [`ProcessMonitor::finish`] (the exit
//! notification). The async spawn/drain orchestration — which needs `Arc<Session>`
//! and the `POST /notifications` path — lives in `daemon.rs`.

use std::process::ExitStatus;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::log::ASK;

mod record;
pub use record::{ProcessRecord, from_log};

mod terminal;
pub use terminal::strip_formatting;

mod tools;
pub use tools::{brief, tool_definitions};

/// `list_processes` — the directed read of running processes (docs/process-monitor.md). Named to
/// avoid colliding with fs-mcp's `list`.
pub const LIST: &str = "list_processes";
/// `read_process` — cursor-based tailing of one process's output (the process monitor's
/// `read_since`, docs/process-monitor.md). Named to avoid colliding with fs-mcp's `read`.
pub const READ: &str = "read_process";
/// `kill_process` — terminate a background process's group (docs/process-monitor.md).
pub const KILL: &str = "kill_process";
/// `watch_process` — register a one-shot output pattern that notifies on a hit
/// (docs/process-monitor.md). Named beside `read_process`/`kill_process`; a bare `watch`
/// would read as a filesystem watcher.
pub const WATCH: &str = "watch_process";
/// `check_inbox` — surface pending notifications on demand (the directed inbox read,
/// docs/inbox.md) so the model needn't wait for them to ride its next action.
pub const CHECK_INBOX: &str = "check_inbox";
/// `wait` — block until a new inbox notification arrives, then surface pending
/// notifications (docs/process-monitor.md). Refuses when no known source can wake it.
pub const WAIT: &str = "wait";
/// The question an `ask` call puts and its place among the asks of one completion,
/// `n` of `of` (1-based), for a surface that draws it as prose rather than as a call
/// (docs/tui.md, docs/web-client.md). `None` for a call that is not an ask, or one
/// with no usable `question` — which the loop answers with an error result and never
/// puts to the operator.
pub fn ask_of_call(calls: &[crate::log::ToolCall], index: usize) -> Option<(String, usize, usize)> {
    let is_ask = |c: &&crate::log::ToolCall| c.name == ASK;
    let call = calls.get(index).filter(is_ask)?;
    let question = call.input.get("question")?.as_str()?.trim();
    (!question.is_empty()).then(|| {
        (
            question.to_string(),
            calls[..index].iter().filter(is_ask).count() + 1,
            calls.iter().filter(is_ask).count(),
        )
    })
}

/// Render a process id as its prose handle — the form the model and every human
/// transcript sees (`p#3`). The bare ordinal is what routes and frames carry
/// (docs/process-monitor.md): the prefix does its disambiguating work only where the id
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
/// (docs/process-monitor.md). Over-matching is safe (a skipped ordinal
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

/// Lines of tail shown per process in the triage (and carried in a pin / exit
/// snapshot). A tuned constant (docs/process-monitor.md).
const TAIL_LINES: usize = 12;

/// Bounded per-process capture. Output past this is dropped from the *front*
/// (oldest-first) with a counted breadcrumb — no silent caps (docs/process-monitor.md).
const BUFFER_CAP: usize = 256 * 1024;

/// One backgrounded process. `buffer` is the single bounded capture both read
/// projections derive from — the tail view and the cursor [`read_since`].
///
/// [`read_since`]: ProcessMonitor::read_since
struct Process {
    /// The spawn-minted ordinal — the durable identity that survives every property
    /// change (docs/process-monitor.md). Rendered `p#<n>` in prose by
    /// [`handle`]; carried bare in routes and frames.
    id: u64,
    command: String,
    /// The `tool_use` id of the transcript call that spawned this process — the
    /// call↔process mapping (docs/process-monitor.md). Stamped at the mint, so it
    /// survives the handoff flip like every other property. `None` for a process
    /// registered outside a tool call (nothing does today; the type doesn't claim
    /// otherwise).
    call_id: Option<String>,
    /// Whether the process runs detached under the process monitor (`true`) or is the
    /// loop's in-flight foreground tool (`false`). A **property, not an identity**:
    /// the foreground→background handoff flips it in place
    /// (docs/process-monitor.md).
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
    /// Muted (the process monitor's `dismiss` for a live process, docs/process-monitor.md): kept running, exit still
    /// notifies, but the running tail stops surfacing in the triage.
    muted: bool,
    running: bool,
    /// How this process ended, stamped by [`ProcessMonitor::finish`] — the same
    /// record the log's exit entry carries, kept here so a live daemon answers
    /// `GET /processes/:id` for an exited process without re-reading the log
    /// (docs/process-monitor.md). `None` while it runs.
    exit: Option<crate::log::ProcessExit>,
    /// Registered output patterns awaiting a hit (docs/process-monitor.md). Each fires
    /// once and is removed; an entry that never matches dies with the process.
    watches: Vec<String>,
    /// How far into the *logical* stream (total bytes ever received, dropped
    /// included) the watches have been scanned. Advanced by
    /// [`push_chunk`](ProcessMonitor::push_chunk) whether or not any watch is registered,
    /// which is what makes a watch forward-only: output already printed when the model
    /// registers is already behind this mark.
    scanned: u64,
}

/// A live snapshot of one running process for the triage's ambient `background`
/// section (docs/process-monitor.md). Owned (it borrows nothing of the registry) so the loop can
/// build it under the lock and hand it to the triager, which then awaits.
///
/// Model-facing: the tail is inline and the times are humanized elapsed counts.
/// [`ProcessState`] is the control API's view of the same registry and diverges on
/// both fields for the mirror-image reason (docs/process-monitor.md).
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
/// No `tail` and no last-chunk stamp: both change with every output chunk, and this
/// topic is level-triggered, so carrying them would emit a whole-topic replacement per
/// chunk. That content rides the ephemeral `output` class as `process_output` deltas
/// instead (docs/process-monitor.md). What remains changes only on spawn, exit, mute, or the
/// handoff flip.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessState {
    /// The spawn-minted ordinal (rendered `p#<n>` in prose; bare here and in routes).
    pub id: u64,
    pub command: String,
    /// The `tool_use` id of the transcript call this process came from — the
    /// call↔process mapping (docs/process-monitor.md). A client holding a transcript
    /// entry finds its process here (and its output at
    /// `GET /processes/:id/output`); a client holding a process row finds the
    /// entry to jump to. Stamped at the mint, so a call that was backgrounded
    /// mid-run carries the same id it did in the foreground.
    pub call_id: Option<String>,
    /// The OS pid — an *attribute*, never the identifier (pids are recycled; the log
    /// is durable, docs/process-monitor.md). Carried so a client or the
    /// agent can interact with the process directly (signals, `lsof`) when the
    /// process monitor doesn't model what it needs. `None` if the OS didn't report one.
    pub pid: Option<u32>,
    /// Absolute Unix millis. The client subtracts from its own clock; the daemon never
    /// re-emits merely because time passed.
    pub spawned_at: crate::log::Timestamp,
    /// `false` while the process is the loop's in-flight foreground tool; flipped in
    /// place by the handoff (docs/process-monitor.md).
    pub background: bool,
    /// Dismissed-from-view (still running, still notifies on exit). Carried rather
    /// than filtered so the listing is the whole truth; clients that hide muted rows
    /// do so themselves.
    pub muted: bool,
}

/// What one process's exit produced (docs/process-monitor.md): the durable record for
/// the log, and the `(source, body)` of the notification for the inbox. One
/// return rather than two lookups — the drain task takes the registry lock once.
#[derive(Clone, Debug)]
pub struct Finished {
    /// The exit as the log records it — status, duration, and when.
    pub exit: crate::log::ProcessExit,
    /// The notification's source: the plain `p#<n>` handle.
    pub source: String,
    /// The notification's body: status, duration, and the final tail.
    pub body: String,
}

/// One fired watch (docs/process-monitor.md), ready to become a
/// [`Notification`](crate::log::Notification). Produced under the registry lock by
/// [`ProcessMonitor::push_chunk`] and posted by the caller once the lock is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchHit {
    /// The prose handle of the process that printed the line (`p#3`).
    pub handle: String,
    /// The pattern that matched — echoed back so the model can tell two watches on
    /// one process apart, and so a `⟢` block that outlives the tool call still says
    /// what was being waited for.
    pub pattern: String,
    /// The matching line, trimmed of its line ending. Truncated at
    /// [`WATCH_LINE_CAP`] with a breadcrumb: a notification body is prose the model
    /// reads whole, and one pathological line must not become the turn.
    pub line: String,
}

impl WatchHit {
    /// The `(source, body)` pair the notification carries. `source` is the plain
    /// handle, as the exit notification's is, so the pinned `⟢ p#3: …` block reads
    /// the same for both kinds of event; the body says which kind it is.
    pub fn notification(&self) -> (String, String) {
        (
            self.handle.clone(),
            format!(
                "matched the watch on `{}`:\n{}\n(that watch has now fired and is done; \
                 watch_process again if you want the next occurrence)",
                self.pattern, self.line
            ),
        )
    }
}

/// Longest matching line a [`WatchHit`] carries verbatim.
const WATCH_LINE_CAP: usize = 2000;

/// Fire and remove every watch whose pattern appears in output this process has not
/// been scanned for yet (docs/process-monitor.md).
///
/// Complete lines are scanned once each; the trailing partial line is scanned too
/// but does **not** advance the mark, so a command whose output has no line ending
/// (a prompt, a progress bar) still matches, and re-scanning it costs nothing
/// because a watch fires at most once. The scan runs whether or not a watch is
/// registered — advancing the mark is what makes a later `watch_process` see only
/// output printed after it was asked for.
fn scan_watches(p: &mut Process) -> Vec<WatchHit> {
    let dropped = p.dropped as u64;
    // Bytes dropped at the cap were scanned before they were dropped; if the cap bit
    // hard enough to pass the mark, they are gone either way.
    let from = p.scanned.max(dropped);
    let watching = !p.watches.is_empty();
    let (complete, text) = {
        let unscanned = &p.buffer[(from - dropped) as usize..];
        let complete = unscanned
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        // Only decoded when something is waiting on it: this runs per chunk of
        // every process, watched or not. Patterns match the text the agent reads, so
        // a colour escape inside a line cannot split a match.
        let text = match watching {
            true => strip_formatting(&String::from_utf8_lossy(unscanned)),
            false => String::new(),
        };
        (complete, text)
    };
    p.scanned = from + complete as u64;
    if !watching {
        return Vec::new();
    }
    let pid = p.id;
    let mut hits = Vec::new();
    p.watches.retain(
        |pattern| match text.lines().find(|line| line.contains(pattern.as_str())) {
            Some(line) => {
                hits.push(WatchHit {
                    handle: handle(pid),
                    pattern: pattern.clone(),
                    line: cap_line(line),
                });
                false
            }
            None => true,
        },
    );
    hits
}

/// A matching line, bounded at [`WATCH_LINE_CAP`] on a char boundary.
fn cap_line(line: &str) -> String {
    if line.len() <= WATCH_LINE_CAP {
        return line.to_string();
    }
    let mut end = WATCH_LINE_CAP;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [line truncated]", &line[..end])
}

/// One cursor read of a process's retained output — the structured core shared by the
/// model's `read_process` prose ([`ProcessMonitor::read_since`]) and
/// `GET /processes/:id/output` (docs/control-api.md).
pub struct OutputRead {
    /// The retained bytes from the caller's cursor to the end, lossily decoded.
    /// Escapes and carriage returns are kept; `read_since` strips them for the model.
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

/// The ephemeral background-process registry (docs/process-monitor.md). Held behind a `Mutex` in
/// [`Session`](crate::daemon); never logged and never projected, which is what keeps it
/// clear of the cache prefix. (Its stamps *do* reach the control wire — see
/// [`ProcessState`] — which is harmless: the control API and the projection are
/// different wires, and only the projection has to stay byte-stable.)
#[derive(Default)]
pub struct ProcessMonitor {
    next: u64,
    procs: Vec<Process>,
}

impl ProcessMonitor {
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
    pub fn register_background(
        &mut self,
        command: String,
        pgid: Option<u32>,
        call_id: Option<String>,
    ) -> u64 {
        self.mint(command, pgid, call_id, true)
    }

    /// Register the loop's freshly-spawned *foreground* command. Same namespace, same
    /// mint (docs/process-monitor.md): the id exists from spawn, so a
    /// client can address the process — and the handoff is a property change on it —
    /// without a provisional identity. The foreground path feeds
    /// [`push_chunk`](Self::push_chunk) as it drains, and either
    /// [`remove`](Self::remove)s the entry when the call completes (the `tool_result`
    /// is the canonical output) or [`make_background`](Self::make_background)s it on
    /// detach.
    pub fn register_foreground(
        &mut self,
        command: String,
        pgid: Option<u32>,
        call_id: Option<String>,
    ) -> u64 {
        self.mint(command, pgid, call_id, false)
    }

    fn mint(
        &mut self,
        command: String,
        pgid: Option<u32>,
        call_id: Option<String>,
        background: bool,
    ) -> u64 {
        self.next += 1;
        let id = self.next;
        let now = Instant::now();
        self.procs.push(Process {
            id,
            command,
            call_id,
            background,
            pgid,
            buffer: Vec::new(),
            dropped: 0,
            spawned_at: now,
            last_chunk_at: now,
            spawned_wall: crate::log::Timestamp::now(),
            muted: false,
            running: true,
            exit: None,
            watches: Vec::new(),
            scanned: 0,
        });
        id
    }

    /// The foreground→background handoff, on the registry: flip the property in
    /// place (docs/process-monitor.md). The id, the capture buffer, and every cursor a
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

    /// Append a freshly-read output chunk to a process's bounded capture, stamp its
    /// activity clock (the source of the triage's `idle` figure), and scan the new
    /// output against any registered watches (docs/process-monitor.md).
    ///
    /// Returns the hits, which the caller turns into notifications *after* dropping
    /// the registry lock — the notify path takes the inbox lock and wakes the loop,
    /// and neither belongs under this one.
    pub fn push_chunk(&mut self, id: u64, chunk: &[u8]) -> Vec<WatchHit> {
        let Some(p) = self.find_mut(id) else {
            return Vec::new();
        };
        p.buffer.extend_from_slice(chunk);
        if p.buffer.len() > BUFFER_CAP {
            let excess = p.buffer.len() - BUFFER_CAP;
            p.buffer.drain(..excess);
            p.dropped += excess;
        }
        p.last_chunk_at = Instant::now();
        scan_watches(p)
    }

    /// Register a one-shot output watch on a running process (docs/process-monitor.md):
    /// the drain scan fires a notification the next time a line of its output
    /// contains `pattern`, and the watch is then removed. `false` if the id is
    /// unknown or the process has already exited — there will be no more output to
    /// match, so an accepted watch would never fire.
    pub fn watch(&mut self, id: u64, pattern: String) -> bool {
        match self.procs.iter_mut().find(|p| p.id == id && p.running) {
            Some(p) => {
                p.watches.push(pattern);
                true
            }
            None => false,
        }
    }

    /// How many watches a process currently has pending — the count the tool result
    /// reports back so the model knows what it is now waiting on.
    pub fn watch_count(&self, id: u64) -> usize {
        self.find(id).map_or(0, |p| p.watches.len())
    }

    /// Set a process's muted flag (the process monitor's `dismiss` for a live process,
    /// docs/process-monitor.md — and its `PATCH`-able inverse).
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

    /// One *running* process's representation, or `None` — the `PATCH`/`DELETE`
    /// target check for `/processes/:id` (docs/control-api.md), where "running"
    /// is what the verb needs. The `GET` reads [`record`](Self::record) instead,
    /// which answers for exited processes too.
    pub fn running_state(&self, id: u64) -> Option<ProcessState> {
        self.procs
            .iter()
            .find(|p| p.id == id && p.running)
            .map(state_of)
    }

    /// One process's full record — running or exited — from the live registry
    /// (docs/process-monitor.md). The richest of the two sources `GET /processes/:id`
    /// consults; the log fold ([`record::from_log`]) answers when this doesn't,
    /// which is every reader of a session this daemon didn't run.
    ///
    /// A completed *foreground* entry is removed from the registry, so it answers
    /// `None` here like any unknown id.
    pub fn record(&self, id: u64) -> Option<ProcessRecord> {
        let p = self.find(id)?;
        Some(ProcessRecord {
            id: p.id,
            command: p.command.clone(),
            call_id: p.call_id.clone(),
            pid: p.pgid,
            spawned_at: p.spawned_wall,
            background: p.background,
            muted: p.muted,
            running: p.running,
            exit: p.exit,
        })
    }

    /// The **mint** half of a process's durable record (docs/process-monitor.md),
    /// built from the live entry: the payload the daemon appends when a process
    /// enters the background namespace — at a background spawn, or at the
    /// handoff that adopts a foreground command. `None` if the id is unknown.
    pub fn spawn_event(&self, id: u64) -> Option<crate::log::ProcessEvent> {
        let p = self.find(id)?;
        Some(crate::log::ProcessEvent {
            id: p.id,
            kind: crate::log::ProcessEventKind::Spawned(crate::log::ProcessSpawn {
                command: p.command.clone(),
                call_id: p.call_id.clone().map(crate::log::CallId),
                pid: p.pgid,
                // The spawn stamp, which for an adopted command is older than the
                // entry this payload lands in.
                at: p.spawned_wall,
            }),
        })
    }

    /// The ambient auto-show: a snapshot of every *running, un-muted background*
    /// process for the triage's `background` section (docs/process-monitor.md). Foreground
    /// entries are excluded — the model reads a triage between calls, when its own
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

    /// Snapshot a running process's current tail for a **pin** (docs/process-monitor.md): the
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

    /// Mark a process exited, stamp its durable exit record, and produce its exit
    /// notification — status + duration + final tail (docs/process-monitor.md).
    /// `None` if the id is unknown.
    pub fn finish(&mut self, id: u64, status: Option<ExitStatus>) -> Option<Finished> {
        let ran = {
            let p = self.find(id)?;
            Instant::now().saturating_duration_since(p.spawned_at)
        };
        let exit = crate::log::ProcessExit {
            outcome: crate::log::ExitOutcome::of(status),
            // Saturating: a duration past u64 milliseconds is unreachable, and a
            // wrapped one would read as a fast process.
            duration_ms: u64::try_from(ran.as_millis()).unwrap_or(u64::MAX),
            at: crate::log::Timestamp::now(),
        };
        let p = self.find_mut(id)?;
        p.running = false;
        p.exit = Some(exit);
        let body = format!(
            "{} after {}; final output:\n{}",
            describe_status(status),
            humanize(ran),
            tail_text(&p.buffer, p.dropped)
        );
        Some(Finished {
            exit,
            source: handle(p.id),
            body,
        })
    }

    /// Whether any background process remains able to produce an inbox notification
    /// (docs/process-monitor.md). Used before the loop-owned `wait` blocks, so it does not
    /// leave a turn open when no monitored event can arrive.
    pub fn has_running_background(&self) -> bool {
        self.procs.iter().any(|p| p.background && p.running)
    }

    /// The directed `list_processes` view (docs/process-monitor.md): all *running background*
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
    /// (docs/process-monitor.md) — [`read_output`](Self::read_output) rendered for the
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
        out.push_str(&strip_formatting(&content));
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
    /// (docs/process-monitor.md). A muted process is still running, so it's included.
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
        call_id: p.call_id.clone(),
        pid: p.pgid,
        spawned_at: p.spawned_wall,
        background: p.background,
        muted: p.muted,
    }
}

/// The last [`TAIL_LINES`] lines of a capture, lossily decoded and stripped of
/// terminal formatting ([`strip_formatting`]), with a leading
/// breadcrumb when content was dropped (off the front at the cap, or just older
/// than the tail window) — no silent truncation (docs/process-monitor.md).
fn tail_text(buffer: &[u8], dropped: usize) -> String {
    let decoded = strip_formatting(&String::from_utf8_lossy(buffer));
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
