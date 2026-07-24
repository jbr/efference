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
use crate::tui::composer::Composer;
use crate::tui::document::{Document, DocumentNode, NodePath, TuiAction};
use crate::tui::projection::{self, LiveTail, Phase};
use crate::tui::render_context::{RenderContext, ThemeError};
use crate::tui::{BgProc, PaceCmd, PaceView, PendingGate, TuiEvent, UsageStats};
use crossbeam_channel::Receiver;
use tokio::sync::mpsc::UnboundedSender;

/// The idle status-bar hint (also restored when an approval gate resolves).
const IDLE_STATUS: &str = "efference — q:quit ?:help t:theme  j/k:scroll  ⏎/space:expand";

/// Cap on a process-panel row's live tail between `/state` polls (TUI-bg-2). The
/// panel only renders the last line, and a poll reconciles to the authoritative
/// snapshot every ~400ms, so this just bounds the gap-filling append.
const BG_TAIL_CAP: usize = 4096;

/// The between-turns speed rungs the `+`/`-` keys step through (ms)
/// (docs/tui.md). `0` is as-fast-as-possible; the ladder is coarse on
/// purpose — a felt "slower/faster," not a precise slider. Tunable in the end-of-sequence ergonomics
/// pass.
const PACE_LADDER: &[u64] = &[0, 500, 1_000, 2_000, 5_000, 10_000, 30_000];

