//! Interactive UI state (vendored from ferritin, adapted for efference).
//!
//! The reusable core — viewport/scroll tracking, the keyboard link cursor, the
//! per-frame layout and render caches — carries over verbatim. What changed: the
//! request-thread channel (`cmd_tx`/`resp_rx`) and rustdoc navigation `History`
//! are gone. Input arrives as [`TuiEvent`]s from the daemon's SSE bridge; the
//! state holds the durable [`log`](InteractiveState::log) plus a [`LiveTail`] of
//! in-flight streaming deltas, and re-projects both into a [`Document`] as events
//! land (docs/README.md — the transcript is another `project(log)`).

use ratatui::layout::{Position, Rect};
use std::borrow::Cow;
use std::time::Instant;

use super::theme::InteractiveTheme;
use super::utils::{find_node_at_path_mut, supports_cursor_shape};
use crate::frontend::{Approval, Interrupt};
use crate::log::{LogEntry, Payload};
use crate::tui::clipboard::TextClipboard;
use crate::tui::composer::Composer;
use crate::tui::document::{Document, DocumentNode, NodePath, TuiAction};
use crate::tui::projection::{self, LiveTail, Phase};
use crate::tui::render_context::{RenderContext, ThemeError};
use crate::tui::{
    BgProc, PaceCmd, PaceView, PendingGate, SidebarCmd, SidebarReturn, SidebarRow, TuiEvent,
    UpstreamFailureView, UsageStats,
};

use crossbeam_channel::Receiver;
use tokio::sync::mpsc::UnboundedSender;

/// The idle status-bar line (also restored when an approval gate resolves). After
/// the bind purge (docs/tui.md) the status bar stops enumerating
/// verb keys — the strip is the legend — and just points there.
const IDLE_STATUS: &str = "efference — ↑ controls";

/// The status-bar line for a read-only view of a recorded log (docs/tui.md): it
/// names the session being read and says the view is inert, since the transcript
/// itself looks identical to a live one.
pub(super) fn view_status(label: &str) -> String {
    format!("viewing {label} · read-only — ↑ controls")
}

/// Cap on a process-panel row's live tail between `/state` polls (TUI-bg-2). The
/// panel only renders the last line, and a poll reconciles to the authoritative
/// snapshot every ~400ms, so this just bounds the gap-filling append.
const BG_TAIL_CAP: usize = 4096;

/// The between-turns speed rungs the `+`/`-` keys step through (ms)
/// (docs/tui.md). `0` is as-fast-as-possible; the ladder is coarse on
/// purpose — a felt "slower/faster," not a precise slider. Tunable in the end-of-sequence ergonomics
/// pass.
const PACE_LADDER: &[u64] = &[0, 500, 1_000, 2_000, 5_000, 10_000, 30_000];

/// The pace rung one step from `cur` along [`PACE_LADDER`]: `slower` climbs toward
/// the slowest rung (clamped there), else steps down toward as-fast-as-possible.
/// Shared by the strip's pace widget stepper (docs/tui.md).
pub(super) fn next_pace_rung(cur: u64, slower: bool) -> u64 {
    let idx = PACE_LADDER.iter().rposition(|&ms| ms <= cur).unwrap_or(0);
    if slower {
        PACE_LADDER.get(idx + 1).copied().unwrap_or(cur)
    } else {
        PACE_LADDER.get(idx.saturating_sub(1)).copied().unwrap_or(0)
    }
}

/// A short human label for a speed dial value: `fast` at zero, else `2s` / `500ms`.
pub(super) fn pace_label(speed_ms: u64, held: bool) -> String {
    let speed = if speed_ms == 0 {
        "fast".to_string()
    } else if speed_ms.is_multiple_of(1000) {
        format!("{}s", speed_ms / 1000)
    } else {
        format!("{speed_ms}ms")
    };
    if held {
        format!("HELD ({speed})")
    } else {
        speed
    }
}

/// The status-bar pace indicator (docs/tui.md) — loud to the operator.
/// `None` at the default (fast, not held) so the normal state adds no chrome; a `⏸`/`⏱` glyph marks
/// held / slowed otherwise.
pub(super) fn pace_indicator(pace: Option<PaceView>) -> Option<String> {
    let p = pace?;
    match (p.held, p.speed_ms) {
        (false, 0) => None,
        (true, 0) => Some("⏸ HELD".to_string()),
        (true, ms) => Some(format!("⏸ HELD · {}", pace_label(ms, false))),
        (false, ms) => Some(format!("⏱ {}", pace_label(ms, false))),
    }
}

