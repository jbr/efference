//! The interactive TUI (docs/README.md — the human-facing client).
//!
//! Vendored-and-adapted from ferritin's interactive renderer. The
//! interface ferritin exposed — a general-purpose document IR ([`document`]) rendered
//! by a domain-free pipeline ([`render_node`] et al.) over a syntect theme
//! ([`theme`], [`render_context`]) — carries over almost verbatim. What changed:
//!
//! - The rustdoc shell (Navigator/Request/Commands and the two-thread request
//!   architecture) is gone; the efference daemon *is* the request side. The TUI
//!   subscribes to the daemon's `/events` control stream and projects the log
//!   into a [`document::Document`] via [`projection`], re-projected as events land.
//! - The IR's navigation appendage (rustdoc `Navigate` actions, `Metadata` item
//!   blocks tied to `DocRef`) is stripped; what remains is markdown + code +
//!   collapsible blocks.

// Vendored from ferritin: the IR and theme modules keep a full constructor /
// helper surface (Span builders, node convenience ctors, …) that efference does
// not exercise yet but that keeps the code shaped like upstream for backporting.
#![allow(dead_code)]

pub(crate) mod block_menu;
pub(crate) mod clipboard;
pub(crate) mod color_scheme;
pub(crate) mod composer;
pub(crate) mod control_strip;
pub(crate) mod document;
pub(crate) mod events;
pub(crate) mod frame_cost;
pub(crate) mod gallery;
pub(crate) mod grid;
#[cfg(test)]
pub(crate) mod harness;
pub(crate) mod image;
pub(crate) mod keyboard;
pub(crate) mod markdown;
pub(crate) mod mouse;
pub(crate) mod projection;
pub(crate) mod render_code_block;
pub(crate) mod render_context;
pub(crate) mod render_document;
pub(crate) mod render_frame;
pub(crate) mod render_help_screen;
pub(crate) mod render_image;
pub(crate) mod render_loading_bar;
pub(crate) mod render_node;
pub(crate) mod render_scrollbar;
pub(crate) mod render_sidebar;
pub(crate) mod render_span;
pub(crate) mod render_status_bar;
pub(crate) mod render_table;
pub(crate) mod render_theme_picker;
pub(crate) mod sidebar;
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