/// A short human label for a speed dial value: `fast` at zero, else `2s` / `500ms`.
fn pace_label(speed_ms: u64, held: bool) -> String {
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
    /// Retired: the approval gate moved inline into the transcript (docs/tui.md),
    /// so it no longer reserves a bottom-chrome row. Kept (always 0) so the mouse
    /// hit-testing math reads uniformly as `1 + input_height + gate_height`.
    pub gate_height: u16,
    /// The right-docked sidebar's rectangle as of the last frame, or `None` when it's
    /// hidden (narrow terminal). Remembered so the sidebar-only dev screenshot can
    /// slice exactly those columns (docs/tui.md).
    pub sidebar_area: Option<Rect>,
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
    /// Per-exchange view-state the human has toggled (expand/collapse, raw-vs-wire),
    /// kept here so it survives re-projection (docs/tui.md): the transcript rebuilds
    /// on every streamed token, so a block's toggles can't live in the rebuilt
    /// `Document`. `project` reads it, keyed by `call_id`.
    pub view: projection::ViewState,
    /// Inbound events from the SSE bridge (durable entries + ephemeral deltas).
    pub events: Receiver<TuiEvent>,

    /// The input composer (M2). The swappable input interface (docs/tui.md).
    pub composer: Composer,
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
    /// resolves it via `POST /approvals/:call_id`.
    pub approve_tx: UnboundedSender<(String, Approval)>,
    /// Sends a pace command (`speed_ms`, `held`) to the tokio side, which `POST`s it to
    /// `/pace` (docs/tui.md).
    pub pace_tx: UnboundedSender<PaceCmd>,
    /// Fires the operator's interrupt intent at the in-flight tool: `Kill` →
    /// `POST /interrupt` (the hard rung of graduated Esc, docs/tui.md), `Detach` →
    /// `POST /background` (the foreground→background handoff, docs/supervisor.md).
    /// A no-op if nothing is running.
    pub interrupt_tx: UnboundedSender<Interrupt>,
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

impl<'a> InteractiveState<'a> {
    /// Create new interactive state. The document starts empty; it is filled in
    /// as [`TuiEvent`]s arrive over `events`.
    pub(super) fn new(
        events: Receiver<TuiEvent>,
        submit_tx: UnboundedSender<String>,
        approve_tx: UnboundedSender<(String, Approval)>,
        pace_tx: UnboundedSender<PaceCmd>,
        interrupt_tx: UnboundedSender<Interrupt>,
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
                sidebar_area: None,
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
                debug_message: IDLE_STATUS.into(),
                is_hovering: false,
                supports_cursor: supports_cursor_shape(),
                quit_requested: false,
            },
            loading: LoadingState {
                pending_request: false,
                started_at: Instant::now(),
                was_loading: false,
            },
            log: Vec::new(),
            live: LiveTail::default(),
            view: projection::ViewState::default(),
            events,
            composer: Composer::new(),
            composing: true,
            strip_focus: None,
            staging: None,
            strip_hits: Vec::new(),
            block_menu_focus: 0,
            block_menu_hits: Vec::new(),
            submit_tx,
            pending_turns: Vec::new(),
            pending_gate: None,
            approve_tx,
            pace_tx,
            interrupt_tx,
            loop_running: false,
            pace: None,
            processes: Vec::new(),
            usage: None,
            policy: None,
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
    /// The `ConfigChange` a `.allow` at the gate proposes (docs/permissions.md). An
    /// explicit `spec` is taken as a bash command-prefix to allow; bare `.allow`
    /// derives it from the gated call — the program for a `bash` call, else the tool's
    /// own name. `None` when nothing sensible can be formed (the caller falls back to a
    /// redirect). Path/scope grants need the richer surface; this is the common case.
    fn allow_change_impl(gate: &PendingGate, spec: &str) -> Option<crate::log::ConfigChange> {
        use crate::log::ConfigChange;
        if !spec.is_empty() {
            return Some(ConfigChange::Command {
                command: spec.to_string(),
            });
        }
        if gate.name == "bash" {
            let cmd = gate.input.get("command").and_then(|c| c.as_str())?;
            let program = cmd.split_whitespace().next()?;
            Some(ConfigChange::Command {
                command: program.to_string(),
            })
        } else {
            Some(ConfigChange::Tool {
                name: gate.name.clone(),
            })
        }
    }

    pub(super) fn submit_composer(&mut self, text: String) {
        if let Some(gate) = self.pending_gate.take() {
            let trimmed = text.trim();
            let decision = if trimmed.is_empty() {
                Approval::Approve
            } else if trimmed == ".abort" {
                Approval::Abort
            } else if trimmed == ".allow" || trimmed.starts_with(".allow ") {
                // Add a rule, then run the call iff it now clears (docs/permissions.md).
                // An explicit prefix (`.allow git log`) grants that bash command prefix;
                // bare `.allow` derives the grant from the gated call (the bash program,
                // or a structured tool's name). The mechanism re-checks — nothing rides
                // along unchecked. Written to the session's config file like any commit.
                let spec = trimmed.strip_prefix(".allow").unwrap_or("").trim();
                match Self::allow_change_impl(&gate, spec) {
                    Some(change) => Approval::ConfigEdit {
                        change,
                        note: "allowed at the gate".into(),
                    },
                    // Couldn't form a grant (e.g. bare `.allow` on a path-only tool) —
                    // fall back to steering the model rather than guessing a rule.
                    None => Approval::Redirect(trimmed.to_string()),
                }
            } else {
                Approval::Redirect(trimmed.to_string())
            };
            let label = match &decision {
                Approval::Approve => "approved".to_string(),
                Approval::Abort => "aborted".to_string(),
                Approval::Redirect(_) => "redirected".to_string(),
                // The composer doesn't author grants (that needs a richer surface); kept
                // exhaustive so a future grant-capable client labels cleanly.
                Approval::ConfigEdit { .. } => "granted".to_string(),
            };
            let _ = self.approve_tx.send((gate.call_id, decision));
            self.composer.clear();
            self.ui.debug_message = label.into();
            self.viewport.stuck_to_bottom = true;
            return;
        }

        if self.submit_tx.send(text.clone()).is_ok() {
            // Echo the message immediately as a pending transcript unit
            // (docs/tui.md): it's handed to the daemon but not yet a
            // committed `User` entry, so it renders dimmed until its log entry streams back
            // via `/events`. Reproject now so the echo appears on this keypress rather than
            // waiting for the next daemon event.
            self.pending_turns.push(text);
            self.composer.clear();
            self.ui.debug_message = "sent".into();
            // Follow the new turn as it streams in.
            self.viewport.stuck_to_bottom = true;
            self.mark_reproject();
        } else {
            self.ui.debug_message = "could not send (daemon gone?)".into();
        }
    }

    /// Step the between-turns speed dial along [`PACE_LADDER`] and push it to the daemon
    /// (docs/tui.md — the `+`/`-` keys). `slower` (`+`) moves up the
    /// ladder, else down toward as-fast-as-possible. The hold overlay is left untouched (a change
    /// while held recolors resume). Optimistically updates the local pace so the status bar reflects
    /// it on this keypress; the `/state` poll reconciles.
    pub(super) fn nudge_pace_speed(&mut self, slower: bool) {
        let cur = self.pace.map(|p| p.speed_ms).unwrap_or(0);
        let held = self.pace.map(|p| p.held).unwrap_or(false);
        // The ladder rung at or just below the current speed, then step off it.
        let idx = PACE_LADDER.iter().rposition(|&ms| ms <= cur).unwrap_or(0);
        let next = if slower {
            PACE_LADDER.get(idx + 1).copied().unwrap_or(cur) // clamp at the slowest rung
        } else {
            PACE_LADDER.get(idx.saturating_sub(1)).copied().unwrap_or(0)
        };
        let _ = self.pace_tx.send((Some(next), None));
        self.pace = Some(PaceView {
            speed_ms: next,
            held,
        });
        self.ui.debug_message = format!("pace · {}", pace_label(next, held)).into();
        self.dirty = true;
    }

    /// Toggle the pause overlay and push it to the daemon (docs/tui.md —
    /// the hold/resume key). The speed dial is retained underneath (resume returns to it).
    /// Optimistic like [`nudge_pace_speed`](Self::nudge_pace_speed).
    pub(super) fn toggle_pace_hold(&mut self) {
        let cur = self.pace.unwrap_or(PaceView {
            speed_ms: 0,
            held: false,
        });
        let held = !cur.held;
        let _ = self.pace_tx.send((None, Some(held)));
        self.pace = Some(PaceView { held, ..cur });
        self.ui.debug_message = if held {
            "paused — ⏸ HELD (resume: same key, or send a message)".into()
        } else {
            "resumed".into()
        };
        self.dirty = true;
    }

    /// Whether the loop is doing something an Esc could pause or interrupt
    /// (docs/tui.md): a request is streaming (`pending_request`, event-driven and immediate) **or**
    /// the daemon reports it is executing a turn (`loop_running`, polled — the signal that survives
    /// a *silent* long-running tool, where streaming reads false). The union catches both the fast
    /// edge and the silent middle.
    pub(super) fn is_loop_active(&self) -> bool {
        self.loading.pending_request || self.loop_running
    }

    /// Graduated Esc (docs/tui.md): a **state machine**, not a press-count — each Esc's effect is a
    /// function of the current state, so no counter, timer, or reset is needed. Precedence, gentlest
    /// first:
    ///
    /// 1. **not at rest in the composer** (an overlay is open, or focus is in the transcript) →
    ///    return to the composer.
    /// 2. **at the composer, held** → interrupt the in-flight tool (`POST /interrupt`).
    /// 3. **at the composer, not held, loop active** → pause/hold.
    /// 4. **at the composer, not held, nothing happening** → swallow (Esc has nothing to do).
    ///
    /// So sitting in the composer while the agent works, one Esc pauses and a second interrupts;
    /// and jamming Esc from anywhere still walks return-to-composer → pause → interrupt.
    pub(super) fn handle_escape(&mut self) {
        // A stager in progress is the gentlest thing Esc can undo: cancel it and
        // restore the parked draft, before any return/pause/interrupt rung.
        if self.staging.is_some() {
            self.cancel_staging();
            return;
        }
        let at_rest_composer = self.composing && matches!(self.ui_mode, UiMode::Normal);
        if !at_rest_composer {
            self.normalize_to_composer();
            return;
        }
        let held = self.pace.is_some_and(|p| p.held);
        if held {
            self.esc_interrupt();
        } else if self.is_loop_active() {
            self.esc_hold();
        }
        // else: nothing happening → swallow (a silent no-op, by design).
    }

    /// Return to the composer's steady state, from anywhere (docs/tui.md) — close an overlay (help /
    /// theme picker), or from transcript navigation refocus the composer. Esc is one-directional
    /// toward the composer; this is the gentlest Esc step and the first thing it does when the UI
    /// isn't already at rest there.
    fn normalize_to_composer(&mut self) {
        match std::mem::replace(&mut self.ui_mode, UiMode::Normal) {
            UiMode::Help => {}
            UiMode::ThemePicker {
                saved_theme_name, ..
            } => {
                let _ = self.apply_theme(&saved_theme_name);
            }
            UiMode::Normal => {
                self.composing = true;
                self.strip_focus = None;
                self.viewport.keyboard_cursor = KeyboardCursor::VirtualTop;
                self.ui.debug_message = "compose — ⏎ send · ^J newline · ↑ navigate".into();
            }
        }
    }

    /// Approve the pending gate without touching the composer — the control
    /// strip's `approve` verb (docs/tui.md). Unlike an empty-composer `⏎` submit,
    /// this leaves any drafted text in place (the draft may be a redirect the
    /// operator decided against sending).
    pub(super) fn approve_gate(&mut self) {
        if let Some(gate) = self.pending_gate.take() {
            let _ = self.approve_tx.send((gate.call_id, Approval::Approve));
            self.ui.debug_message = "approved".into();
            self.viewport.stuck_to_bottom = true;
        }
    }

    /// Abort the pending gate — the strip's `abort` verb, same decision the typed
    /// `.abort` sentinel produces, draft likewise left in place.
    pub(super) fn abort_gate(&mut self) {
        if let Some(gate) = self.pending_gate.take() {
            let _ = self.approve_tx.send((gate.call_id, Approval::Abort));
            self.ui.debug_message = "aborted".into();
            self.viewport.stuck_to_bottom = true;
        }
    }

    /// Open the theme picker on the current theme — shared by the `t` key and the
    /// strip's `theme` item.
    pub(super) fn open_theme_picker(&mut self) {
        let themes = crate::tui::render_context::RenderContext::available_themes();
        let current_theme = self
            .current_theme_name
            .clone()
            .or_else(|| themes.first().cloned())
            .unwrap_or_else(|| "default".to_string());
        let selected_index = themes.iter().position(|t| t == &current_theme).unwrap_or(0);
        self.ui_mode = UiMode::ThemePicker {
            selected_index,
            saved_theme_name: current_theme,
        };
        self.ui.debug_message =
            "Select theme (↑/↓ to navigate, Enter to save, Esc to cancel)".into();
    }

    /// The pause step of graduated Esc (docs/tui.md): set the `held` overlay (unit 2) and push it to
    /// the daemon so the loop stops at the next juncture. Idempotent (ensures held whatever the current
    /// pace), optimistic like the pace keys. The hint names both ways forward: another Esc stops a
    /// running tool now, and typing a message resumes (submit always resumes, unit 2).
    fn esc_hold(&mut self) {
        let cur = self.pace.unwrap_or(PaceView {
            speed_ms: 0,
            held: false,
        });
        let _ = self.pace_tx.send((None, Some(true)));
        self.pace = Some(PaceView { held: true, ..cur });
        self.ui.debug_message =
            "⏸ HELD — esc again to stop the running tool · type to resume".into();
        self.dirty = true;
    }

    /// The interrupt step of graduated Esc (docs/tui.md): abort the in-flight tool now — fire
    /// `POST /interrupt`, which kills the running tool's process group (a no-op if nothing is
    /// running). Reaches the one point pace can't (mid-tool).
    pub(super) fn esc_interrupt(&mut self) {
        let _ = self.interrupt_tx.send(Interrupt::Kill);
        self.ui.debug_message = "interrupting the running tool…".into();
        self.dirty = true;
    }

    /// The foreground→background handoff key (docs/supervisor.md): ask the daemon
    /// to detach the in-flight tool into the background supervisor — keep it
    /// running as a `bg#<n>` process instead of killing it or waiting it out.
    /// Fire-and-forget like the interrupt: the daemon no-ops if nothing is
    /// mid-tool. A message typed right after rides the seam-drain, so it reaches
    /// the model alongside the handoff — no separate note-capture flow.
    pub(super) fn background_detach(&mut self) {
        if self.is_loop_active() {
            let _ = self.interrupt_tx.send(Interrupt::Detach);
            self.ui.debug_message =
                "moving the running tool to the background… · type to follow up".into();
        } else {
            self.ui.debug_message = "nothing running to background".into();
        }
        self.dirty = true;
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
                // A committed user turn resolves its optimistic pending echo
                // (docs/tui.md): the entry *is* on the wire now
                // (`project(log)` is exactly what the request sends), so drop the matching
                // queued echo. First-match on the ordered queue keeps ordering correct even
                // when the same message was sent twice.
                if let Payload::User(blocks) = &entry.payload {
                    let text = projection::join_text(blocks);
                    if let Some(pos) = self.pending_turns.iter().position(|p| *p == text) {
                        self.pending_turns.remove(pos);
                    }
                }
                // Entries can arrive out of order across the durable/replay boundary;
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
                // Prose arriving *is* the progress signal, so it supersedes any phase line.
                self.live.phase = None;
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ThinkingDelta(text) => {
                match &mut self.live.phase {
                    Some(Phase::Thinking { text: buf }) => buf.push_str(&text),
                    _ => self.live.phase = Some(Phase::Thinking { text }),
                }
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ToolCallStart { name } => {
                // Replaces a thinking phase: the model has moved on to acting.
                self.live.phase = Some(Phase::CallingTool {
                    name,
                    input: String::new(),
                });
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ToolCallInput { fragment } => {
                if let Some(Phase::CallingTool { input, .. }) = &mut self.live.phase {
                    input.push_str(&fragment);
                    self.mark_reproject();
                }
            }
            TuiEvent::ToolOutput { call_id, chunk } => {
                self.live.push_tool_output(&call_id, &chunk);
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::Gate(gate) => {
                // The gate renders inline in the transcript (the tail exchange shows
                // `⏸ awaiting approval`, docs/tui.md), so a gate change must
                // reproject — unlike the other ephemeral chrome (processes/usage). The
                // status bar still mirrors the affordance for the scrolled-away case.
                self.ui.debug_message = match &gate {
                    Some(g) => {
                        format!("approval needed: {} — ⏎ approve · type to redirect", g.name).into()
                    }
                    // Gate resolved — drop the stale "approval needed" hint.
                    None => IDLE_STATUS.into(),
                };
                // A stager edits an argument *for* the pending gate; if the gate
                // resolves out from under it (another client, a daemon-side abort),
                // committing would fall through to a normal turn submit — cancel
                // instead and restore the parked draft.
                if gate.is_none() && self.staging.is_some() {
                    self.cancel_staging();
                }
                self.pending_gate = gate;
                self.mark_reproject();
            }
            TuiEvent::Processes(processes) => {
                // Ephemeral chrome, not transcript content — repaint, no reproject
                // (the running tail is never in the log, docs/supervisor.md).
                self.processes = processes;
                self.dirty = true;
            }
            TuiEvent::BgOutput { handle, chunk } => {
                // Smooth the panel's tail between `/state` polls (TUI-bg-2). Append to
                // the matching row; the next poll reconciles to the authoritative
                // snapshot. An unknown handle (process spawned since the last poll) is
                // ignored — it appears within one poll interval. Bound the appended
                // tail so a chatty process can't grow it without limit before the next
                // poll trims it; only the last line is shown anyway.
                if let Some(p) = self.processes.iter_mut().find(|p| p.handle == handle) {
                    p.tail.push_str(&chunk);
                    if p.tail.len() > BG_TAIL_CAP {
                        let from = p.tail.len() - BG_TAIL_CAP;
                        // Trim on a char boundary so the panel never slices a codepoint.
                        let from = (from..=p.tail.len())
                            .find(|&i| p.tail.is_char_boundary(i))
                            .unwrap_or(p.tail.len());
                        p.tail.drain(..from);
                    }
                    self.dirty = true;
                }
            }
            TuiEvent::Usage(usage) => {
                // Ephemeral telemetry, not transcript content — update and redraw, no
                // reproject. Usage stats are rendered in the status bar.
                self.usage = Some(usage);
                return false;
            }
            TuiEvent::Policy(policy) => {
                // Ephemeral chrome (the sidebar POLICY section), not transcript content —
                // update and repaint, no reproject.
                self.policy = Some(policy);
                self.dirty = true;
            }
            TuiEvent::Pace(pace) => {
                // Operator-view chrome (the status-bar pace indicator), not transcript content —
                // update and repaint, no reproject (docs/tui.md). Reconciles
                // the optimistic update a pace key made with the daemon's authoritative pace.
                self.pace = Some(pace);
                self.dirty = true;
            }
            TuiEvent::Running(running) => {
                // Not rendered; it's the "is the loop working" signal graduated Esc reads
                // (docs/tui.md). No repaint needed — nothing on screen depends on it directly.
                self.loop_running = running;
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
        let pending_call = self.pending_gate.as_ref().map(|g| g.call_id.as_str());
        let pending_reason = self.pending_gate.as_ref().and_then(|g| g.reason.as_deref());
        self.document.document = projection::project(
            &self.log,
            &self.live,
            &self.view,
            pending_call,
            pending_reason,
            &self.pending_turns,
        );
        self.viewport.cached_layout = None;
    }

    /// Flip the focused tool-result exchange between its wire (distilled) output and
    /// its full raw output (docs/tui.md). Both are already client-side, so this is a
    /// pure re-render — the content source changes, so unlike expand it can't mutate
    /// in place and must reproject. No-op unless a keyed (tool-result) block is
    /// focused; the focused block is the one `⏎` would expand.
    pub(super) fn toggle_raw_focused(&mut self) {
        let path = match self.viewport.keyboard_cursor {
            KeyboardCursor::Focused { action_index } => {
                match self.render_cache.actions.get(action_index) {
                    Some((_, TuiAction::ExpandBlock(p))) => *p,
                    _ => return,
                }
            }
            _ => return,
        };
        let call_id = match find_node_at_path_mut(&mut self.document.document.nodes, path.indices())
        {
            Some(DocumentNode::TruncatedBlock { key: Some(k), .. }) => k.to_string(),
            _ => return,
        };
        let now_raw = self.view.toggle_raw(&call_id);
        self.ui.debug_message = if now_raw {
            "showing raw output — r to show the distilled view".into()
        } else {
            "showing distilled output — r to show raw".into()
        };
        self.reproject();
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
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let (itx, _irx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = InteractiveState::new(erx, stx, atx, ptx, itx, rc, theme);
        s.pending_gate = Some(PendingGate {
            call_id: "toolu_1".into(),
            name: "bash".into(),
            input: serde_json::json!({ "command": "ls" }),
            reason: None,
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

    /// `.allow <prefix>` at the gate proposes a Command grant for that prefix; the
    /// mechanism (not shown here) re-classifies the call against it (docs/permissions.md).
    #[test]
    fn allow_with_a_prefix_proposes_a_command_grant() {
        let (mut s, mut arx) = state_with_gate();
        s.submit_composer(".allow git log".into());
        match arx.try_recv().unwrap().1 {
            Approval::ConfigEdit { change, .. } => assert_eq!(
                change,
                crate::log::ConfigChange::Command {
                    command: "git log".into()
                }
            ),
            other => panic!("expected a config edit, got {other:?}"),
        }
    }

    /// Bare `.allow` on a `bash` gate derives the grant from the command's program.
    #[test]
    fn bare_allow_grants_the_bash_program() {
        let (mut s, mut arx) = state_with_gate(); // gate is `ls`
        s.submit_composer(".allow".into());
        match arx.try_recv().unwrap().1 {
            Approval::ConfigEdit { change, .. } => assert_eq!(
                change,
                crate::log::ConfigChange::Command {
                    command: "ls".into()
                }
            ),
            other => panic!("expected a config edit, got {other:?}"),
        }
    }

    #[test]
    fn bg_output_smooths_the_matching_process_tail() {
        let (mut s, _arx) = state_with_gate();
        s.processes = vec![BgProc {
            handle: "bg#1".into(),
            command: "cargo watch".into(),
            tail: "tick 1\n".into(),
            running_secs: 3,
            idle_secs: 0,
        }];
        // A delta for the live process appends to its tail (between polls).
        s.handle_event(TuiEvent::BgOutput {
            handle: "bg#1".into(),
            chunk: "tick 2\n".into(),
        });
        assert_eq!(s.processes[0].tail, "tick 1\ntick 2\n");
        assert!(s.dirty, "panel repaints");
        // A delta for an unknown handle is ignored (it appears within one poll).
        s.handle_event(TuiEvent::BgOutput {
            handle: "bg#9".into(),
            chunk: "nope".into(),
        });
        assert_eq!(s.processes[0].tail, "tick 1\ntick 2\n");
    }

    #[test]
    fn bg_output_bounds_the_tail_on_a_char_boundary() {
        let (mut s, _arx) = state_with_gate();
        s.processes = vec![BgProc {
            handle: "bg#1".into(),
            command: "chatty".into(),
            tail: String::new(),
            running_secs: 1,
            idle_secs: 0,
        }];
        // A multibyte burst past the cap must not panic and must stay valid UTF-8.
        let chunk = "é".repeat(BG_TAIL_CAP); // 2 bytes each → 2× the cap
        s.handle_event(TuiEvent::BgOutput {
            handle: "bg#1".into(),
            chunk,
        });
        assert!(s.processes[0].tail.len() <= BG_TAIL_CAP);
        assert!(
            s.processes[0].tail.chars().all(|c| c == 'é'),
            "no codepoint sliced"
        );
    }

    #[test]
    fn submit_without_gate_is_a_normal_turn() {
        let rc = RenderContext::new();
        let theme = InteractiveTheme::from_render_context(&rc);
        let (_etx, erx) = crossbeam_channel::unbounded();
        let (stx, mut srx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let (itx, _irx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = InteractiveState::new(erx, stx, atx, ptx, itx, rc, theme);
        s.submit_composer("hello".into());
        assert_eq!(srx.try_recv().unwrap(), "hello", "went to the turn channel");
        assert!(arx.try_recv().is_err(), "not to the approval channel");
    }
}
