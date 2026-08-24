//! Construction, theme application, and the small state accessors.

use super::*;

impl<'a> InteractiveState<'a> {
    /// Create new interactive state. The document starts empty; it is filled in
    /// as [`TuiEvent`]s arrive over `events`. The argument count is the stateful
    /// surface itself — one receiver plus the intent channels the UI dispatches on
    /// (docs/tui.md) — not an accident to bundle away.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::tui) fn new(
        events: Receiver<TuiEvent>,
        submit_tx: UnboundedSender<String>,
        approve_tx: UnboundedSender<(String, Approval)>,
        pace_tx: UnboundedSender<PaceCmd>,
        interrupt_tx: UnboundedSender<Interrupt>,
        sidebar_tx: UnboundedSender<SidebarCmd>,
        images_tx: UnboundedSender<crate::tui::image::ImageKey>,
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
                layouts: Vec::new(),
                layout_width: 0,
                laid_out: 0,
                highlighted: 0,
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
                last_painted_scroll: 0,
                last_document_height: 0,
                input_height: 1,
                gate_height: 0,
                sidebar_area: None,
                composer_area: None,
                composer_dragging: false,
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
                measuring: false,
                scroll_in_motion: false,
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
            transcript: projection::Transcript::default(),
            view: projection::ViewState::default(),
            events,
            viewing: None,
            highlight_memo: Default::default(),
            images: Default::default(),
            images_tx,
            frame_cost: None,
            composer: Composer::new(),
            clipboard: TextClipboard::new(),
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
            sidebar_tx,
            sidebar_open: false,
            sidebar_focus: None,
            sidebar_return: None,
            sidebar_verb_focus: 0,
            sidebar_hits: Vec::new(),
            sidebar_verb_hits: Vec::new(),
            loop_running: false,
            pace: None,
            processes: Vec::new(),
            usage: None,
            upstream_failure: None,
            policy: None,

            dirty: false,
            reproject_pending: false,
            render_context,
            theme,
            current_theme_name,
        }
    }

    /// Make this a **read-only view** of the recorded session `label` (docs/tui.md —
    /// `efference view`). Set after construction rather than as a `new` parameter, so
    /// the live construction site and every existing scenario stay untouched.
    ///
    /// Focus starts on the control strip: with no composer rendered there is no
    /// composer to rest in, and the strip is the only surface that still carries
    /// verbs (theme, help, close).
    pub(in crate::tui) fn set_viewing(&mut self, label: impl Into<String>) {
        let label = label.into();
        self.ui.debug_message = view_status(&label).into();
        self.viewing = Some(label);
        self.composing = false;
        self.strip_focus = Some(0);
    }

    /// Whether this is a read-only view of a recorded log (no daemon behind it):
    /// every path that would send to a daemon, or focus the composer that sends,
    /// checks this.
    pub(in crate::tui) fn read_only(&self) -> bool {
        self.viewing.is_some()
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

    /// Flip the focused tool result between its wire (distilled) output and
    /// its full raw output (docs/tui.md). Both are already client-side, so this is a
    /// pure re-render — but the content source changes, so it reprojects rather than
    /// editing the document in place. No-op unless a keyed tool-result entry is
    /// focused.
    pub(in crate::tui) fn toggle_raw_focused(&mut self) {
        let path = match self.viewport.keyboard_cursor {
            KeyboardCursor::Focused { action_index } => {
                match self.render_cache.actions.get(action_index) {
                    Some((_, TuiAction::FocusEntry(p))) => *p,
                    _ => return,
                }
            }
            _ => return,
        };
        let call_id = match find_node_at_path_mut(&mut self.document.document.nodes, path.indices())
        {
            Some(DocumentNode::Entry { key: Some(k), .. }) => k.to_string(),
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

    /// Copy the current selection into the TUI clipboard.
    pub(in crate::tui) fn copy_selection(&mut self) {
        let Some(text) = self.composer.selected_text() else {
            self.set_debug_message("nothing selected");
            return;
        };
        let system_written = self.clipboard.copy(text).is_ok();
        self.set_debug_message(if system_written {
            "copied"
        } else {
            "copied (system clipboard unavailable)"
        });
    }

    /// Cut the current selection when present; otherwise retain C-w's word-delete
    /// behavior in the composer.
    pub(in crate::tui) fn cut_selection_or_delete_word(&mut self, key: crossterm::event::KeyEvent) {
        let Some(text) = self.composer.cut_selection() else {
            let _ = self.composer.handle_key(key);
            return;
        };
        let system_written = self.clipboard.copy(text).is_ok();
        self.set_debug_message(if system_written {
            "cut"
        } else {
            "cut (system clipboard unavailable)"
        });
    }

    /// Paste UTF-8 text from the system clipboard, falling back only when system
    /// access fails. Arboard does not distinguish empty from non-text content, so
    /// `ContentNotAvailable` is reported without using stale fallback text.
    pub(in crate::tui) fn paste_clipboard(&mut self) {
        match self.clipboard.paste() {
            crate::tui::clipboard::Paste::Text(text) => {
                self.composer.paste_text(&text);
                self.set_debug_message("pasted");
            }
            crate::tui::clipboard::Paste::Unavailable => {
                if let Some(text) = self.clipboard.fallback().map(str::to_owned) {
                    self.composer.paste_text(&text);
                    self.set_debug_message("pasted (in-process clipboard)");
                } else {
                    self.set_debug_message("clipboard unavailable");
                }
            }
            crate::tui::clipboard::Paste::NoText => {
                self.set_debug_message("clipboard has no text");
            }
        }
    }

    /// Apply a theme by name, rebuilding the interactive theme
    pub(in crate::tui) fn apply_theme(&mut self, theme_name: &str) -> Result<(), ThemeError> {
        self.render_context.set_theme_name(theme_name)?;
        self.theme = InteractiveTheme::from_render_context(&self.render_context);
        self.current_theme_name = Some(theme_name.to_string());
        // Image padding is theme-background-colored (docs/tui.md); re-encode so
        // it follows the new background rather than keeping the old theme's.
        self.images
            .set_theme_background(self.theme.document_bg_style.bg);
        Ok(())
    }
}