/// One inbound message to the UI thread, decoded by the bridge from the unified
/// control stream's frames (docs/control-api.md): durable [`Entry`](TuiEvent::Entry)
/// values from `log` frames, the ephemeral deltas from `output` frames (folded into
/// the live tail until their committed entry lands), and the chrome below from
/// `state` frames — each a whole-topic replacement.
#[derive(Debug)]
pub(crate) enum TuiEvent {
    /// A committed log entry (a `log` frame — the durable class).
    Entry(crate::log::LogEntry),
    /// A fragment of in-progress assistant prose (`assistant_delta`).
    AssistantDelta(String),
    /// A fragment of in-progress extended thinking (`thinking_delta`). Kept distinct
    /// from [`AssistantDelta`](Self::AssistantDelta) so it can never be rendered as the
    /// assistant's answer.
    ThinkingDelta(String),
    /// The model began emitting a tool call (`tool_call_start`), named before its
    /// arguments arrive. `index` is the content block the call's input fragments
    /// will name; `call_id` is the `tool_use` id its result will key on
    /// (docs/control-api.md) — the identity a live block binds to before commit.
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
    },
    /// A fragment of a tool call's input arriving (`tool_call_input`), verbatim, for
    /// the call at content block `index`. Shown as text, never parsed — it is a
    /// partial slice of a JSON document. Scoped by `index` because blocks stream
    /// sequentially: without it, a second call's fragments are indistinguishable
    /// from the first's continuing.
    ToolCallInput { index: usize, fragment: String },
    /// A fragment of a running tool's output (`tool_output`).
    ToolOutput { call_id: String, chunk: String },
    /// A fragment of a running background process's output (`process_output`).
    /// Folded into the matching process panel row's tail — the sole source of tail
    /// content, since the `processes` topic carries structure only; demuxed by
    /// `id` exactly as [`ToolOutput`](TuiEvent::ToolOutput) is by `call_id`.
    ProcessOutput { id: u64, chunk: String },
    /// The pending tool-approval gate changed (the `gate` topic).
    Gate(Option<PendingGate>),
    /// The live background processes changed (the `processes` topic,
    /// docs/supervisor.md — the running tail is ephemeral, never logged, so it has no
    /// `log` entry; [`ProcessOutput`](TuiEvent::ProcessOutput) deltas layer the content
    /// on top).
    Processes(Vec<BgProc>),
    /// The usage statistics changed (the `usage` topic). Reflects cumulative token
    /// counts and cache telemetry.
    Usage(UsageStats),
    /// The last non-success upstream response changed (the `upstream_failure` topic).
    /// Ephemeral transport diagnostic, rendered in the status bar.
    UpstreamFailure(Option<UpstreamFailureView>),
    /// The trajectory-stalled condition changed (the `stall` topic,
    /// docs/transport.md). Level-triggered: `None` once a trajectory starts
    /// again. Status-bar chrome — it is what distinguishes an idle daemon that
    /// finished from one that gave up.
    Stall(Option<StallView>),
    /// A retry in flight changed (the `upstream_retry` topic, docs/transport.md).
    /// Level-triggered: `None` is the all-clear. Ephemeral transport diagnostic,
    /// rendered in the status bar.
    UpstreamRetry(Option<UpstreamRetryView>),
    /// What this session is — the configured model and its working directory (the
    /// `session_identity` topic, docs/control-api.md). Static per session today;
    /// it arrives on the connect snapshot and drives the sidebar's SESSION section.
    SessionIdentity(Option<SessionIdentityView>),

    /// The effective policy + its pending proposals changed (the `role`, `proposals`,
    /// and `review_level` topics, merged by the bridge). Drives the sidebar's POLICY
    /// section (docs/permissions.md).
    Policy(PolicyView),
    /// The operator's pace dial / hold overlay changed (the `pace` topic,
    /// docs/tui.md). Loud to the operator (surfaced in the status bar),
    /// silent to the agent — it is never logged or projected.
    Pace(PaceView),
    /// Whether the loop is executing a turn (the `running` topic). The
    /// "is something happening" signal graduated Esc reads to choose pause-vs-swallow
    /// (docs/tui.md) — reliable through a silent tool, unlike streaming deltas.
    Running(bool),
    /// A result image's bytes came back from the control channel, decoded — or
    /// didn't, with the reason (docs/tui.md). Folded into the
    /// [`ImageStore`](image::ImageStore); the transcript's reservation for it was
    /// sized from metadata and does not change, so this repaints without a
    /// reproject.
    Image {
        seq: u64,
        idx: usize,
        image: Result<::image::DynamicImage, String>,
    },
    /// A one-line note from a control-channel call the UI thread dispatched and
    /// could not await — today the continue gesture's decline (docs/tui.md).
    /// Rendered in the status line and nowhere else: it is operator chrome, never
    /// transcript content.
    Notice(String),
    /// A recall came back: the turns the daemon **actually** withdrew, in submission order
    /// (docs/tui.md). The list can be shorter than what was asked for — or empty, when the loop
    /// dequeued the turn first — because the server's answer is authoritative; the composer
    /// splices exactly this and nothing else.
    Recalled(Vec<crate::client::WithdrawnTurn>),
    /// The event source closed (daemon gone) → the UI should exit.
    Disconnected,
}

/// How the TUI picks its graphics protocol for result images (docs/tui.md).
/// Detection is a startup act — `Picker::from_query_stdio` runs once in
/// `ui_loop`, before the crossterm reader thread takes stdin — and tmux makes
/// detection lie in both directions (its DA1 masks a kitty-capable outer
/// terminal; passthrough can make OSC 1337 work with nothing detectable saying
/// so), so the override ships in the same change as detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageMode {
    /// Query the terminal; fall back to halfblocks when nothing answers.
    #[default]
    Auto,
    /// Force the kitty graphics protocol (a tmux-masked kitty outer terminal).
    Kitty,
    /// Force iTerm2's OSC 1337 (env identification is honestly weaker than
    /// kitty's query fence, so forcing it is sometimes the truth detection missed).
    Iterm2,
    /// No image rendering: an image renders as its one-line reference.
    Off,
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