/// The status-bar upstream diagnostic. It records the last non-success response;
/// a later successful request intentionally does not remove it (docs/transport.md).
pub(super) fn upstream_failure_indicator(failure: Option<UpstreamFailureView>) -> Option<String> {
    let failure = failure?;
    Some(format!("upstream {} ×{}", failure.status, failure.count))
}

/// UI mode — the modal structure of the interface. ferritin's `DevLog` and
/// `Input` (go-to/search) modes are dropped for the read-only M1 viewer.
#[derive(Debug)]
pub(super) enum UiMode {
    /// Normal browsing mode
    Normal,
    /// Help screen
    Help,
    /// Theme picker modal
    ThemePicker {
        /// Index of currently selected theme
        selected_index: usize,
        /// Theme name to restore on cancel
        saved_theme_name: String,
    },
}

/// Document state. (ferritin also carried navigation `History` here; the
/// efference transcript is a single growing document, so only the doc remains —
/// the `.document.document` path the render pipeline uses is preserved.)
#[derive(Debug)]
pub(super) struct DocumentState<'a> {
    pub document: Document<'a>,
    /// Per-top-level-node layout, parallel to `document.nodes` and valid only at
    /// [`layout_width`](Self::layout_width) (docs/tui.md — the layout cache).
    /// `None` means "not laid out yet at this width"; the next pass computes it.
    pub layouts: Vec<Option<NodeLayout<'a>>>,
    /// The render width `layouts` was computed at. A width change invalidates all
    /// of them, since wrapping is width-dependent.
    pub layout_width: u16,
    /// How many top-level nodes the last walk actually laid out, as opposed to
    /// reusing. The number a growing transcript must not grow: it is what the
    /// render-cost test asserts on.
    pub laid_out: usize,
    /// How many source lines the last walk actually ran syntect over — i.e. how
    /// many missed the [highlight memo](crate::tui::render_code_block). A *visible*
    /// node is re-laid-out every frame, so a multi-screen code block would re-parse
    /// itself on each one; this is what the scroll-cost test pins (docs/tui.md).
    pub highlighted: usize,
}

impl<'a> DocumentState<'a> {
    /// Drop every cached node layout (a width change, or a wholesale rebuild).
    pub fn invalidate_all(&mut self) {
        self.layouts.clear();
        self.layouts.resize_with(self.document.nodes.len(), || None);
    }

    /// Drop one top-level node's cached layout — used where a node changes on its
    /// own, e.g. an in-place `ExpandBlock` (`tui/events.rs`), which changes that
    /// node's height and nothing else's.
    pub fn invalidate_node(&mut self, idx: usize) {
        if let Some(slot) = self.layouts.get_mut(idx) {
            *slot = None;
        }
    }

    /// Replace a run of top-level nodes, keeping the parallel layout cache aligned.
    /// The **only** way the incremental projection (`tui/projection/incremental.rs`)
    /// changes the document, so a replaced node can never keep a stale height: the
    /// nodes and their layouts move together or not at all.
    pub fn splice_nodes(
        &mut self,
        range: std::ops::Range<usize>,
        nodes: Vec<DocumentNode<'a>>,
    ) -> bool {
        if range.is_empty() && nodes.is_empty() {
            return false;
        }
        self.layouts.resize_with(self.document.nodes.len(), || None);
        self.layouts
            .splice(range.clone(), std::iter::repeat_n(None, nodes.len()));
        self.document.nodes.splice(range, nodes);
        true
    }
}

/// One top-level node's laid-out result, reused on frames where the node itself
/// did not change and it falls outside the viewport (docs/tui.md).
///
/// `actions` carries the node's focus/click targets with **`y` relative to the
/// node's first row**, so replaying them costs one add per action when the nodes
/// above have shifted. Registering them for skipped nodes is what keeps
/// [`RenderCache::actions`] — and therefore the `action_index` the keyboard cursor
/// is expressed in — identical to a full walk's.
#[derive(Debug, Clone)]
pub(super) struct NodeLayout<'a> {
    pub height: u16,
    pub actions: Vec<(Rect, TuiAction<'a>)>,
}

/// Cached document layout information
#[derive(Debug, Clone, Copy)]
pub(super) struct DocumentLayoutCache {
    pub render_width: u16,
    pub document_height: u16,
}

