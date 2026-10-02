//! Rendering a [`DocumentNode::Image`] (docs/tui.md).
//!
//! The node reserves a fixed rectangle — a pure function of its pixel metadata
//! and the render width, via [`ImageStore::footprint`] — and what paints *into*
//! that rectangle depends on where the bytes have gotten to and how much of the
//! rectangle is on screen. Fully visible → the native protocol's escape (the
//! crisp render). Partially scrolled, top or bottom → a row-slice of the
//! halfblocks scratch buffer, which pixel protocols can't do (they paint from
//! the image's top edge only). Pending or failed → a one-line gloss at the top
//! visible row. The reservation itself never changes, so scrolling and byte
//! arrival never reflow the transcript around an image.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui_image::Image;

use super::image::Painted;
use super::state::InteractiveState;

impl<'a> InteractiveState<'a> {
    /// Render one image node at the layout cursor, advancing it by the reserved
    /// height. With images off (no picker), the reference renders as a single
    /// dim line rather than a blank reservation.
    pub(super) fn render_image_node(
        &mut self,
        seq: u64,
        idx: usize,
        media_type: &str,
        px_width: u32,
        px_height: u32,
        buf: &mut Buffer,
    ) {
        let gloss = format!("⧉ {px_width}×{px_height} {media_type}");
        let style = self.theme.muted_style;
        let x = self.layout.indent;
        // One column of right margin, like the code-block boxes keep.
        let max_cols = self.layout.area.width.saturating_sub(x + 1);
        let Some(cells) = self.images.footprint(px_width, px_height, max_cols) else {
            self.write_text(buf, self.layout.pos.y, x, &gloss, self.layout.area, style);
            self.layout.pos.y += 1;
            return;
        };

        let top = self.layout.pos.y;
        let bottom = top.saturating_add(cells.height);
        self.layout.pos.y = bottom;
        if self.layout.measuring {
            return;
        }
        let scroll = self.viewport.scroll_offset;
        // The viewport, narrowed by any clip a folded item set (docs/tui.md).
        let (view_top, view_bottom) = self.drawable_rows(self.layout.area.height);
        let (vis_top, vis_bottom) = (top.max(view_top), bottom.min(view_bottom));
        if vis_top >= vis_bottom {
            return;
        }

        let key = (seq, idx);
        self.images.ensure_encoded(key, cells);
        match self.images.painted(key) {
            Painted::Pending => {
                let text = format!("{gloss} · fetching…");
                self.write_text(buf, vis_top, x, &text, self.layout.area, style);
            }
            Painted::Failed(why) => {
                let text = format!("{gloss} · unavailable: {why}");
                self.write_text(buf, vis_top, x, &text, self.layout.area, style);
            }
            Painted::Ready { native, scratch } => {
                let fully_visible = top >= view_top && bottom <= view_bottom;
                // While the viewport is moving, a protocol that retransmits on
                // every reposition flashes; hold the halfblocks slice for the
                // motion and let the settled frame restore the crisp render.
                let steady =
                    !(self.layout.scroll_in_motion && self.images.native_flickers_on_move());
                if fully_visible
                    && steady
                    && let Some(protocol) = native
                {
                    // The widget's own cell-skip keeps ratatui from overwriting
                    // the escape's area on redraw.
                    let size = protocol.size();
                    let rect = Rect::new(x, top - scroll, size.width, size.height);
                    Image::new(protocol).render(rect, buf);
                } else {
                    // The halfblocks slice: ordinary cells, blitted row-wise, so
                    // any sub-range of the image's rows can show (docs/tui.md).
                    blit_rows(scratch, x, top, vis_top..vis_bottom, scroll, buf);
                }
            }
        }
    }
}

/// Copy the scratch buffer's rows `rows - top` to the frame at `rows - scroll`,
/// clipped to both buffers. The scratch may be a hair smaller than the reserved
/// footprint (protocol resize keeps aspect on the font grid); rows past it stay
/// background.
fn blit_rows(
    scratch: &Buffer,
    x: u16,
    top: u16,
    rows: std::ops::Range<u16>,
    scroll: u16,
    buf: &mut Buffer,
) {
    for row in rows {
        let src_y = row - top;
        if src_y >= scratch.area.height {
            break;
        }
        let dst_y = row - scroll;
        for src_x in 0..scratch.area.width {
            let Some(src) = scratch.cell((src_x, src_y)) else {
                continue;
            };
            if let Some(dst) = buf.cell_mut((x.saturating_add(src_x), dst_y)) {
                *dst = src.clone();
            }
        }
    }
}
