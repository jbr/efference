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

mod artifacts;
pub(crate) mod attention;
pub(crate) mod block_menu;
pub(crate) mod clipboard;
pub(crate) mod closed;
pub(crate) mod color_scheme;
pub(crate) mod composer;
pub(crate) mod control_strip;
pub(crate) mod detail;
pub(crate) mod document;
pub(crate) mod events;
pub(crate) mod frame_cost;
pub(crate) mod gallery;
pub(crate) mod grid;
#[cfg(test)]
pub(crate) mod harness;
mod host_dispatch;
pub(crate) mod image;
pub mod inline;
pub mod inline_list;
pub(crate) mod keyboard;
pub(crate) mod markdown;
pub(crate) mod mouse;
pub(crate) mod pasteboard;
pub(crate) mod policy_view;
pub(crate) mod process_panel;
pub(crate) mod projection;
pub(crate) mod render_code_block;
pub(crate) mod render_context;
pub(crate) mod render_detail;
pub(crate) mod render_document;
pub(crate) mod render_entry;
pub(crate) mod render_frame;
pub(crate) mod render_header;
pub(crate) mod render_help_screen;
pub(crate) mod render_image;
pub(crate) mod render_loading_bar;
pub(crate) mod render_node;
pub(crate) mod render_scrollbar;
pub(crate) mod render_sessions;
pub(crate) mod render_sidebar;
pub(crate) mod render_span;
pub(crate) mod render_staged;
pub(crate) mod render_status_bar;
pub(crate) mod render_table;
pub(crate) mod render_theme_picker;
pub mod resume_list;
pub(crate) mod senders;
pub mod session_form;
pub(crate) mod sessions;
pub(crate) mod sidebar;
pub(crate) mod span_style;
pub(crate) mod staged;
pub(crate) mod starting;
pub(crate) mod state;
pub(crate) mod table_layout;
pub(crate) mod theme;
pub(crate) mod topics;
pub(crate) mod utils;
pub(crate) mod write_text;

// The render pipeline and input handlers reference these via `super::` (i.e. the
// `tui` module namespace), mirroring ferritin's `renderer::interactive` layout.
// Private re-export: visible to the `tui` submodules, not the wider crate.
use senders::Senders;
use state::{InteractiveState, UiMode};

/// Why the loop is parked at its cold check: read from the `cold_parked` loop phase and
/// whether the `cold` topic still holds the belief (docs/control-api.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkReason {
    /// Waiting for a person to act before an uncached read, or for a compaction.
    Expired,
    /// A compaction summarized the history while parked; a send resumes it.
    Compacted,
}