/// Keyboard cursor state for link navigation
///
/// Models the cursor as always being in one of three positions:
/// - **VirtualTop**: Conceptually above all content (initial state, or scrolled up past first link)
/// - **Focused**: On a specific link (which may be visible or off-screen due to mouse scrolling)
/// - **VirtualBottom**: Conceptually below all content (scrolled down past last link)
///
/// The cursor position is preserved across scrolling (mouse/Page Up/Down) and only
/// resets to VirtualTop when navigating to a new document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyboardCursor {
    /// Positioned conceptually above the document (initial state)
    VirtualTop,
    /// Focused on a specific link (index into render_cache.actions)
    /// The link may be visible or scrolled off-screen
    Focused { action_index: usize },
    /// Positioned conceptually below the document
    VirtualBottom,
}

/// Viewport and scroll tracking
#[derive(Debug)]
pub(super) struct ViewportState {
    pub scroll_offset: u16,
    pub cursor_pos: Option<Position>,
    pub clicked_position: Option<Position>,
    pub cached_layout: Option<DocumentLayoutCache>,
    /// The scroll offset the last completed paint used. `render_frame` compares
    /// it against the settled offset to know the viewport is **in motion** this
    /// frame — the signal that swaps a retransmit-per-move image protocol
    /// (iTerm2, sixel) to its flicker-free halfblocks slice while scrolling
    /// (docs/tui.md).
    pub last_painted_scroll: u16,
    /// The document height as of the last completed walk, kept **after**
    /// [`cached_layout`](Self::cached_layout) is invalidated.
    ///
    /// A re-projection drops the layout cache, so between a streamed fragment and
    /// the next paint the current height is genuinely unknown — and during a stream
    /// that is most of the time. `max_scroll` answering "0" there made every scroll
    /// event evaluate `offset >= max_scroll` as true, i.e. "the user is at the
    /// bottom", which silently re-armed stream-following and snapped the viewport
    /// down from wherever they were reading. The previous frame's height is the last
    /// thing actually measured, and it errs the safe way: content only ever arrives
    /// *below*, so it under-estimates the bottom rather than claiming to be at it.
    pub last_document_height: u16,
    /// Last known viewport height for scroll clamping
    pub last_viewport_height: u16,
    /// Scrollbar hover/drag state
    pub scrollbar_hovered: bool,
    pub scrollbar_dragging: bool,
    /// Keyboard navigation cursor
    pub keyboard_cursor: KeyboardCursor,
    /// While true, new streamed content keeps the viewport pinned to the bottom.
    /// Cleared when the user scrolls up; re-armed when they scroll back to the end.
    pub stuck_to_bottom: bool,
    /// Height (rows) the composer occupied last frame, so mouse hit-testing and
    /// scroll clamping agree with `render_frame` on where the transcript ends.
    pub input_height: u16,
    /// Retired: the approval gate moved inline into the transcript (docs/tui.md),
    /// so it no longer reserves a bottom-chrome row. Kept (always 0) so the mouse
    /// hit-testing math reads uniformly as `1 + input_height + gate_height`.
    pub gate_height: u16,
    /// The right-docked sidebar's rectangle as of the last frame, or `None` when it's
    /// hidden (narrow terminal). Remembered so the sidebar-only dev screenshot can
    /// slice exactly those columns (docs/tui.md).
    pub sidebar_area: Option<Rect>,
    /// The composer's text rectangle as of the last frame, after a staged-verb chip
    /// has claimed its left columns. Mouse hit testing uses this rendered geometry.
    pub composer_area: Option<Rect>,
    /// True while a primary-button drag is extending the composer selection.
    pub composer_dragging: bool,
}

/// Rendering state computed each frame
#[derive(Debug)]
pub(super) struct RenderCache<'a> {
    pub actions: Vec<(Rect, TuiAction<'a>)>,
}

/// A stager activation in progress (docs/tui.md): the control-strip verb whose
/// argument the composer is currently editing, plus the operator's parked draft.
#[derive(Debug)]
pub(super) struct Staging {
    pub verb: StagedVerb,
    /// The composer text at activation, restored on commit or cancel.
    pub parked: String,
}

/// The strip verbs that stage an argument into the composer. One so far; the
/// chip label and commit dispatch match on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StagedVerb {
    /// `.allow` — edit the grant spec (a bash command prefix) before committing.
    Allow,
}

impl StagedVerb {
    /// The chip label shown at the prompt while staging.
    pub(super) fn label(self) -> &'static str {
        match self {
            StagedVerb::Allow => "allow",
        }
    }
}

