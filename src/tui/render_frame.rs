use ratatui::{
    Frame,
    layout::{Position, Rect},
};

use super::{InteractiveState, UiMode};
use crate::tui::document::NodePath;

/// Max rows the live process panel occupies — beyond this, processes are still
/// running and tracked, just not all shown (Slice 2's `list()` surfaces all).
const PROC_PANEL_MAX: u16 = 6;

/// The session drawer's width, including its 1-col left divider (docs/tui.md).
pub(super) const DRAWER_WIDTH: u16 = 56;
/// The widest the transcript column grows, margins included (docs/tui.md): on a
/// wider terminal it is centred, as the web client centres its column, so a line
/// of prose stays a readable length.
pub(super) const TRANSCRIPT_MEASURE: u16 = 112;

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
/// Content text lands here too — a process's command line is whatever the model
/// wrote — so control characters are folded, not written.
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
            cell.set_char(crate::tui::write_text::fold_control(ch))
                .set_style(style);
        }
        col += 1;
    }
    col
}

impl<'a> InteractiveState<'a> {
    pub(super) fn render_frame(&mut self, frame: &mut Frame) {
        let full = frame.area();

        // Top to bottom: the header row; the transcript; then the bottom chrome —
        // [process panel?] / staged / control strip / composer / status. The composer
        // grows with its content (1 row, up to a cap). The rightmost column of the
        // transcript is the scrollbar. The process panel (docs/process-monitor.md) is one
        // row per running process, capped, hidden when none run. The approval gate is
        // *not* chrome: it renders inline in the transcript at the pending call
        // (docs/tui.md). The session drawer (what the docked sidebar used to carry)
        // is an overlay on the transcript's right, open on demand.
        let header_height: u16 = 1;
        // The transcript, strip, and composer share one column, centred once the
        // terminal is wider than the measure; the header and status rows, the
        // scrollbar, and the drawer stay on the screen's edges.
        let measure = full.width.min(TRANSCRIPT_MEASURE);
        let column_x = full.x + (full.width - measure) / 2;
        let content_width = if column_x > full.x {
            measure
        } else {
            full.width
        };

        // A staged verb's chip (docs/tui.md) sits left of the composer; the
        // composer wraps within the remaining columns, so height must agree.
        let chip_w: u16 = self
            .staging
            .as_ref()
            .map(|s| s.verb.label().chars().count() as u16 + 1)
            .unwrap_or(0);
        // A read-only view renders no composer at all (docs/tui.md): the row would
        // be an input that cannot send. The transcript takes the rows instead.
        let input_height = if self.read_only() {
            0
        } else {
            (self
                .composer
                .line_count(content_width.saturating_sub(chip_w)) as u16)
                .clamp(1, 6)
        };
        let proc_height: u16 = (self.processes.len() as u16).min(PROC_PANEL_MAX);
        self.viewport.input_height = input_height;
        // The control strip (docs/tui.md) holds a constant one-row slot directly
        // above the composer; staged attachments take a row each above it, capped.
        let strip_height: u16 = 1;
        let staged_height = if self.read_only() {
            0
        } else {
            self.staged_rows()
        };
        self.viewport.above_input_height = strip_height + staged_height;
        let bottom = 1 + input_height + strip_height + staged_height + proc_height;

        let main_area = Rect {
            x: column_x,
            y: full.y + header_height,
            // Clear of the scrollbar, which keeps the screen's last column.
            width: content_width.min(full.width.saturating_sub(1 + column_x - full.x)),
            height: full.height.saturating_sub(bottom + header_height),
        };
        let proc_area = Rect {
            x: column_x,
            y: full.height.saturating_sub(bottom),
            width: content_width,
            height: proc_height,
        };
        let staged_area = Rect {
            x: column_x,
            y: full
                .height
                .saturating_sub(1 + input_height + strip_height + staged_height),
            width: content_width,
            height: staged_height,
        };
        let strip_area = Rect {
            x: column_x,
            y: full.height.saturating_sub(1 + input_height + strip_height),
            width: content_width,
            height: strip_height,
        };
        let input_area = Rect {
            x: column_x,
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
        // The drawer overlays the transcript's right side and only the transcript:
        // the bottom chrome stays whole, so a gate's strip verbs are never under it.
        // A read-only view has no live state to fill it.
        let drawer_width = DRAWER_WIDTH.min(full.width);
        let sidebar_area = (self.sidebar_open && !self.read_only()).then(|| Rect {
            x: full.x + full.width - drawer_width,
            y: main_area.y,
            width: drawer_width,
            height: main_area.height,
        });
        // Remembered for drawer click dispatch and the drawer-only dev screenshot.
        self.viewport.sidebar_area = sidebar_area;
        self.viewport.doc_origin = Position::new(main_area.x, main_area.y);

        if matches!(self.ui_mode, UiMode::Help) {
            // Help covers the entire area (including the composer/status rows).
            self.render_help_screen(frame.buffer_mut(), full);
            return;
        }

        // The margins either side of a centred column are the document's surface
        // too, so the column reads as placed on the page rather than cut out of it.
        for y in (full.y + header_height)..full.height.saturating_sub(1) {
            for x in full.x..full.x + full.width {
                if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                    cell.reset();
                    cell.set_style(self.theme.document_bg_style);
                }
            }
        }

        // The document renders at the origin of a buffer of its own — every layout
        // and paint path works in document coordinates anchored at `(0, 0)` — and is
        // copied into the frame at `main_area`'s offset. Cells are copied whole,
        // including an image protocol's escape cell and the skip flags beside it, so
        // native images land where they were laid out (docs/tui.md).
        let doc_area = Rect::new(0, 0, main_area.width, main_area.height);
        let mut doc_buf = ratatui::buffer::Buffer::empty(doc_area);
        for cell in doc_buf.content.iter_mut() {
            cell.set_style(self.theme.document_bg_style);
        }

        self.viewport.last_viewport_height = main_area.height;
        self.layout.pos = Position::default();
        self.layout.indent = 0;
        self.layout.node_path = NodePath::new();
        self.layout.area = doc_area;

        // Lay out (without drawing) first, so the document height is known before
        // anything is painted: a stream-following viewport can then be pinned to the
        // new bottom and painted *once*, instead of painting at the stale offset and
        // repainting at the corrected one (docs/tui.md).
        self.measure_document(doc_area);
        if self.viewport.stuck_to_bottom {
            self.scroll_to_bottom();
        }

        // The offset is settled now, so this frame knows whether the viewport is
        // moving. Image rendering reads the flag to hold the halfblocks slice
        // under retransmit-per-move protocols (docs/tui.md); the extra `dirty`
        // schedules one settled repaint right after motion stops, which is the
        // frame that restores the crisp native render.
        self.layout.scroll_in_motion =
            self.viewport.scroll_offset != self.viewport.last_painted_scroll;
        if self.layout.scroll_in_motion {
            self.dirty = true;
        }
        self.viewport.last_painted_scroll = self.viewport.scroll_offset;

        self.render_document(doc_area, &mut doc_buf);
        self.paint_item_hover(&mut doc_buf);
        let buf = frame.buffer_mut();
        for y in 0..doc_area.height {
            for x in 0..doc_area.width {
                if let (Some(src), Some(dst)) = (
                    doc_buf.cell((x, y)),
                    buf.cell_mut((main_area.x + x, main_area.y + y)),
                ) {
                    *dst = src.clone();
                }
            }
        }

        self.render_header(
            frame.buffer_mut(),
            Rect::new(full.x, full.y, full.width, header_height),
        );

        // Live background-process panel, between the transcript and the gate (docs/process-monitor.md).
        if proc_height > 0 {
            self.render_processes_panel(frame.buffer_mut(), proc_area);
        }

        // Staged attachments, then the control strip directly above the composer.
        self.render_staged(frame.buffer_mut(), staged_area);
        self.render_control_strip(frame.buffer_mut(), strip_area);

        // Composer — skipped entirely in a read-only view, which reserved it no rows
        // above (`input_height` is 0 there): an input that cannot send is not chrome
        // worth drawing.
        if !self.read_only() {
            self.render_composer(frame, input_area, chip_w);
        }

        self.render_status_bar(frame.buffer_mut(), status_area);

        if let Some(layout_cache) = self.viewport.cached_layout {
            // The scrollbar draws in the column just past the rect it is handed:
            // the screen's last one, whatever the transcript column's placement.
            let track = Rect {
                x: full.x,
                width: full.width.saturating_sub(1),
                ..main_area
            };
            self.render_scrollbar(frame.buffer_mut(), track, layout_cache.document_height);
        }

        if let Some(sb) = sidebar_area {
            self.render_sidebar(frame.buffer_mut(), sb);
        }

        if let UiMode::ThemePicker { selected_index, .. } = self.ui_mode {
            self.render_theme_picker(frame.buffer_mut(), full, selected_index);
        }
        // Cloned rather than borrowed: the paint takes `&mut self` for the
        // render caches, and the menu is a handful of rows.
        if let UiMode::Sessions(menu) = &self.ui_mode {
            let menu = menu.clone();
            self.render_sessions(frame.buffer_mut(), full, &menu);
        }
    }

