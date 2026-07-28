//! The interactive TUI (docs/README.md — the human-facing client).
//!
//! Vendored-and-adapted from ferritin's interactive renderer. The
//! interface ferritin exposed — a general-purpose document IR ([`document`]) rendered
//! by a domain-free pipeline ([`render_node`] et al.) over a syntect theme
//! ([`theme`], [`render_context`]) — carries over almost verbatim. What changed:
//!
//! - The rustdoc shell (Navigator/Request/Commands and the two-thread request
//!   architecture) is gone; the efference daemon *is* the request side. The TUI
//!   subscribes to the daemon's `/events` + `/stream` SSE and projects the log
//!   into a [`document::Document`] via [`projection`], re-projected as events land.
//! - The IR's navigation appendage (rustdoc `Navigate` actions, `Metadata` item
//!   blocks tied to `DocRef`) is stripped; what remains is markdown + code +
//!   collapsible blocks.

// Vendored from ferritin: the IR and theme modules keep a full constructor /
// helper surface (Span builders, node convenience ctors, …) that efference does
// not exercise yet but that keeps the code shaped like upstream for backporting.
#![allow(dead_code)]

pub(crate) mod block_menu;
pub(crate) mod color_scheme;
pub(crate) mod composer;
pub(crate) mod control_strip;
pub(crate) mod document;
pub(crate) mod events;
pub(crate) mod gallery;
pub(crate) mod grid;
#[cfg(test)]
pub(crate) mod harness;
pub(crate) mod keyboard;
pub(crate) mod markdown;
pub(crate) mod mouse;
pub(crate) mod projection;
pub(crate) mod render_code_block;
pub(crate) mod render_context;
pub(crate) mod render_document;
pub(crate) mod render_frame;
pub(crate) mod render_help_screen;
pub(crate) mod render_loading_bar;
pub(crate) mod render_node;
pub(crate) mod render_scrollbar;
pub(crate) mod render_sidebar;
pub(crate) mod render_span;
pub(crate) mod render_status_bar;
pub(crate) mod render_table;
pub(crate) mod render_theme_picker;
pub(crate) mod span_style;
pub(crate) mod state;
pub(crate) mod table_layout;
pub(crate) mod theme;
pub(crate) mod utils;
pub(crate) mod write_text;

// The render pipeline and input handlers reference these via `super::` (i.e. the
// `tui` module namespace), mirroring ferritin's `renderer::interactive` layout.
// Private re-export: visible to the `tui` submodules, not the wider crate.
use state::{InteractiveState, UiMode};

/// One inbound message to the UI thread, bridged from the daemon's SSE streams
/// by the tokio side (see `main.rs`). Durable [`Entry`](TuiEvent::Entry) values
/// come from `/events` (a serialized [`LogEntry`](crate::log::LogEntry)); the
/// ephemeral deltas come from `/stream` and are folded into the live tail until
/// their committed entry lands.
#[derive(Debug)]
pub(crate) enum TuiEvent {
    /// A committed log entry (from the durable `/events` tail).
    Entry(crate::log::LogEntry),
    /// A fragment of in-progress assistant prose (`/stream` `assistant_delta`).
    AssistantDelta(String),
    /// A fragment of in-progress extended thinking (`/stream` `thinking_delta`). Kept distinct
    /// from [`AssistantDelta`](Self::AssistantDelta) so it can never be rendered as the
    /// assistant's answer.
    ThinkingDelta(String),
    /// The model began emitting a tool call (`/stream` `tool_call_start`), named before its
    /// arguments arrive.
    ToolCallStart { name: String },
    /// A fragment of a tool call's input arriving (`/stream` `tool_call_input`), verbatim. Shown
    /// as text, never parsed — it is a partial slice of a JSON document.
    ToolCallInput { fragment: String },
    /// A fragment of a running tool's output (`/stream` `tool_output`).
    ToolOutput { call_id: String, chunk: String },
    /// A fragment of a running background process's output (`/stream` `bg_output`,
    /// TUI-bg-2). Folded into the matching process panel row's tail to smooth it
    /// between the ~400ms [`Processes`](TuiEvent::Processes) polls; demuxed by
    /// `handle` exactly as [`ToolOutput`](TuiEvent::ToolOutput) is by `call_id`.
    BgOutput { handle: String, chunk: String },
    /// The pending tool-approval gate changed (polled from `GET /state`, since the
    /// gate is ephemeral control state with no SSE event — docs/README.md parks
    /// "pending-approval as synthetic `/events`" for later).
    Gate(Option<PendingGate>),
    /// The live background processes changed (polled from `GET /state` alongside the
    /// gate, docs/supervisor.md — the running tail is ephemeral, never logged, so it has no
    /// `/events` entry; the process panel is fed by the poll). Smooth `/stream`
    /// `bg_output` deltas layer on top between polls (TUI-bg-2, [`BgOutput`](TuiEvent::BgOutput)).
    Processes(Vec<BgProc>),
    /// The usage statistics changed (polled from `GET /state` alongside the gate
    /// and processes). Reflects cumulative token counts and cache telemetry.
    Usage(UsageStats),
    /// The effective policy + its pending proposals changed (polled from `GET /state`'s
    /// `role` + `proposals`). Drives the sidebar's POLICY section (docs/permissions.md).
    Policy(PolicyView),
    /// The operator's pace dial / hold overlay changed (polled from `GET /state`'s `pace`,
    /// docs/tui.md). Loud to the operator (surfaced in the status bar),
    /// silent to the agent — it is never logged or projected.
    Pace(PaceView),
    /// Whether the loop is executing a turn (polled from `GET /state`'s `running`). The
    /// "is something happening" signal graduated Esc reads to choose pause-vs-swallow
    /// (docs/tui.md) — reliable through a silent tool, unlike streaming deltas.
    Running(bool),
    /// The event source closed (daemon gone) → the UI should exit.
    Disconnected,
}