/// An active inline widget (docs/tui.md): the *widget* item class's sub-state, a
/// strip verb whose value is being adjusted in place with `↑`/`↓`. One so far —
/// the pace stepper — so this holds just what a revert needs; a second widget
/// would generalise it (backlog note, not now).
#[derive(Debug)]
pub(super) struct PaceWidget {
    /// `speed_ms` at entry, restored on Esc-revert. Stepping previews by mutating
    /// the live `pace` (so the status bar tracks it); only `⏎` pushes to the daemon.
    pub entry_speed_ms: u64,
}

/// UI display state
#[derive(Debug)]
pub(super) struct UiState {
    pub mouse_enabled: bool,
    pub debug_message: Cow<'static, str>,
    pub is_hovering: bool,
    pub supports_cursor: bool,
    /// Raised by an activation that must end the UI loop (the strip's `detach`,
    /// which mouse clicks can reach — clicks have no quit return path). Checked
    /// by the loop alongside `handle_key_event`'s return.
    pub quit_requested: bool,
}

/// Loading/streaming indicator state
#[derive(Debug)]
pub(super) struct LoadingState {
    pub pending_request: bool,
    pub started_at: Instant,
    /// Whether the wait cursor is currently shown (so `update_cursor` only
    /// transitions shapes on change).
    pub was_loading: bool,
}

/// Layout state - cursor position, indentation, and viewport
/// Reset at the start of each frame render
#[derive(Debug)]
pub(super) struct LayoutState {
    pub pos: Position,
    pub indent: u16,
    pub node_path: NodePath,
    pub area: Rect,
    /// Stack of x positions where blockquote markers should be drawn
    /// When rendering content, markers are drawn at each of these positions
    pub blockquote_markers: Vec<u16>,
    /// True during the **measure** pass (docs/tui.md): the walk runs for its
    /// heights only, so every row counts as off-screen — no cell is written and no
    /// syntax highlighting runs. [`row_visible`](InteractiveState::row_visible) is
    /// the single place that reads it.
    pub measuring: bool,
    /// True while this frame's scroll offset differs from the last painted one —
    /// the viewport is moving. Image rendering reads it to hold the halfblocks
    /// slice under protocols whose native render flashes on every reposition
    /// (docs/tui.md); a settled follow-up frame restores the crisp render.
    pub scroll_in_motion: bool,
}

