//! The staged-attachment rows above the control strip (docs/tui.md): one row per chip — what
//! it is, what the daemon made of it, and a `✕` that takes it back — under a band of image
//! thumbnails when the terminal draws images and any staged image has decoded.

use ratatui::{buffer::Buffer, layout::Rect};

use ratatui::widgets::Widget;
use ratatui_image::Image;

use super::image::Painted;
use super::staged::{Staged, Status};
use super::state::InteractiveState;
use crate::attachments::Kind;

/// At most this many chip rows; past it the last row counts the rest.
const MAX_ROWS: usize = 4;

/// The thumbnail band's height: enough to recognize a screenshot, few enough to stay chrome.
const THUMB_ROWS: u16 = 5;

/// The widest a single thumbnail may be, in columns.
const THUMB_MAX_COLS: u16 = 24;

/// The chip's description: the triage signals the model will read, short.
pub(crate) fn detail(staged: &Staged) -> String {
    match &staged.status {
        Status::Uploading => "uploading…".into(),
        Status::Failed(e) => format!("upload failed: {e}"),
        Status::Ready(arrival) => kind_detail(&arrival.kind),
    }
}

/// An attachment's kind as a chip states it. Shared with the transcript's rendering of a
/// sent turn's attachments.
pub(crate) fn kind_detail(kind: &Kind) -> String {
    match kind {
        Kind::Image {
            format,
            width,
            height,
            bytes,
        } => format!("{format} · {width}×{height} · {}", human_bytes(*bytes)),
        Kind::Text { lines, bytes, .. } if *lines > 0 => {
            format!("{lines} lines · {}", human_bytes(*bytes))
        }
        Kind::Text { bytes, .. } | Kind::Binary { bytes } => human_bytes(*bytes),
    }
}

fn human_bytes(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

impl<'a> InteractiveState<'a> {
    /// Rows the staged row takes: a chip row each (capped), plus the thumbnail band when there
    /// is a thumbnail to draw in it.
    pub(crate) fn staged_rows(&self) -> u16 {
        let chips = self.staged.items().len().min(MAX_ROWS) as u16;
        chips + if self.has_thumbnails() { THUMB_ROWS } else { 0 }
    }

    fn has_thumbnails(&self) -> bool {
        self.staged_images.enabled()
            && self
                .staged
                .items()
                .iter()
                .any(|s| self.staged_images.source_size((s.key, 0)).is_some())
    }

    pub(super) fn render_staged(&mut self, buf: &mut Buffer, area: Rect) {
        self.viewport.staged_hits.clear();
        for y in area.y..area.y + area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell_mut((area.x + x, y)) {
                    cell.reset();
                    cell.set_style(self.theme.document_bg_style);
                }
            }
        }
        let mut area = area;
        if self.has_thumbnails() {
            let band = Rect {
                height: THUMB_ROWS.min(area.height),
                ..area
            };
            self.render_thumbnails(buf, band);
            area.y += band.height;
            area.height -= band.height;
        }
        let items = self.staged.items();
        let overflow = items.len().saturating_sub(MAX_ROWS);
        for (row, staged) in items.iter().take(area.height as usize).enumerate() {
            let y = area.y + row as u16;
            let line = Rect {
                y,
                height: 1,
                ..area
            };
            if overflow > 0 && row + 1 == MAX_ROWS {
                put(
                    buf,
                    line,
                    1,
                    &format!("+{} more attached", overflow + 1),
                    self.theme.muted_style,
                );
                continue;
            }
            let mut x = put(buf, line, 1, "⎘ ", self.theme.muted_style);
            x = put(buf, line, x, staged.name(), self.theme.strip_item_style);
            x = put(buf, line, x + 2, &detail(staged), self.theme.muted_style);
            let close = x + 2;
            if close < area.width {
                put(buf, line, close, "✕", self.theme.strip_item_style);
                self.viewport
                    .staged_hits
                    .push((y, area.x + close, area.x + close + 1, staged.key));
            }
        }
    }

    /// Staged images' thumbnails, left to right in staging order, a column apart, bottom-aligned
    /// in the band so each sits on the chip rows below it. Stationary chrome, so always the
    /// native render when there is one.
    fn render_thumbnails(&mut self, buf: &mut Buffer, band: Rect) {
        let keys: Vec<u64> = self.staged.items().iter().map(|s| s.key).collect();
        let mut x = band.x + 1;
        for key in keys {
            let Some((w, h)) = self.staged_images.source_size((key, 0)) else {
                continue;
            };
            let room = band.width.saturating_sub(x - band.x);
            let Some(cells) =
                self.staged_images
                    .footprint_within(w, h, THUMB_MAX_COLS.min(room), band.height)
            else {
                return;
            };
            if cells.width == 0 || cells.width > room {
                break;
            }
            self.staged_images.ensure_encoded((key, 0), cells);
            let rect = Rect::new(
                x,
                band.y + band.height - cells.height,
                cells.width,
                cells.height,
            );
            match self.staged_images.painted((key, 0)) {
                Painted::Ready {
                    native: Some(protocol),
                    ..
                } => Image::new(protocol).render(rect, buf),
                Painted::Ready { scratch, .. } => {
                    for sy in 0..scratch.area.height.min(rect.height) {
                        for sx in 0..scratch.area.width.min(rect.width) {
                            if let (Some(src), Some(dst)) = (
                                scratch.cell((sx, sy)),
                                buf.cell_mut((rect.x + sx, rect.y + sy)),
                            ) {
                                *dst = src.clone();
                            }
                        }
                    }
                }
                Painted::Pending | Painted::Failed(_) => {}
            }
            x += cells.width + 2;
        }
    }
}

/// Write `text` from column `col` of a one-row area, clipped at its edge; returns the column
/// after the last character written.
fn put(buf: &mut Buffer, area: Rect, col: u16, text: &str, style: ratatui::style::Style) -> u16 {
    let mut x = col;
    for ch in text.chars() {
        if x >= area.width {
            break;
        }
        if let Some(cell) = buf.cell_mut((area.x + x, area.y)) {
            cell.set_char(super::write_text::fold_control(ch))
                .set_style(style);
        }
        x += 1;
    }
    x
}
