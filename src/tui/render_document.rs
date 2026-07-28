use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
};

use super::state::{DocumentLayoutCache, InteractiveState, NodeLayout};
use crate::tui::document::NodePath;

// Baseline left margin for all content - provides breathing room and space for outdented borders
pub(super) const BASELINE_LEFT_MARGIN: u16 = 3;

impl<'a> InteractiveState<'a> {
    /// Whether `row` (a document row) lands inside the viewport — the single test
    /// gating every write and every syntax-highlight call in the document render.
    /// Always false during the [measure](Self::measure_document) pass, so that pass
    /// costs layout arithmetic and nothing else.
    pub(super) fn row_visible(&self, row: u16) -> bool {
        !self.layout.measuring
            && row >= self.viewport.scroll_offset
            && row < self.viewport.scroll_offset + self.layout.area.height
    }

    /// Lay out any nodes whose height isn't cached, without drawing, and record the
    /// document's total height (docs/tui.md). Run before the paint so the scroll
    /// offset can be settled against a known height *and then* painted once —
    /// rather than painting, discovering the document grew, and painting again.
    ///
    /// `area` must be the transcript rect the paint will use.
    pub(super) fn measure_document(&mut self, area: Rect) {
        self.layout.area = area;
        self.layout.measuring = true;
        // Nothing is written during a measure, so the sink can be empty.
        let mut sink = Buffer::empty(Rect::default());
        self.walk_document(&mut sink);
        self.layout.measuring = false;
    }

    /// Render document nodes to buffer, updating action map
    pub(super) fn render_document(&mut self, _area: Rect, buf: &mut Buffer) {
        self.walk_document(buf);
    }

    /// The one document walk, shared by the measure and paint passes.
    ///
    /// A top-level node is laid out in full when it is **visible** or when its
    /// layout isn't cached; otherwise its cached height advances the cursor and its
    /// cached actions are replayed. So the per-frame cost is the visible region plus
    /// whatever changed, not the whole log — the property that keeps a streaming
    /// frame flat as the transcript grows (docs/tui.md).
    fn walk_document(&mut self, buf: &mut Buffer) {
        self.render_cache.actions.clear();
        // The block menu is repainted with the focused unit each frame; its click
        // targets are rebuilt alongside the action map (docs/tui.md).
        self.block_menu_hits.clear();

        // Wrapping is width-dependent, so a width change invalidates every height.
        if self.document.layout_width != self.layout.area.width {
            self.document.layout_width = self.layout.area.width;
            self.document.invalidate_all();
        }
        self.document
            .layouts
            .resize_with(self.document.document.nodes.len(), || None);

        self.layout.pos = Position {
            x: BASELINE_LEFT_MARGIN,
            y: 0,
        };
        self.layout.indent = BASELINE_LEFT_MARGIN;
        self.document.laid_out = 0;

        // Use raw pointer to avoid borrow checker issues when calling render_node
        let nodes_ptr = self.document.document.nodes.as_ptr();
        let node_count = self.document.document.nodes.len();

        for idx in 0..node_count {
            // Add blank line between consecutive top-level blocks
            if idx > 0 {
                self.layout.pos.y += 1;
            }
            let start = self.layout.pos.y;

            // Reuse the cached layout unless the node is on screen (where it must
            // actually be drawn). Off-screen rows produce no output either way, so
            // the only thing the walk owes them is their height and their actions.
            if let Some(cached) = self.document.layouts[idx].as_ref() {
                let end = start.saturating_add(cached.height);
                let visible = end > self.viewport.scroll_offset
                    && start < self.viewport.scroll_offset + self.layout.area.height;
                if self.layout.measuring || !visible {
                    for (rect, action) in &cached.actions {
                        let mut rect = *rect;
                        rect.y = start.saturating_add(rect.y);
                        self.render_cache.actions.push((rect, action.clone()));
                    }
                    self.layout.pos.y = end;
                    continue;
                }
            }

            // Update path for this top-level node
            self.layout.node_path = NodePath::new();
            self.layout.node_path.push(idx);

            let action_base = self.render_cache.actions.len();
            self.document.laid_out += 1;

            // SAFETY: idx is bounded by node_count, and nodes_ptr is valid for the duration of this method
            let node = unsafe { &*nodes_ptr.add(idx) };
            self.render_node(node, buf);

            // Record what this node cost, so a later frame that scrolls it out of
            // view can skip it. Action rects are stored relative to the node's first
            // row, so replaying them only needs the node's new start.
            let actions = self.render_cache.actions[action_base..]
                .iter()
                .map(|(rect, action)| {
                    let mut rect = *rect;
                    rect.y = rect.y.saturating_sub(start);
                    (rect, action.clone())
                })
                .collect();
            self.document.layouts[idx] = Some(NodeLayout {
                height: self.layout.pos.y.saturating_sub(start),
                actions,
            });
        }

        self.viewport.cached_layout = Some(DocumentLayoutCache {
            render_width: self.layout.area.width,
            document_height: self.layout.pos.y,
        });
    }
}