/// The operator's pace state, as surfaced by `GET /state`'s `pace`
/// (docs/tui.md). `speed_ms` is the between-turns delay; `held` is the
/// pause overlay. Operator-view chrome — never part of the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaceView {
    pub speed_ms: u64,
    pub held: bool,
}

/// A pace command the operator issues from the TUI (docs/tui.md), sent to
/// the tokio side which `POST`s it to `/pace`: `(speed_ms, held)`, each optional so a key can move
/// the speed dial, toggle the hold, or both.
pub(crate) type PaceCmd = (Option<u64>, Option<bool>);

/// One live background process, as surfaced by `GET /state`'s `processes` array
/// (the serialized `supervisor::BgView`, docs/supervisor.md). `Serialize` too so the poller
/// can fingerprint the list and only emit on change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BgProc {
    pub handle: String,
    pub command: String,
    pub tail: String,
    pub running_secs: u64,
    pub idle_secs: u64,
}

/// A pending tool-approval gate, as surfaced by `GET /state`'s `pending_approval`.
#[derive(Debug, Clone)]
pub(crate) struct PendingGate {
    pub call_id: String,
    pub name: String,
    pub input: serde_json::Value,
    /// The spotter's one-line reason for holding this call, if it gated via the spotter
    /// (docs/permissions.md) — surfaced inline at the gate so the human sees *why*.
    pub reason: Option<String>,
}
/// Usage statistics from the daemon, as surfaced by `GET /state`'s `usage` field.
/// Polled alongside the gate and processes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct UsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub seconds_since_last_request: Option<u64>,
}

/// One line in the sidebar's POLICY section: a committed grant, or a *proposed* one
/// (rendered tentatively — see [`PolicyView`]). A proposed entry carries the proposal's
/// `seq` so the sidebar can commit/cancel it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyEntry {
    pub label: String,
    pub proposed: bool,
    pub seq: Option<u64>,
}

/// The effective policy and its pending proposals, from `GET /state`'s `role` +
/// `proposals` (docs/permissions.md). On-thesis: a proposal is a projection of its
/// committed self, so it's merged into its section *in sorted position* and flagged
/// `proposed` — committing it drops the flag in place, no reflow.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PolicyView {
    pub scope: Option<String>,
    /// A proposed *replacement* scope (a `Scope` proposal), shown tentatively under the
    /// committed one; `(seq, text)`.
    pub scope_proposal: Option<(u64, String)>,
    pub commands: Vec<PolicyEntry>,
    pub paths: Vec<PolicyEntry>,
    pub tools: Vec<PolicyEntry>,
    /// The review-level dial (docs/tui.md) from `/state`'s
    /// `review_level` — who reviews a gated call (`autonomous` / `spotter_human` / `human_only`).
    /// Read-only here; the interactive gear selector is deferred to the ergonomics pass.
    pub review_level: Option<String>,
}

use std::io::{self, stdout};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, select};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_lite::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::client::Client;
use crate::log::LogEntry;
use render_context::{OutputMode, RenderContext};
use theme::InteractiveTheme;
use utils::set_cursor_shape;

/// Bullet characters for list items at different nesting levels.
/// Cycles through these as lists nest deeper. (Vendored from ferritin's
/// `renderer::mod`, which lived above the interactive renderer.)
const LIST_BULLETS: &[char] = &['◦', '▪', '•', '‣', '⁃'];

/// Get the bullet character for a given indentation level.
///
/// The indent is the column position; each nesting level adds ~4 columns
/// (2 spaces + bullet + space).
pub(crate) fn bullet_for_indent(indent: u16) -> char {
    let nesting_level = (indent / 4) as usize;
    LIST_BULLETS[nesting_level % LIST_BULLETS.len()]
}

#[cfg(test)]
mod test;

mod bridge;
mod parse;
mod run;
mod ui_loop;

pub use run::{run, run_gallery, run_view};

use bridge::bridge;
use parse::*;
use run::DEFAULT_THEME;
use ui_loop::{Senders, ui_loop};