/// One live background process as the panel holds it: the wire's structural fields
/// merged with the tail this client accumulated itself.
///
/// The `processes` topic carries structure only — no tail, no last-chunk stamp — because
/// both change per output chunk and would make a level-triggered topic re-emit at the
/// rate of the output (docs/control-api.md). The content arrives as `process_output`
/// deltas instead, layered on top by `handle_event`.
///
/// `running_secs`/`idle_secs` are **derived for display** when state changes, from the
/// absolute stamps beside them. Derived at event time rather than at render time so
/// rendering stays a pure function of state (the snapshot harness depends on that); the
/// stamps are retained so the clock tick can recompute them without a round-trip
/// (`refresh_clocks`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BgProc {
    /// The spawn-minted process id (bare on the wire; rendered `p#<n>` for display).
    pub id: u64,
    pub command: String,
    /// The `tool_use` id of the call that spawned this process, from the wire
    /// (docs/supervisor.md). `None` on a daemon that predates the mapping; the
    /// sidebar's `view` verb is the one thing that needs it, and says so when it
    /// is missing rather than jumping somewhere arbitrary.
    pub call_id: Option<String>,
    /// Absolute Unix millis, from the wire.
    pub spawned_at: u64,
    /// Absolute Unix millis, stamped locally on each `process_output` delta. `None` until
    /// the first chunk — the wire cannot supply it without re-emitting per chunk.
    pub last_chunk_at: Option<u64>,
    /// Accumulated locally from `process_output` deltas, seeded on first sight from
    /// `GET /processes/:id/output` (the bridge's cold-open fetch, docs/control-api.md).
    pub tail: String,
    pub running_secs: u64,
    pub idle_secs: u64,
}

impl BgProc {
    /// Recompute the display counters from the absolute stamps. Called whenever the
    /// panel's state changes, and once a second by the clock tick (`refresh_clocks`).
    pub fn refresh_elapsed(&mut self) {
        self.running_secs = secs_since(self.spawned_at);
        self.idle_secs = self.last_chunk_at.map(secs_since).unwrap_or(0);
    }

    /// The prose form of the id (`p#3`) — what the panel shows, matching the
    /// vocabulary the model and the transcript use.
    pub fn handle(&self) -> String {
        crate::supervisor::handle(self.id)
    }
}

/// A pending tool-approval gate, as surfaced by `GET /state`'s `gate`.
#[derive(Debug, Clone)]
pub(crate) struct PendingGate {
    pub call_id: String,
    pub name: String,
    pub input: serde_json::Value,
    /// The spotter's one-line reason for holding this call, if it gated via the spotter
    /// (docs/permissions.md) — surfaced inline at the gate so the human sees *why*.
    pub reason: Option<String>,
}
/// Usage statistics from the daemon — the `usage` state topic (also `GET /state`'s
/// `usage` key).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct UsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    /// The wire's absolute `usage.last_request_at` (Unix millis), retained so the
    /// clock tick can recompute the elapsed display without a round-trip
    /// (docs/control-api.md — the daemon sends the instant, not a ticking count, so a
    /// level-triggered `usage` topic stays quiet).
    pub last_request_at: Option<u64>,
    /// Seconds since the last upstream request, derived from `last_request_at` at
    /// parse time and again on each clock tick. Derived rather than computed at render
    /// time so rendering stays a pure function of state and the snapshot harness stays
    /// deterministic.
    pub seconds_since_last_request: Option<u64>,
    /// The model's context-window limit, from `usage.context.limit` — the
    /// boot-fetched capability (docs/transport.md). `None` when the daemon
    /// reports no capabilities (offline verbs, tests).
    pub context_limit: Option<u64>,
    /// The last request's total prompt tokens, from `usage.context.used`.
    /// `None` until the first request of the session.
    pub context_used: Option<u64>,
    /// The session's billed cost so far in USD, from `usage.cost.usd` — the
    /// provider's own accounting (docs/transport.md). `None` when the provider
    /// has no per-request cost surface (direct Anthropic).
    pub cost_usd: Option<f64>,
    /// Requests the cost fetch could not price, from `usage.cost.unpriced` —
    /// nonzero marks `cost_usd` as an undercount.
    pub cost_unpriced: Option<u64>,
    /// Lines kept vs. lines triage was shown, cumulative over the session, from
    /// `usage.retention` (docs/redaction-fork.md). `(0, 0)` before the first
    /// distilled result — unknown, and rendered as absent rather than as 0%.
    pub kept_lines: u64,
    pub shown_lines: u64,
    /// Results whose output a failed gate withheld, from `usage.retention.withheld`.
    /// Not folded into the ratio; rendered only when nonzero.
    pub withheld_results: u64,
}

/// The last non-success upstream response, as surfaced by `GET /state`. Cleared
/// by the next 2xx, so it names a failure that is current (docs/transport.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UpstreamFailureView {
    pub status: u16,
    pub count: u32,
}

