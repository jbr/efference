use ratatui::{
    Frame,
    layout::{Position, Rect},
};

use super::{InteractiveState, UiMode};
use crate::tui::document::NodePath;

impl<'a> InteractiveState<'a> {
    pub(super) fn render_frame(&mut self, frame: &mut Frame) {
        let full = frame.area();

        // Bottom chrome, top to bottom: [gate banner?] / composer / status. The
        // composer grows with its content (1 row, up to a cap). The rightmost
        // column of the transcript is the scrollbar.
        let input_height = (self.composer.line_count(full.width) as u16).clamp(1, 6);
        let gate_height: u16 = if self.pending_gate.is_some() { 1 } else { 0 };
        self.viewport.input_height = input_height;
        self.viewport.gate_height = gate_height;
        let bottom = 1 + input_height + gate_height;

        let main_area = Rect {
            x: full.x,
            y: full.y,
            width: full.width.saturating_sub(1),
            height: full.height.saturating_sub(bottom),
        };
        let gate_area = Rect {
            x: full.x,
            y: full.height.saturating_sub(bottom),
            width: full.width,
            height: gate_height,
        };
        let input_area = Rect {
            x: full.x,
            y: full.height.saturating_sub(1 + input_height),
            width: full.width,
            height: input_height,
        };
        let status_area = Rect {
            x: full.x,
            y: full.height.saturating_sub(1),
            width: full.width,
            height: 1,
        };

        if matches!(self.ui_mode, UiMode::Help) {
            // Help covers the entire area (including the composer/status rows).
            self.render_help_screen(frame.buffer_mut(), full);
            return;
        }

        // Clear the transcript area with the theme background.
        for y in 0..main_area.height {
            for x in 0..main_area.width {
                if let Some(cell) = frame.buffer_mut().cell_mut((x, main_area.y + y)) {
                    cell.set_style(self.theme.document_bg_style);
                }
            }
        }

        self.viewport.last_viewport_height = main_area.height;
        self.layout.pos = Position::default();
        self.layout.indent = 0;
        self.layout.node_path = NodePath::new();
        self.layout.area = main_area;

        self.render_document(main_area, frame.buffer_mut());

        // Approval-gate banner, just above the composer (M3).
        if gate_height > 0 {
            self.render_gate_banner(frame.buffer_mut(), gate_area);
        }

        // Composer: clear its rows, then draw. Place the terminal cursor in it when
        // composing so the user sees where they're typing.
        for y in 0..input_area.height {
            for x in 0..input_area.width {
                if let Some(cell) = frame.buffer_mut().cell_mut((input_area.x + x, input_area.y + y)) {
                    cell.reset();
                    cell.set_style(self.theme.document_bg_style);
                }
            }
        }
        let prompt_style = if self.composing {
            self.theme.status_style
        } else {
            self.theme.muted_style
        };
        let cursor = self.composer.render(
            frame.buffer_mut(),
            input_area,
            self.theme.document_bg_style,
            prompt_style,
        );
        if self.composing {
            frame.set_cursor_position(cursor);
        }

        self.render_status_bar(frame.buffer_mut(), status_area);

        if let Some(layout_cache) = self.viewport.cached_layout {
            self.render_scrollbar(frame.buffer_mut(), main_area, layout_cache.document_height);
        }

        if let UiMode::ThemePicker { selected_index, .. } = self.ui_mode {
            self.render_theme_picker(frame.buffer_mut(), full, selected_index);
        }
    }

    /// Render the pending approval gate as a single highlighted banner row.
    fn render_gate_banner(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let Some(gate) = &self.pending_gate else {
            return;
        };
        // Prefer a bash `command`; otherwise a compact JSON of the input.
        let detail = gate
            .input
            .get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| gate.input.to_string());
        let text = format!(
            "⛔ approve {}: {}   ⏎ approve · type to redirect · .abort",
            gate.name, detail
        );
        let style = ratatui::style::Style::default()
            .bg(self.theme.status_loading_bg)
            .fg(self.theme.status_loading_fg)
            .add_modifier(ratatui::style::Modifier::BOLD);
        for x in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + x, area.y)) {
                cell.reset();
                cell.set_style(style);
            }
        }
        for (col, ch) in (1..).zip(text.chars()) {
            if col >= area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((area.x + col, area.y)) {
                cell.set_char(ch).set_style(style);
            }
        }
    }
}
