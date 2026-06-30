use ratatui::{
    Frame,
    layout::{Position, Rect},
};

use super::{InteractiveState, UiMode};
use crate::tui::document::NodePath;

/// Max rows the live process panel occupies — beyond this, processes are still
/// running and tracked, just not all shown (Slice 2's `list()` surfaces all).
const PROC_PANEL_MAX: u16 = 6;

/// Minimum total terminal width at which the sidebar docks; below it the sidebar is
/// hidden (the narrow-mode drawer is a later step), and the process strip returns.
pub(super) const SIDEBAR_MIN_TOTAL: u16 = 90;
/// Docked sidebar column width, including its 1-col left divider (docs/tui.md).
pub(super) const SIDEBAR_WIDTH: u16 = 30;

/// Compact seconds → `8s` / `4m12s` / `1h3m`, for the process-panel timing.
pub(super) fn fmt_secs(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

/// Write `text` starting at column `start` of row `y`, in `style`, truncating at the
/// area's right edge. Returns the next free column. Column-at-a-time, matching the
/// gate/status-bar render (wide glyphs occupy one cell, as elsewhere in this TUI).
fn put_cols(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    y: u16,
    start: u16,
    text: &str,
    style: ratatui::style::Style,
) -> u16 {
    let mut col = start;
    for ch in text.chars() {
        if col >= area.width {
            break;
        }
        if let Some(cell) = buf.cell_mut((area.x + col, y)) {
            cell.set_char(ch).set_style(style);
        }
        col += 1;
    }
    col
}

impl<'a> InteractiveState<'a> {
    pub(super) fn render_frame(&mut self, frame: &mut Frame) {
        let full = frame.area();

        // Bottom chrome, top to bottom: [process panel?] / composer / status. The
        // composer grows with its content (1 row, up to a cap). The rightmost column
        // of the transcript is the scrollbar. The process panel (docs/supervisor.md) is one row
        // per running process, capped, hidden when none run — an ambient strip between
        // the transcript and the composer. The approval gate is *not* bottom chrome:
        // it renders inline in the transcript at the pending call (docs/tui.md).
        // Right-docked sidebar when the terminal is wide enough (docs/tui.md). It takes
        // the rightmost columns for the full height above the status bar; the transcript
        // and composer share the remaining left column. Processes move *into* the sidebar,
        // so the bottom process strip only appears in the narrow (no-sidebar) fallback
        // until the drawer lands.
        let sidebar_width: u16 = if full.width >= SIDEBAR_MIN_TOTAL {
            SIDEBAR_WIDTH
        } else {
            0
        };
        let content_width = full.width.saturating_sub(sidebar_width);

        let input_height = (self.composer.line_count(content_width) as u16).clamp(1, 6);
        let proc_height: u16 = if sidebar_width > 0 {
            0
        } else {
            (self.processes.len() as u16).min(PROC_PANEL_MAX)
        };
        self.viewport.input_height = input_height;
        self.viewport.gate_height = 0;
        let bottom = 1 + input_height + proc_height;

        let main_area = Rect {
            x: full.x,
            y: full.y,
            width: content_width.saturating_sub(1),
            height: full.height.saturating_sub(bottom),
        };
        let proc_area = Rect {
            x: full.x,
            y: full.height.saturating_sub(bottom),
            width: content_width,
            height: proc_height,
        };
        let input_area = Rect {
            x: full.x,
            y: full.height.saturating_sub(1 + input_height),
            width: content_width,
            height: input_height,
        };
        let status_area = Rect {
            x: full.x,
            y: full.height.saturating_sub(1),
            width: full.width,
            height: 1,
        };
        let sidebar_area = (sidebar_width > 0).then(|| Rect {
            x: full.x + content_width,
            y: full.y,
            width: sidebar_width,
            height: full.height.saturating_sub(1),
        });
        // Remembered for the sidebar-only dev screenshot slice (docs/tui.md).
        self.viewport.sidebar_area = sidebar_area;

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

        // Live background-process panel, between the transcript and the gate (docs/supervisor.md).
        if proc_height > 0 {
            self.render_processes_panel(frame.buffer_mut(), proc_area);
        }

        // Composer: clear its rows, then draw. Place the terminal cursor in it when
        // composing so the user sees where they're typing.
        for y in 0..input_area.height {
            for x in 0..input_area.width {
                if let Some(cell) = frame
                    .buffer_mut()
                    .cell_mut((input_area.x + x, input_area.y + y))
                {
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

        if let Some(sb) = sidebar_area {
            self.render_sidebar(frame.buffer_mut(), sb);
        }

        if let UiMode::ThemePicker { selected_index, .. } = self.ui_mode {
            self.render_theme_picker(frame.buffer_mut(), full, selected_index);
        }
    }

    /// Render the current state to an off-screen [`TestBackend`] and encode it as
    /// the same framed character grid the snapshot harness commits (docs/tui.md) —
    /// the live "dev screenshot" capture (`Ctrl+S`). It draws a *fresh* frame
    /// rather than reading the real backend's buffer, so the capture is independent
    /// of draw timing; the next real draw overwrites the layout fields it touches.
    /// Reuses [`crate::tui::grid::frame_grid`], so a live capture is byte-comparable
    /// to a committed `.snap`.
    pub(super) fn capture_screenshot_grid(&mut self, width: u16, height: u16) -> String {
        use ratatui::{Terminal, backend::TestBackend};

        let mut term = Terminal::new(TestBackend::new(width, height))
            .expect("TestBackend construction is infallible");
        self.flush_reproject();
        let _ = term.draw(|frame| self.render_frame(frame));
        let cursor = term.get_cursor_position().ok().map(|p| (p.x, p.y));
        crate::tui::grid::frame_grid(term.backend().buffer(), width, cursor)
    }

    /// Render the live background-process panel (docs/supervisor.md): one row per running
    /// process — `▶ bg#3  cargo watch · 4m12s · idle 8s │ <last output line>` — the
    /// handle/header in the accent style, the last tail line muted. Fed by the
    /// `/state` poll; capped at [`PROC_PANEL_MAX`] rows (the rest are still in the
    /// list, just not shown — the directed `list()` read of Slice 2 will surface all).
    fn render_processes_panel(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        for (row, p) in (0..area.height).zip(&self.processes) {
            let y = area.y + row;
            for x in 0..area.width {
                if let Some(cell) = buf.cell_mut((area.x + x, y)) {
                    cell.reset();
                    cell.set_style(self.theme.document_bg_style);
                }
            }
            let header = format!(
                "▶ {}  {} · {} · idle {} │ ",
                p.handle,
                p.command,
                fmt_secs(p.running_secs),
                fmt_secs(p.idle_secs)
            );
            let last_line = p.tail.lines().next_back().unwrap_or("");
            let col = put_cols(
                buf,
                area,
                y,
                1,
                &header,
                self.theme.breadcrumb_current_style,
            );
            put_cols(buf, area, y, col, last_line, self.theme.muted_style);
        }
    }
}