    /// Draw the composer into its reserved rows and place the terminal cursor in it
    /// while composing, so the user sees where they are typing. While staging, the
    /// verb's chip renders left of the prompt (`allow › rm█`) — the visible "your
    /// next ⏎ commits an argument, not a message" signal (docs/tui.md); the composer
    /// itself is untouched chrome, drawn in the columns the chip leaves.
    fn render_composer(&mut self, frame: &mut Frame, input_area: Rect, chip_w: u16) {
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
        let composer_area = if let Some(staging) = &self.staging {
            put_cols(
                frame.buffer_mut(),
                input_area,
                input_area.y,
                0,
                staging.verb.label(),
                self.theme.strip_item_focused_style,
            );
            Rect {
                x: input_area.x + chip_w,
                y: input_area.y,
                width: input_area.width.saturating_sub(chip_w),
                height: input_area.height,
            }
        } else {
            input_area
        };
        self.viewport.composer_area = Some(composer_area);
        let cursor = self.composer.render(
            frame.buffer_mut(),
            composer_area,
            self.theme.document_bg_style,
            prompt_style,
            self.theme.composer_selection_style,
        );
        if self.composing {
            frame.set_cursor_position(cursor);
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

    /// Like [`capture_screenshot_grid`](Self::capture_screenshot_grid) but frames only
    /// the session drawer's columns (`Alt+S`) — so a wide-terminal capture of just the
    /// drawer stays narrow instead of the full screen. The drawer is opened for the
    /// capture and left as it was found; a read-only view, which has none, falls back
    /// to the whole grid.
    pub(super) fn capture_sidebar_grid(&mut self, width: u16, height: u16) -> String {
        use ratatui::{Terminal, backend::TestBackend};

        let mut term = Terminal::new(TestBackend::new(width, height))
            .expect("TestBackend construction is infallible");
        self.flush_reproject();
        let was_open = std::mem::replace(&mut self.sidebar_open, true);
        let _ = term.draw(|frame| self.render_frame(frame));
        self.sidebar_open = was_open;
        match self.viewport.sidebar_area {
            Some(rect) => crate::tui::grid::frame_grid_region(term.backend().buffer(), rect),
            None => crate::tui::grid::frame_grid(term.backend().buffer(), width, None),
        }
    }

    /// Render the live background-process panel (docs/process-monitor.md): one row per running
    /// process — `▶ p#3  cargo watch · 4m12s · idle 8s │ <last output line>` — the
    /// handle/header in the accent style, the last tail line muted. Fed by the
    /// `processes` topic; capped at [`PROC_PANEL_MAX`] rows (the rest are still in the
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
                p.handle(),
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