/// A retry in flight, as surfaced by `GET /state`'s `upstream_retry`
/// (docs/transport.md).
///
/// The topic also carries `next_attempt_at` (absolute Unix millis) and the
/// upstream's `detail` text; neither is kept here. The status bar renders one
/// line and does not tick, so a countdown would either be stale or force a
/// wall-clock read into the render path, and the error text belongs in the log
/// rather than in a single status row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamRetryView {
    /// `"turn"` or `"peek"` — an exhausted peek withholds a verdict where an
    /// exhausted turn ends the trajectory, so the surface is rendered, not folded away.
    pub surface: String,
    /// The daemon's lead line, e.g. *stream aborted, retrying (2 of 12)*. It already
    /// names the attempt and budget, so the status bar shows it verbatim.
    pub message: String,
}

/// A trajectory that gave up, as surfaced by `GET /state`'s `stall`
/// (docs/transport.md).
///
/// The topic also carries the reason, the attempt count, the instant it
/// stopped, and the upstream's `detail`; the status bar keeps only the lead
/// line, for the same reason it keeps only the retry's — one row, no clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StallView {
    /// The daemon's lead line, e.g. *the trajectory stopped: 12 attempts in a
    /// row aborted upstream — send a turn to retry*. It already names the
    /// recovery, so the status bar shows it verbatim.
    pub message: String,
}

/// What the session is, as surfaced by `GET /state`'s `session_identity`
/// (docs/control-api.md): the model it was configured with and the directory it
/// runs in. Rendered in the sidebar's SESSION section — which model is answering
/// is not derivable from the transcript, and a log that mixes models across
/// sessions needs the current one named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionIdentityView {
    pub model: String,
    /// The working directory, already shortened against `$HOME` at parse time —
    /// derivation stays out of the render path, as with the usage stats.
    pub cwd: String,
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

/// An actionable sidebar row, held by *identity* rather than screen position
/// (docs/tui.md). A process is its spawn-minted id; a proposal is the `seq` of its
/// `Proposed` log entry. Focus stored as an index into a frame-rebuilt list would
/// silently substitute whatever occupies the position after the list reorders or
/// shrinks — a process exits between focus landing and `⏎`, and the press fires a
/// different row. Identity makes that substitution impossible: each activation
/// re-resolves against current data, and a vanished row no-ops instead of firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarRow {
    Process(u64),
    Proposal(u64),
}

impl SidebarRow {
    /// The stable key two renders of the same row compare equal on.
    pub(crate) fn id(self) -> u64 {
        match self {
            SidebarRow::Process(id) | SidebarRow::Proposal(id) => id,
        }
    }
}

/// Where focus came from when it entered the sidebar (docs/tui.md): Tab in the
/// sidebar returns there. The composer is not a source — Tab types in it — so the
/// alternatives are the strip (at its item) and the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarReturn {
    /// The strip, at the verb that held focus — identity, like the focus itself
    /// (docs/tui.md), so returning cannot land on a different verb.
    Strip(control_strip::StripVerb),
    Transcript,
}

/// A verb dispatched from a focused sidebar row, sent to the tokio side, which
/// issues the matching control-channel call (docs/tui.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarCmd {
    /// `DELETE /processes/:id` — SIGKILL the process group (docs/supervisor.md).
    KillProcess(u64),
    /// `PATCH /proposals/:seq {state:"committed"}` — fold the change into the
    /// effective config (docs/permissions.md).
    CommitProposal(u64),
    /// `PATCH /proposals/:seq {state:"cancelled"}` — drop it without touching
    /// the effective config (docs/permissions.md).
    CancelProposal(u64),
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
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
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

pub use gallery::stream::Script as StreamScript;
pub use run::{run, run_gallery, run_view};

use bridge::bridge;
use run::DEFAULT_THEME;
use ui_loop::{Senders, ui_loop};

/// Seconds elapsed since an absolute Unix-millis stamp, per the local clock. The
/// client-side half of "send absolute instants, let clients compute elapsed"
/// (docs/control-api.md). Saturating: a stamp in the future (clock skew between a
/// remote daemon and this client) reads as 0 rather than wrapping.
pub(crate) fn secs_since(at_ms: u64) -> u64 {
    let now = crate::log::Timestamp::now().0;
    now.saturating_sub(at_ms) / 1000
}
