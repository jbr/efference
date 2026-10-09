//! Construction, theme application, and the small state accessors.

use super::*;

#[cfg(test)]
mod test;

impl<'a> InteractiveState<'a> {
    /// Create new interactive state. The document starts empty; it is filled in
    /// as [`TuiEvent`]s arrive over `events`. The intent channels travel as one
    /// [`Senders`](crate::tui::senders::Senders), which is what keeps adding an
    /// intent to one field on each half rather than a parameter on every entry
    /// point (docs/tui.md).
    pub(in crate::tui) fn new(
        events: Receiver<TuiEvent>,
        senders: crate::tui::senders::Senders,
        render_context: RenderContext,
        theme: InteractiveTheme,
    ) -> Self {
        let crate::tui::senders::Senders {
            submit: submit_tx,
            recall: recall_tx,
            attachments: attachments_tx,
            continue_loop: continue_tx,
            fold: fold_tx,
            approve: approve_tx,
            pace: pace_tx,
            interrupt: interrupt_tx,
            sidebar: sidebar_tx,
            images: images_tx,
            host: host_tx,
            attention: _,
        } = senders;
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
                strip_hovered: false,
                strip_hover: None,
                scrollbar_hovered: false,
                scrollbar_dragging: false,
                keyboard_cursor: KeyboardCursor::VirtualTop,
                stuck_to_bottom: true,
                last_painted_scroll: 0,
                last_document_height: 0,
                input_height: 1,
                above_input_height: 0,
                staged_hits: Vec::new(),
                sidebar_area: None,
                doc_origin: Position::default(),
                header_hits: Vec::new(),
                header_hovered: None,
                composer_area: None,
                composer_dragging: false,
            },
            render_cache: RenderCache {
                actions: Vec::new(),
                items: Vec::new(),
            },
            layout: LayoutState {
                pos: Position::default(),
                indent: 0,
                node_path: NodePath::new(),
                area: Rect::default(),
                blockquote_markers: Vec::new(),
                measuring: false,
                scroll_in_motion: false,
                clip: None,
                level: Default::default(),
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
            expanded: std::collections::HashSet::new(),
            events,
            viewing: None,
            dim_register: false,
            highlight_memo: Default::default(),
            images: Default::default(),
            staged_images: Default::default(),
            images_tx,
            frame_cost: None,
            composer: Composer::new(),
            clipboard: TextClipboard::new(),
            composing: true,
            strip_focus: None,
            strip_pinned: None,
            staging: None,
            pace_widget: None,
            strip_hits: Vec::new(),
            block_menu_focus: 0,
            block_menu_hits: Vec::new(),
            submit_tx,
            recall_tx,
            attachments_tx,
            staged: Default::default(),
            clipboard_image: false,
            owns_host: false,
            continue_tx,
            fold_tx,
            cold_consent: false,
            pending_turns: Vec::new(),
            ask_restored: None,
            ask: None,
            pending_gate: None,
            approve_tx,
            pace_tx,
            interrupt_tx,
            sidebar_tx,
            host_tx,
            switch_to: None,
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
            upstream_retry: None,
            stall: None,
            cache_expired: false,
            folding: false,
            session_identity: None,
            policy: None,

            dirty: false,
            reproject_pending: false,
            render_context,
            theme,
            current_theme_name,
        }
    }

    /// Make this a **read-only view** of the recorded session `label` (docs/tui.md —
    /// `efference dev view`). Set after construction rather than as a `new` parameter, so
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
        self.strip_focus = self.leading_strip_verb();
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
    /// Dialect-neutral: reads calls through [`AssistantOutput::tool_calls`], so a
    /// Responses function call is found the same way a Messages `tool_use` is
    /// (docs/log-and-projection.md). That view is derived — a Responses call parses
    /// its argument string on demand — which is why this returns an owned call rather
    /// than a borrow into the log. An entry whose arguments do not parse yields
    /// `None`: the loop records such a turn without executing it, so nothing is in
    /// flight.
    ///
    /// A completion may make several calls (docs/README.md), and the loop appends
    /// *no* result until every call has run and the gate has closed
    /// (docs/redaction-fork.md). So on a multi-call step this reports the entry's first
    /// call for the whole step, not whichever call is currently executing — exact for a
    /// single call; on a batch the cost is a stale label rather than a wrong lifecycle
    /// state. A gate
    /// pending on the call looks the same from here; callers that care check
    /// `pending_gate` first.
    ///
    /// [`AssistantOutput::tool_calls`]: crate::log::AssistantOutput::tool_calls
    pub(in crate::tui) fn in_flight_call(&self) -> Option<crate::log::ToolCall> {
        let (idx, call) = self
            .log
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, entry)| match &entry.payload {
                Payload::Assistant { output, .. } => output
                    .tool_calls()
                    .ok()?
                    .into_iter()
                    .next()
                    .map(|call| (i, call)),
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
        let Some(call_id) = self.focused_entry_key() else {
            return;
        };
        let now_raw = self.view.toggle_raw(&call_id);
        self.ui.debug_message = if now_raw {
            "showing raw output — r to show the distilled view".into()
        } else {
            "showing distilled output — r to show raw".into()
        };
        self.reproject();
    }

    /// Flip the focused *running* tool's output between the trailing-lines peek and
    /// the whole output so far (docs/tui.md). The flag is recorded in
    /// [`ViewState`](crate::tui::projection::ViewState) against the call's
    /// `call_id`, which is the entry's durable key — the block is rebuilt on every
    /// arriving fragment, so a choice held anywhere else lasts one frame. No-op
    /// unless a keyed running-tool entry is focused.
    pub(in crate::tui) fn toggle_peek_focused(&mut self) {
        let Some(call_id) = self.focused_entry_key() else {
            return;
        };
        let now_full = self.view.toggle_expanded(&call_id);
        self.ui.debug_message = if now_full {
            "showing the whole output so far — it keeps growing".into()
        } else {
            "showing the last lines".into()
        };
        self.reproject();
    }

    /// The focused transcript entry's durable key (its `call_id`), if it has one.
    /// Shared by the per-entry verbs so they resolve focus the same way.
    fn focused_entry_key(&mut self) -> Option<String> {
        let KeyboardCursor::Focused { action_index } = self.viewport.keyboard_cursor else {
            return None;
        };
        let path = match self.render_cache.actions.get(action_index) {
            Some((_, TuiAction::FocusEntry(p))) => *p,
            _ => return None,
        };
        match find_node_at_path_mut(&mut self.document.document.nodes, path.indices()) {
            Some(DocumentNode::Entry { key: Some(k), .. }) => Some(k.to_string()),
            _ => None,
        }
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

    /// Apply a theme by name, rebuilding the interactive theme
    pub(in crate::tui) fn apply_theme(&mut self, theme_name: &str) -> Result<(), ThemeError> {
        self.render_context.set_theme_name(theme_name)?;
        self.theme = InteractiveTheme::from_render_context(&self.render_context);
        self.current_theme_name = Some(theme_name.to_string());
        // Image padding is theme-background-colored (docs/tui.md); re-encode so
        // it follows the new background rather than keeping the old theme's.
        self.images
            .set_theme_background(self.theme.document_bg_style.bg);
        self.staged_images
            .set_theme_background(self.theme.document_bg_style.bg);
        Ok(())
    }
}
