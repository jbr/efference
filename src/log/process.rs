//! The durable half of a background process's record (docs/process-monitor.md).
//!
//! The process monitor's registry is ephemeral — it dies with the daemon — but a
//! `p#<n>` handle is minted into prose the moment a command is backgrounded, and
//! that prose outlives the registry in the log. These entries are what a later
//! reader resolves the handle against: a **mint** record when a process enters
//! the background namespace, an **exit** record when it ends. Together they carry
//! the metadata half of the promise (handle, spawning call, command, spawn time,
//! exit status, duration); the *output* half stays the registry's bounded,
//! best-effort capture and is not recorded here.
//!
//! Both halves carry their own wall stamp rather than leaning on the entry's `at`.
//! The mint has to: a process adopted by the foreground→background handoff is
//! recorded when it is adopted, which is not when it spawned. The exit follows the
//! mint so the two read the same way.
//!
//! None of this reaches the wire — a [`Payload::ProcessEvent`] projects to
//! nothing, like [`Telemetry::Tool`] (docs/log-and-projection.md) — so the
//! shape here cannot move the cached prefix.
//!
//! [`Payload::ProcessEvent`]: crate::log::Payload::ProcessEvent
//! [`Telemetry::Tool`]: crate::log::Telemetry::Tool

use serde::{Deserialize, Serialize};

use super::{CallId, Timestamp};

/// One lifecycle fact about one background process, addressed by the
/// spawn-minted ordinal (docs/process-monitor.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessEvent {
    /// The spawn-minted ordinal — bare, as routes and frames carry it; rendered
    /// `p#<n>` in prose by [`crate::process_monitor::handle`].
    pub id: u64,
    pub kind: ProcessEventKind,
}

/// The two halves of the record. Two variants of one payload rather than two
/// payloads: they share the id that joins them, and a reader folding a log for
/// "what was `p#3`" wants both under one match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessEventKind {
    /// The process entered the background namespace — spawned with `background:
    /// true`, or adopted by the handoff (docs/process-monitor.md).
    Spawned(ProcessSpawn),
    /// The process ended. Written by the drain task that saw the exit.
    Exited(ProcessExit),
}

/// What a process was, recorded at the moment its handle became a fact a reader
/// can encounter (docs/process-monitor.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSpawn {
    pub command: String,
    /// The `tool_use` id of the call that spawned it — the call↔process mapping
    /// (docs/process-monitor.md), here in a typed field rather than recoverable only
    /// from result prose. `None` for a process registered outside a tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<CallId>,
    /// The OS pid (== the process-group id). An attribute, never the identity:
    /// pids are recycled, so a recorded one is a breadcrumb for reading old
    /// system logs, not something a later session can address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// When the process **spawned** — not when this entry was appended, which
    /// differ for an adopted foreground command.
    pub at: Timestamp,
}

/// How a process ended and how long it ran (docs/process-monitor.md). Also the
/// exit half of a [`ProcessRecord`](crate::process_monitor::ProcessRecord), so the
/// live registry and a log fold answer with one type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExit {
    pub outcome: ExitOutcome,
    /// Wall-clock milliseconds from spawn to exit.
    pub duration_ms: u64,
    /// When the exit was observed.
    pub at: Timestamp,
}

/// How a process ended (docs/process-monitor.md) — the structured form of the prose
/// the exit notification carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExitOutcome {
    /// Exited normally with this status code.
    Code(i32),
    /// Terminated by a signal, which reports no code — `kill_process`, the
    /// teardown sweep, or a kill from outside the harness.
    Signal,
    /// The OS reported no status at all (the wait itself failed). Distinct from
    /// a signal: nothing is known, rather than something specific.
    Unknown,
}

impl ExitOutcome {
    /// Classify a `wait` result. `None` is the wait that failed.
    pub fn of(status: Option<std::process::ExitStatus>) -> Self {
        match status {
            Some(status) => match status.code() {
                Some(code) => Self::Code(code),
                None => Self::Signal,
            },
            None => Self::Unknown,
        }
    }
}
