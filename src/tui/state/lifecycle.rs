//! Construction, theme application, and the small state accessors.

use super::*;

impl<'a> InteractiveState<'a> {
    /// Create new interactive state. The document starts empty; it is filled in
    /// as [`TuiEvent`]s arrive over `events`.
    pub(in crate::tui) fn new(
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
            pace_widget: None,
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

    /// Whether the loop is doing something an Esc could pause or interrupt
    /// (docs/tui.md): a request is streaming (`pending_request`, event-driven and immediate) **or**
    /// the daemon reports it is executing a turn (`loop_running`, polled — the signal that survives
    /// a *silent* long-running tool, where streaming reads false). The union catches both the fast
    /// edge and the silent middle.
    pub(in crate::tui) fn is_loop_active(&self) -> bool {
        self.loading.pending_request || self.loop_running
    }

    /// The tool call currently executing, derived from the committed log alone —
    /// the last `Assistant` entry's call, if no `ToolResult` has paired it yet.
    /// `None` while the model is generating (nothing called yet, or the last call
    /// already resolved).
    ///
    /// No daemon round-trip: the loop appends the `Assistant` entry *before* it
    /// runs the call and the `ToolResult` after (`src/agent.rs`), so the unpaired
    /// tail of the log the TUI already holds is exactly "what is running now."
    ///
    /// `disable_parallel_tool_use` asks for one call per turn but cannot enforce it
    /// (docs/README.md), so an entry may carry several — and the loop appends *no*
    /// result until every call has run and the gate has closed
    /// (docs/redaction-fork.md). So on a multi-call step this reports the entry's first
    /// call for the whole step, not whichever call is currently executing. Exact in the
    /// single-call case, which is the norm; approximate on the rare multi-call one, where
    /// the cost is a slightly stale label rather than a wrong lifecycle state. A gate
    /// pending on the call looks the same from here; callers that care check
    /// `pending_gate` first.
    pub(in crate::tui) fn in_flight_call(&self) -> Option<&crate::log::ToolCall> {
        let (idx, call) = self
            .log
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, entry)| match &entry.payload {
                Payload::Assistant { calls, .. } => calls.first().map(|c| (i, c)),
                _ => None,
            })?;
        let paired = self.log[idx + 1..].iter().any(|entry| {
            matches!(&entry.payload, Payload::ToolResult { call_id, .. } if *call_id == call.id)
        });
        (!paired).then_some(call)
    }

    /// Open the theme picker on the current theme — shared by the `t` key and the
    /// strip's `theme` item.
    pub(in crate::tui) fn open_theme_picker(&mut self) {
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

    /// Flip the focused tool-result exchange between its wire (distilled) output and
    /// its full raw output (docs/tui.md). Both are already client-side, so this is a
    /// pure re-render — the content source changes, so unlike expand it can't mutate
    /// in place and must reproject. No-op unless a keyed (tool-result) block is
    /// focused; the focused block is the one `⏎` would expand.
    pub(in crate::tui) fn toggle_raw_focused(&mut self) {
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

    pub(in crate::tui) fn set_debug_message(&mut self, message: impl Into<Cow<'static, str>>) {
        if !self.loading.pending_request {
            self.ui.debug_message = message.into();
        }
    }

    /// Apply a theme by name, rebuilding the interactive theme
    pub(in crate::tui) fn apply_theme(&mut self, theme_name: &str) -> Result<(), ThemeError> {
        self.render_context.set_theme_name(theme_name)?;
        self.theme = InteractiveTheme::from_render_context(&self.render_context);
        self.current_theme_name = Some(theme_name.to_string());
        Ok(())
    }
}
