//! Interactive UI state (vendored from ferritin, adapted for efference).
//!
//! The reusable core — viewport/scroll tracking, the keyboard link cursor, the
//! per-frame layout and render caches — carries over verbatim. What changed: the
//! request-thread channel (`cmd_tx`/`resp_rx`) and rustdoc navigation `History`
//! are gone. Input arrives as [`TuiEvent`]s from the daemon's SSE bridge; the
//! state holds the durable [`log`](InteractiveState::log) plus a [`LiveTail`] of
//! in-flight streaming deltas, and re-projects both into a [`Document`] as events
//! land (DESIGN §10 — the transcript is another `project(log)`).

use ratatui::layout::{Position, Rect};
use std::borrow::Cow;
use std::time::Instant;

use super::theme::InteractiveTheme;
use super::utils::supports_cursor_shape;
use crate::frontend::Approval;
use crate::log::LogEntry;
use crate::tui::composer::Composer;
use crate::tui::document::{Document, NodePath, TuiAction};
use crate::tui::projection::{self, LiveTail};
use crate::tui::render_context::{RenderContext, ThemeError};
use crate::tui::{BgProc, PendingGate, TuiEvent, UsageStats};
use crossbeam_channel::Receiver;
use tokio::sync::mpsc::UnboundedSender;

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
    /// Rows the approval-gate banner occupied last frame (0 when no gate is
    /// pending). Counted alongside `input_height` as bottom chrome.
    pub gate_height: u16,
}

/// Rendering state computed each frame
#[derive(Debug)]
pub(super) struct RenderCache<'a> {
    pub actions: Vec<(Rect, TuiAction<'a>)>,
}

/// UI display state
#[derive(Debug)]
pub(super) struct UiState {
    pub mouse_enabled: bool,
    pub debug_message: Cow<'static, str>,
    pub is_hovering: bool,
    pub supports_cursor: bool,
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
    /// Inbound events from the SSE bridge (durable entries + ephemeral deltas).
    pub events: Receiver<TuiEvent>,

    /// The input composer (M2). The swappable input seam (DESIGN §10.5).
    pub composer: Composer,
    /// Focus model: true = composing (keys go to the composer), false = command
    /// mode (M1's single-key transcript commands). Esc toggles; a printable key in
    /// command mode returns to composing. Starts focused.
    pub composing: bool,
    /// Sends a submitted turn to the tokio side, which `POST`s it to the daemon.
    pub submit_tx: UnboundedSender<String>,
    /// The pending tool-approval gate, if any (polled from `GET /state`, M3).
    pub pending_gate: Option<PendingGate>,
    /// Sends an approval decision (`call_id`, decision) to the tokio side, which
    /// resolves it via `POST /approvals/:call_id`.
    pub approve_tx: UnboundedSender<(String, Approval)>,
    /// The live background processes (DESIGN §6), polled from `GET /state`. Rendered
    /// as a persistent panel above the composer; empty hides it. Ephemeral chrome —
    /// never part of the transcript document, so it never triggers a reproject.
    pub processes: Vec<BgProc>,
    /// The cumulative usage statistics (polled from `GET /state`). Rendered in the
    /// status bar. Ephemeral chrome — reflects API telemetry from the daemon.
    pub usage: Option<UsageStats>,

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

impl<'a> InteractiveState<'a> {
    /// Create new interactive state. The document starts empty; it is filled in
    /// as [`TuiEvent`]s arrive over `events`.
    pub(super) fn new(
        events: Receiver<TuiEvent>,
        submit_tx: UnboundedSender<String>,
        approve_tx: UnboundedSender<(String, Approval)>,
        render_context: RenderContext,
        theme: InteractiveTheme,
    ) -> Self {
        let current_theme_name = render_context
            .current_theme_name()
            .as_ref()
            .map(|s| s.to_string());
        Self {
            document: DocumentState {
                document: Document::new(),
            },
            viewport: ViewportState {
                scroll_offset: 0,
                cursor_pos: None,
                clicked_position: None,
                cached_layout: None,
                last_viewport_height: 0,
                scrollbar_hovered: false,
                scrollbar_dragging: false,
                keyboard_cursor: KeyboardCursor::VirtualTop,
                stuck_to_bottom: true,
                input_height: 1,
                gate_height: 0,
            },
            render_cache: RenderCache {
                actions: Vec::new(),
            },
            layout: LayoutState {
                pos: Position::default(),
                indent: 0,
                node_path: NodePath::new(),
                area: Rect::default(),
                blockquote_markers: Vec::new(),
            },
            ui_mode: UiMode::Normal,
            ui: UiState {
                mouse_enabled: true,
                debug_message: "efference — q:quit ?:help t:theme  j/k:scroll  ⏎/space:expand"
                    .into(),
                is_hovering: false,
                supports_cursor: supports_cursor_shape(),
            },
            loading: LoadingState {
                pending_request: false,
                started_at: Instant::now(),
                was_loading: false,
            },
            log: Vec::new(),
            live: LiveTail::default(),
            events,
            composer: Composer::new(),
            composing: true,
            submit_tx,
            pending_gate: None,
            approve_tx,
            processes: Vec::new(),
            usage: None,
            dirty: false,
            reproject_pending: false,
            render_context,
            theme,
            current_theme_name,
        }
    }