/// Main interactive state - composes all UI state
#[derive(Debug)]
pub(super) struct InteractiveState<'a> {
    pub document: DocumentState<'a>,
    pub viewport: ViewportState,
    pub render_cache: RenderCache<'a>,
    pub layout: LayoutState,
    pub ui_mode: UiMode,
    pub ui: UiState,
    pub loading: LoadingState,

    /// The durable log, accumulated from the `/events` tail. The transcript is
    /// projected from this plus [`live`](InteractiveState::live).
    pub log: Vec<LogEntry>,
    /// In-flight streaming deltas not yet committed to the log.
    pub live: LiveTail,
    /// What the transcript document currently reflects, so the next projection can
    /// splice rather than rebuild (docs/tui.md).
    pub transcript: projection::Transcript,
    /// Per-exchange view-state the human has toggled (expand/collapse, raw-vs-wire),
    /// kept here so it survives re-projection (docs/tui.md): the transcript rebuilds
    /// on every streamed token, so a block's toggles can't live in the rebuilt
    /// `Document`. `project` reads it, keyed by `call_id`.
    pub view: projection::ViewState,
    /// Inbound events from the SSE bridge (durable entries + ephemeral deltas).
    pub events: Receiver<TuiEvent>,

    /// `Some(label)` when this is a **read-only view** of a recorded log with no
    /// daemon behind it (docs/tui.md — `efference view`): the label names the
    /// session in the status bar, and its presence is what
    /// [`read_only`](Self::read_only) reports. In that mode the composer is not
    /// rendered, the strip carries no loop verbs, and the resting focus is the
    /// strip rather than the composer — nothing in the UI can send.
    pub viewing: Option<String>,

    /// True while rendering inside a [`DocumentNode::Dimmed`] subtree
    /// (docs/tui.md). Render-pass scratch, not UI state: it is set and restored
    /// within a single `render_node` walk, so a frame is still a pure function
    /// of the state above it — which the snapshot harness depends on.
    pub dim_register: bool,

    /// Computed syntax highlighting, kept between frames so a visible multi-screen
    /// code block is parsed once rather than on every paint (docs/tui.md).
    pub highlight_memo: super::render_code_block::HighlightMemo,

    /// Every result image the transcript references — fetch state, decoded
    /// pixels, encodings — plus the terminal's graphics capability
    /// (docs/tui.md). The transcript's image nodes are references into this.
    pub images: super::image::ImageStore,
    /// Requests a result image's bytes from the tokio side, which GETs
    /// `/log/:seq/images/:idx` and answers with a [`TuiEvent::Image`].
    pub images_tx: UnboundedSender<super::image::ImageKey>,

    /// Paint durations, when `--frame-cost` asked for them (`frame_cost.rs`).
    /// `None` is the normal case and costs nothing.
    pub frame_cost: Option<super::frame_cost::FrameCost>,

    /// The input composer (M2). The swappable input interface (docs/tui.md).
    pub composer: Composer,
    /// System plain-text clipboard plus an in-process fallback. Kept outside the
    /// composer so editor state stays deterministic.
    pub clipboard: TextClipboard,
    /// Focus model: true = composing (keys go to the composer), false = command
    /// mode (M1's single-key transcript commands). Esc toggles; a printable key in
    /// command mode returns to composing. Starts focused.
    pub composing: bool,
    /// Control-strip focus (docs/tui.md): `Some(i)` while the strip's item `i` is
    /// focused — the middle stop on the transcript → strip → composer axis. Only
    /// meaningful while `composing` is false; cleared on every return to the
    /// composer.
    pub strip_focus: Option<usize>,
    /// An in-progress stager activation (docs/tui.md): the composer is editing a
    /// strip verb's argument, with the operator's draft parked for restoration on
    /// commit or cancel. `Some` implies `composing`.
    pub staging: Option<Staging>,
    /// An active inline pace widget (docs/tui.md): `Some` while the strip's pace
    /// stepper captures `↑`/`↓`. Implies strip focus; mutually exclusive with
    /// `staging` (that is composer-focused). Esc reverts it, `⏎` commits.
    pub pace_widget: Option<PaceWidget>,
    /// The strip items' rendered column ranges (`x_start..x_end`, item index),
    /// rebuilt each frame — what mouse clicks on the strip row resolve against.
    pub strip_hits: Vec<(u16, u16, usize)>,
    /// Block-menu selection (docs/tui.md): which of the focused transcript unit's
    /// entry verbs `⏎` fires. Reset to the leading verb whenever focus moves to
    /// another unit, and clamped to the current unit's verb list at use.
    pub block_menu_focus: usize,
    /// The block menu's rendered item rects in *document* coordinates, rebuilt
    /// each frame — what a click resolves against, ahead of the whole-header
    /// expand action underneath it.
    pub block_menu_hits: Vec<(Rect, usize)>,
    /// Sends a submitted turn to the tokio side, which `POST`s it to the daemon.
    pub submit_tx: UnboundedSender<String>,
    /// Optimistic echoes of just-submitted user turns, in submission order, not yet
    /// confirmed by a committed `User` log entry (docs/tui.md).
    /// `project` renders them dimmed after the live tail; each is dropped when its
    /// matching `User` entry arrives via `/events` (content match on this ordered
    /// queue). The delivery indicator: a message never vanishes into the void while a
    /// trajectory runs, and the pending→committed flip *is* "now on the wire."
    pub pending_turns: Vec<String>,
    /// The pending tool-approval gate, if any (polled from `GET /state`, M3).
    pub pending_gate: Option<PendingGate>,
    /// Sends an approval decision (`call_id`, decision) to the tokio side, which
    /// resolves it via `PUT /approvals/:call_id`.
    pub approve_tx: UnboundedSender<(String, Approval)>,
    /// Sends a pace command (`speed_ms`, `held`) to the tokio side, which `POST`s it to
    /// `/pace` (docs/tui.md).
    pub pace_tx: UnboundedSender<PaceCmd>,
    /// Fires the operator's interrupt intent at the in-flight tool: `Kill` →
    /// `POST /loop/interrupt` (the hard rung of graduated Esc, docs/tui.md), `Detach` →
    /// `PATCH /processes/:id {background: true}` (the foreground→background handoff, docs/supervisor.md).
    /// A no-op if nothing is running.
    pub interrupt_tx: UnboundedSender<Interrupt>,
    /// Dispatches a focused sidebar row's verb to the tokio side, which issues the
    /// matching control-channel call (docs/tui.md). Fire-and-forget: a stale row
    /// (process exited, proposal resolved elsewhere) is answered 404 by the daemon
    /// and surfaces as a debug message here.
    pub sidebar_tx: UnboundedSender<SidebarCmd>,
    /// Whether the sidebar holds the focus context (docs/tui.md) — mutually
    /// exclusive with `composing` and with `strip_focus`, the third context on top
    /// of the vertical axis. Separate from [`sidebar_focus`](Self::sidebar_focus)
    /// because the column can be focused with *no row selected*: the selected
    /// row can vanish under the operator (a process exits between focus landing
    /// and `⏎`), and identity forbids letting the press fall through to whatever
    /// occupies the position now. Open-and-unfocused, `⏎` no-ops until an arrow
    /// key picks a row.
    pub sidebar_open: bool,
    /// The actionable row the sidebar's keys act on, held by identity
    /// ([`SidebarRow`]) rather than screen position; `None` while the column is
    /// open with nothing selected.
    pub sidebar_focus: Option<SidebarRow>,
    /// Where Tab-in-the-sidebar returns to: the strip at item `i`, or the
    /// transcript. Set when focus enters the sidebar; the composer can't be the
    /// source (Tab types there), and Esc homes to it regardless.
    pub sidebar_return: Option<SidebarReturn>,
    /// Which of the focused row's verbs `←`/`→` have selected and `⏎` fires.
    /// Reset to the leading verb whenever the row changes, like the block menu's.
    pub sidebar_verb_focus: usize,
    /// The sidebar rows' rendered rects, rebuilt each frame — what a mouse click
    /// resolves into row focus (`sidebar_verb_hits` likewise for verbs).
    pub sidebar_hits: Vec<(Rect, SidebarRow)>,
    pub sidebar_verb_hits: Vec<(Rect, SidebarRow, usize)>,
    /// Whether the loop is executing a turn, polled from `GET /state`'s `running` (docs/tui.md).
    /// Graduated Esc reads it (via [`is_loop_active`](Self::is_loop_active)) to choose
    /// pause-vs-swallow — reliable through a *silent* long tool, where streaming
    /// (`pending_request`) reads false.
    pub loop_running: bool,
    /// The operator's pace dial / hold overlay, polled from `GET /state`'s `pace`
    /// (docs/tui.md). `None` until the first poll. Ephemeral chrome —
    /// surfaced in the status bar (loud to the operator), never in the transcript, and never
    /// projected (a held/slowed loop is byte-identical on the wire to a fast one).
    pub pace: Option<PaceView>,
    /// The live background processes (docs/supervisor.md), polled from `GET /state`. Rendered
    /// as a persistent panel above the composer; empty hides it. Ephemeral chrome —
    /// never part of the transcript document, so it never triggers a reproject.
    pub processes: Vec<BgProc>,
    /// The cumulative usage statistics (polled from `GET /state`). Rendered in the
    /// status bar. Ephemeral chrome — reflects API telemetry from the daemon.
    pub usage: Option<UsageStats>,
    /// The last non-success upstream response, polled from `GET /state`. It remains
    /// visible after recovery as a transport diagnostic; never transcript content.
    pub upstream_failure: Option<UpstreamFailureView>,

    /// The effective policy + pending proposals (polled from `GET /state`). Drives the
    /// sidebar's POLICY section (docs/permissions.md). Ephemeral chrome.
    pub policy: Option<super::PolicyView>,

    /// A daemon-side change (committed entry, stream delta, gate, process list) is
    /// waiting to be painted. The UI loop coalesces these onto the redraw tick
    /// rather than painting per event, so a reattach burst or a fast token stream
    /// folds cheaply and paints at the frame rate. User input bypasses this and
    /// paints immediately. Cleared after each draw.
    pub dirty: bool,
    /// The log/live tail changed and the transcript must be re-projected before the
    /// next paint. Set alongside [`dirty`](Self::dirty) by transcript-changing
    /// events; chrome-only changes (gate, processes) set `dirty` without this.
    /// Deferring the (O(log)) [`reproject`](Self::reproject) out of the per-event
    /// path is what turns the reattach replay from O(N²) into one fold + a few
    /// projections.
    pub reproject_pending: bool,

    // Rendering config
    pub render_context: RenderContext,
    pub theme: InteractiveTheme,
    pub current_theme_name: Option<String>,
}

impl<'a> InteractiveState<'a> {}

#[cfg(test)]
mod test;

mod escape;
mod events;
mod gate;
mod lifecycle;
mod scroll;