/// One inbound message to the UI thread, decoded by the bridge from the unified
/// control stream's frames (docs/control-api.md): durable [`Entry`](TuiEvent::Entry)
/// values from `log` frames, the ephemeral deltas from `output` frames (folded into
/// the live tail until their committed entry lands), and a [`Topic`](TuiEvent::Topic)
/// for each `state` frame — a whole-topic replacement.
#[derive(Debug)]
pub(crate) enum TuiEvent {
    /// A committed log entry (a `log` frame — the durable class).
    Entry(LogEntry),
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
    /// One state topic's new value (a `state` frame, docs/control-api.md): the whole value,
    /// replacing what the store held for it (`topics.rs`, docs/tui.md).
    Topic(topics::TopicUpdate),
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
    /// An artifact fence in one committed assistant entry. It renders in the web
    /// client; the terminal draws a notice in its place.
    Artifact {
        coordinate: artifacts::Coordinate,
        kind: crate::record::artifact::Kind,
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
    Recalled(Vec<crate::protocol::turns::QueuedTurn>),
    /// A send was refused: it named no question while one was pending, or named one no
    /// longer pending (`answered`). The turn comes back for the composer (docs/tui.md).
    NotDelivered {
        turn: crate::protocol::turns::QueuedTurn,
        answered: bool,
    },
    /// A staged attachment's upload answered (docs/inbox.md): what the daemon stored, keyed
    /// by the staging key, or why it could not.
    Uploaded {
        key: u64,
        result: Result<crate::record::log::attachment::Arrival, String>,
    },
    /// Whether the system clipboard holds an image and no text, learned from its type
    /// list alone when it changes — the strip then says `C-v` attaches it (docs/tui.md).
    ClipboardImage(bool),
    /// A staged image's thumbnail, decoded and scaled off the UI thread.
    Thumbnail {
        key: u64,
        image: ::image::DynamicImage,
    },
    /// The host's session listing came back, or said why it could not
    /// (docs/tui.md). One whole listing per event: the switcher holds the latest
    /// and nothing is reconciled.
    Sessions(Result<Vec<crate::protocol::sessions::ListedSession>, String>),
    /// The event source closed (daemon gone) → the UI should exit.
    Disconnected,
    /// Open the offline form experiment against an empty context (docs/tui.md).
    #[cfg(feature = "dev")]
    SessionFormPrototype,
    /// The new-session form's opening read: where a session would be created and
    /// the connections available for it, or why neither could be read.
    NewSession(Result<session_form::NewSessionContext, String>),
    /// `QUERY /sessions` answered about the form's draft (docs/control-api.md).
    Preview(Result<crate::inference::ResolvedOptions, String>),
    /// `POST /sessions` answered: the new session's id, or why nothing was
    /// minted. On success the TUI leaves this session's stream for that one.
    Created(Result<String, String>),
    /// `GET /sessions?state=archived` answered: the resume page's rows, or why
    /// they could not be read (docs/tui.md).
    Archived(Result<Vec<resume_list::ResumeRow>, String>),
    /// `PATCH /sessions/:id` answered: the id that was revived, or the host's
    /// message. On success the TUI switches to it.
    Resumed(Result<String, String>),
    /// `GET /inference/catalog/:connection` answered: the connection it was asked
    /// for, and its entry or why the read failed. Replaces that connection's
    /// entry in the open form's picker.
    Catalog {
        connection: String,
        result: Result<crate::clients::client::ConnectionCatalog, String>,
    },
}

/// A verb the switcher issues against the *host* rather than the attached
/// session (docs/tui.md). Sent to the tokio side, which addresses the host's own
/// routes at the socket root; the answer rides home as a
/// [`TuiEvent::Sessions`].
#[derive(Debug, Clone)]
pub(crate) enum HostCmd {
    /// `GET /sessions` — the listing, as the switcher opens.
    List,
    /// `DELETE /sessions/:id` — end a session and re-read the listing. The host
    /// answers once teardown has run, so the re-read shows the finished state.
    Archive(String),
    /// `GET /inference` — the connection inventory, as the new-session form
    /// opens. Answered with the working root the dispatcher would create in, so
    /// the form displays exactly what creation will send (docs/tui.md).
    NewSession,
    /// `QUERY /sessions` — resolve the form's current draft. Answered with the
    /// effective selections and the issues that would stop creation.
    Preview(crate::inference::SessionOptions),
    /// `POST /sessions` — create from the form's draft. The selections travel;
    /// the working root and the environment are the dispatcher's, and never
    /// enter the UI thread (docs/env.md). `worktree` is creation-only, outside
    /// the resolvable options (docs/worktrees.md).
    Create {
        options: crate::inference::SessionOptions,
        worktree: bool,
    },
    /// `GET /sessions?state=archived` for the dispatcher's working root — the
    /// resume page's rows.
    Archived,
    /// `PATCH /sessions/:id {"state":"active"}` in the dispatcher's working root,
    /// reviving an archived session.
    Resume(String),
    /// `GET /inference/catalog/:connection` — one connection's entry, refreshed
    /// when older than `max_age` seconds. The working root is the dispatcher's,
    /// like preview's, so recent ranks scope to the project creation would run in.
    Catalog {
        connection: String,
        max_age: Option<u64>,
    },
}

/// Why the UI loop returned (docs/tui.md). Detaching leaves the session running
/// on its host; switching closes this stream and opens another, because the
/// per-session contract is one cursor per handshake (docs/control-api.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Exit {
    /// The client is done.
    Quit,
    /// The session's event stream ended. The entry point asks the host why and says so
    /// on stderr once the terminal is restored (`closed.rs`).
    Closed,
    /// Attach to a different session on the same host.
    Switch { id: String },
    /// Attach to `id` with `draft` in the composer: the turns a failed start handed
    /// back, carried to the session its retry made (`starting.rs`).
    SwitchWithDraft { id: String, draft: String },
}

/// A pace command the operator issues from the TUI (docs/tui.md), sent to
/// the tokio side, which issues `PATCH /loop {pace}`: `(speed_ms, held)`, each optional so a key
/// can move the speed dial, toggle the hold, or both.
pub(crate) type PaceCmd = (Option<u64>, Option<bool>);

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
    /// `DELETE /processes/:id` — SIGKILL the process group (docs/process-monitor.md).
    KillProcess(u64),
    /// `PATCH /proposals/:seq {state:"committed"}` — fold the change into the
    /// effective config (docs/permissions.md).
    CommitProposal(u64),
    /// `PATCH /proposals/:seq {state:"cancelled"}` — drop it without touching
    /// the effective config (docs/permissions.md).
    CancelProposal(u64),
}

use std::io::{self, stdout};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, select};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_lite::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::clients::client::Client;
use crate::record::log::LogEntry;
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
mod dispatchers;
mod operations;
mod run;
mod ui_loop;

pub use gallery::stream::Script as StreamScript;
pub use run::{Attached, run, run_gallery, run_view};

use bridge::bridge;
use run::DEFAULT_THEME;
use ui_loop::ui_loop;

/// Seconds elapsed since an absolute Unix-millis stamp, per the local clock. The
/// client-side half of "send absolute instants, let clients compute elapsed"
/// (docs/control-api.md). Saturating: a stamp in the future (clock skew between a
/// remote daemon and this client) reads as 0 rather than wrapping.
pub(crate) fn secs_since(at_ms: u64) -> u64 {
    let now = crate::record::log::Timestamp::now().0;
    now.saturating_sub(at_ms) / 1000
}

#[cfg(feature = "dev")]
pub use run::run_session_form_prototype;