    /// Handle a composer submission. When a gate is pending, the text resolves it
    /// with the same three-valued semantics as the `attach` REPL — empty approves,
    /// `.abort` aborts, anything else redirects the model. Otherwise it's a normal
    /// user turn (`POST /turn`).
    pub(super) fn submit_composer(&mut self, text: String) {
        if let Some(gate) = self.pending_gate.take() {
            let decision = match text.trim() {
                "" => Approval::Approve,
                ".abort" => Approval::Abort,
                other => Approval::Redirect(other.to_string()),
            };
            let label = match &decision {
                Approval::Approve => "approved".to_string(),
                Approval::Abort => "aborted".to_string(),
                Approval::Redirect(_) => "redirected".to_string(),
            };
            let _ = self.approve_tx.send((gate.call_id, decision));
            self.composer.clear();
            self.ui.debug_message = label.into();
            self.viewport.stuck_to_bottom = true;
            return;
        }

        if self.submit_tx.send(text).is_ok() {
            self.composer.clear();
            self.ui.debug_message = "sent".into();
            // Follow the new turn as it streams in.
            self.viewport.stuck_to_bottom = true;
        } else {
            self.ui.debug_message = "could not send (daemon gone?)".into();
        }
    }

    /// Apply one inbound event: fold it into the log or live tail and flag what the
    /// UI loop must do next. Returns true if the UI should exit.
    ///
    /// This never re-projects or paints itself — it only updates state and sets
    /// [`reproject_pending`](Self::reproject_pending) / [`dirty`](Self::dirty). The
    /// loop drains a whole arriving burst through here, then re-projects and paints
    /// at most once per redraw tick. That deferral is what keeps a reattach replay
    /// (or a fast token stream) from doing O(N²) projection work.
    pub(super) fn handle_event(&mut self, event: TuiEvent) -> bool {
        match event {
            TuiEvent::Entry(entry) => {
                // A committed entry supersedes whatever was streamed live for it.
                self.live.commit(&entry);
                // Entries can arrive out of order across the durable/replay seam;
                // keep the log sorted by seq so the projection is stable.
                match self.log.binary_search_by_key(&entry.seq, |e| e.seq) {
                    Ok(idx) => self.log[idx] = entry,
                    Err(idx) => self.log.insert(idx, entry),
                }
                self.loading.pending_request = false;
                self.mark_reproject();
            }
            TuiEvent::AssistantDelta(text) => {
                self.live.assistant.push_str(&text);
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ToolOutput { call_id, chunk } => {
                self.live.push_tool_output(&call_id, &chunk);
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::Gate(gate) => {
                // A gate is a control-state change, not a log change — no reproject,
                // but the banner still needs a paint.
                if let Some(g) = &gate {
                    self.ui.debug_message = format!(
                        "approval needed: {} — ⏎ approve · type to redirect · .abort",
                        g.name
                    )
                    .into();
                }
                self.pending_gate = gate;
                self.dirty = true;
            }
            TuiEvent::Processes(processes) => {
                // Ephemeral chrome, not transcript content — repaint, no reproject
                // (the running tail is never in the log, DESIGN §6).
                self.processes = processes;
                self.dirty = true;
            }
            TuiEvent::Usage(usage) => {
                // Ephemeral telemetry, not transcript content — update and redraw, no
                // reproject. Usage stats are rendered in the status bar.
                self.usage = Some(usage);
                return false;
            }
            TuiEvent::Disconnected => return true,
        }
        false
    }

    /// Flag that the transcript must be re-projected before the next paint.
    fn mark_reproject(&mut self) {
        self.reproject_pending = true;
        self.dirty = true;
    }

    /// Re-project the transcript from the log + live tail if a change is pending,
    /// then clear the flag. Called once before each paint, so a drained burst of
    /// events collapses to a single projection. The cached layout is invalidated;
    /// the next frame recomputes `document_height` and the UI loop re-pins the
    /// bottom if [`stuck_to_bottom`](ViewportState::stuck_to_bottom).
    pub(super) fn flush_reproject(&mut self) {
        if self.reproject_pending {
            self.reproject();
            self.reproject_pending = false;
        }
    }

    /// Rebuild the transcript document from the log + live tail unconditionally.
    /// Prefer [`flush_reproject`](Self::flush_reproject) on the hot path; this is
    /// for callers that must reproject immediately (e.g. tests, an expand action).
    pub(super) fn reproject(&mut self) {
        self.document.document = projection::project(&self.log, &self.live);
        self.viewport.cached_layout = None;
    }

    pub(super) fn set_debug_message(&mut self, message: impl Into<Cow<'static, str>>) {
        if !self.loading.pending_request {
            self.ui.debug_message = message.into();
        }
    }

    /// Apply a theme by name, rebuilding the interactive theme
    pub(super) fn apply_theme(&mut self, theme_name: &str) -> Result<(), ThemeError> {
        self.render_context.set_theme_name(theme_name)?;
        self.theme = InteractiveTheme::from_render_context(&self.render_context);
        self.current_theme_name = Some(theme_name.to_string());
        Ok(())
    }

    /// Set scroll offset with automatic clamping to valid range
    pub(super) fn set_scroll_offset(&mut self, offset: u16) {
        self.viewport.scroll_offset = offset;
        // Clamp to valid range if we have layout info
        if let Some(cache) = self.viewport.cached_layout {
            let max_scroll = cache
                .document_height
                .saturating_sub(self.viewport.last_viewport_height);
            self.viewport.scroll_offset = self.viewport.scroll_offset.min(max_scroll);
        }
    }

    /// The maximum scroll offset given the cached layout (0 if unknown).
    pub(super) fn max_scroll(&self) -> u16 {
        self.viewport
            .cached_layout
            .map(|cache| {
                cache
                    .document_height
                    .saturating_sub(self.viewport.last_viewport_height)
            })
            .unwrap_or(0)
    }

    /// Pin the viewport to the bottom (used while streaming, when stuck_to_bottom).
    pub(super) fn scroll_to_bottom(&mut self) {
        self.viewport.scroll_offset = self.max_scroll();
    }

    /// Check if position is in the scrollbar column
    pub(super) fn is_in_scrollbar(&self, pos: Position, content_area_width: u16) -> bool {
        // Scrollbar is at content_area_width (which is frame.width - 1)
        pos.x == content_area_width && pos.y < self.viewport.last_viewport_height
    }

    /// Check if scrollbar should be visible (document taller than viewport)
    pub(super) fn scrollbar_visible(&self) -> bool {
        self.viewport
            .cached_layout
            .map(|cache| cache.document_height > self.viewport.last_viewport_height)
            .unwrap_or(false)
    }

    /// Check if a link (by action index) is visible in the current viewport
    pub(super) fn is_link_visible(&self, action_index: usize) -> Option<bool> {
        let (rect, _) = self.render_cache.actions.get(action_index)?;
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        // Link is visible if its rect overlaps with the viewport
        Some(rect.y < viewport_bottom && rect.bottom() > viewport_top)
    }

    /// Determine if a focused link is above or below the viewport
    ///
    /// Returns Some(true) if above, Some(false) if below, None if visible or invalid index.
    pub(super) fn is_link_off_screen(&self, action_index: usize) -> Option<bool> {
        let (rect, _) = self.render_cache.actions.get(action_index)?;
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        if rect.bottom() <= viewport_top {
            Some(true) // Above viewport
        } else if rect.y >= viewport_bottom {
            Some(false) // Below viewport
        } else {
            None // Visible
        }
    }

    /// Find the first visible link index
    pub(super) fn first_visible_link(&self) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Find the last visible link index
    pub(super) fn last_visible_link(&self) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Find the next visible link below the current one
    pub(super) fn next_visible_link(&self, current_index: usize) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .skip(current_index + 1)
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Find the previous visible link above the current one
    pub(super) fn prev_visible_link(&self, current_index: usize) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .take(current_index)
            .rev()
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Reset keyboard cursor (called on navigation to new document)
    pub(super) fn reset_keyboard_cursor(&mut self) {
        self.viewport.keyboard_cursor = KeyboardCursor::VirtualTop;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render_context::RenderContext;
    use tokio::sync::mpsc::UnboundedReceiver;

    /// A state with one pending gate, plus the receiver the decision lands on.
    fn state_with_gate() -> (
        InteractiveState<'static>,
        UnboundedReceiver<(String, Approval)>,
    ) {
        let rc = RenderContext::new();
        let theme = InteractiveTheme::from_render_context(&rc);
        let (_etx, erx) = crossbeam_channel::unbounded();
        let (stx, _srx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = InteractiveState::new(erx, stx, atx, rc, theme);
        s.pending_gate = Some(PendingGate {
            call_id: "toolu_1".into(),
            name: "bash".into(),
            input: serde_json::json!({ "command": "ls" }),
        });
        (s, arx)
    }

    #[test]
    fn empty_submit_approves_the_gate() {
        let (mut s, mut arx) = state_with_gate();
        s.submit_composer(String::new());
        let (id, dec) = arx.try_recv().expect("a decision was sent");
        assert_eq!(id, "toolu_1");
        assert!(matches!(dec, Approval::Approve));
        assert!(s.pending_gate.is_none(), "gate cleared after resolving");
    }

    #[test]
    fn abort_sentinel_aborts_the_gate() {
        let (mut s, mut arx) = state_with_gate();
        s.submit_composer(".abort".into());
        let (_id, dec) = arx.try_recv().unwrap();
        assert!(matches!(dec, Approval::Abort));
    }

    #[test]
    fn other_text_redirects_the_gate() {
        let (mut s, mut arx) = state_with_gate();
        s.submit_composer("use rg instead".into());
        match arx.try_recv().unwrap().1 {
            Approval::Redirect(m) => assert_eq!(m, "use rg instead"),
            other => panic!("expected redirect, got {other:?}"),
        }
    }

    #[test]
    fn submit_without_gate_is_a_normal_turn() {
        let rc = RenderContext::new();
        let theme = InteractiveTheme::from_render_context(&rc);
        let (_etx, erx) = crossbeam_channel::unbounded();
        let (stx, mut srx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = InteractiveState::new(erx, stx, atx, rc, theme);
        s.submit_composer("hello".into());
        assert_eq!(srx.try_recv().unwrap(), "hello", "went to the turn channel");
        assert!(arx.try_recv().is_err(), "not to the approval channel");
    }
}
