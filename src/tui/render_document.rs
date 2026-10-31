use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
};

use super::state::{DocumentLayoutCache, InteractiveState, NodeLayout};
use crate::tui::document::{DocumentNode, NodePath};

/// Whether a top-level node opens a new visual group (and so gets a blank line
/// above it). Everything but a continuation entry does; see
/// [`EntryKind::starts_group`](crate::tui::document::EntryKind::starts_group).
fn starts_group(node: &DocumentNode<'_>) -> bool {
    match node {
        DocumentNode::Entry { kind, .. } => kind.starts_group(),
        _ => true,
    }
}

// Baseline left margin for all content - provides breathing room and space for outdented borders
pub(super) const BASELINE_LEFT_MARGIN: u16 = 3;

impl<'a> InteractiveState<'a> {
    /// Whether `row` (a document row) lands inside the viewport — the single test
    /// gating every write and every syntax-highlight call in the document render.
    /// Always false during the [measure](Self::measure_document) pass, so that pass
    /// costs layout arithmetic and nothing else.
    pub(super) fn row_visible(&self, row: u16) -> bool {
        let (top, bottom) = self.drawable_rows(self.layout.area.height);
        !self.layout.measuring && row >= top && row < bottom
    }

    /// The document rows a write may land in: the viewport, narrowed by any clip a
    /// folded item set (docs/tui.md, "Detail by recency"). `height` is the
    /// viewport's — callers pass the area they were handed.
    pub(super) fn drawable_rows(&self, height: u16) -> (u16, u16) {
        let mut top = self.viewport.scroll_offset;
        let mut bottom = top.saturating_add(height);
        if let Some((clip_top, clip_bottom)) = self.layout.clip {
            top = top.max(clip_top);
            bottom = bottom.min(clip_bottom);
        }
        (top, bottom)
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
        self.render_cache.items.clear();
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
        self.document.highlighted = 0;

        // The nodes are held apart from `self` for the walk: `render_node` borrows one
        // while it mutates the rest of `self`. Nothing under `render_node` reads
        // `document.nodes`, so the emptied field is not observable until they go back.
        let nodes = std::mem::take(&mut self.document.document.nodes);
        let node_count = nodes.len();

        for (idx, node) in nodes.iter().enumerate() {
            // A blank line between consecutive top-level nodes — except where the
            // node is a *continuation* of the one above it. A tool call, what the
            // spotter made of it, and what came back are three entries that read as
            // one unit, so nothing separates them (docs/tui.md).
            if idx > 0 && starts_group(node) {
                self.layout.pos.y += 1;
            }
            let start = self.layout.pos.y;
            // Detail by recency (docs/tui.md): each top-level node is an item, and
            // its distance from the tail sets how much of it renders. A folded node
            // that has drifted to a new level is laid out again.
            let level = super::detail::level_for(node_count - 1 - idx, false);
            self.layout.level = level;

            // Reuse the cached layout unless the node is on screen (where it must
            // actually be drawn). Off-screen rows produce no output either way, so
            // the only thing the walk owes them is their height and their actions.
            if let Some(cached) = self.document.layouts[idx]
                .as_ref()
                .filter(|cached| cached.level == level)
            {
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
                    self.note_item(node, start, end);
                    continue;
                }
            }

            // Update path for this top-level node
            self.layout.node_path = NodePath::new();
            self.layout.node_path.push(idx);

            let action_base = self.render_cache.actions.len();
            self.document.laid_out += 1;

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
                level,
            });
            self.note_item(node, start, self.layout.pos.y);
        }
        self.document.document.nodes = nodes;

        self.viewport.cached_layout = Some(DocumentLayoutCache {
            render_width: self.layout.area.width,
            document_height: self.layout.pos.y,
        });
        // Survives the cache's invalidation, so a scroll arriving between a
        // re-projection and the next paint still has a height to reason against.
        self.viewport.last_document_height = self.layout.pos.y;
    }

    /// Record a foldable item's rows for hover and click (docs/tui.md).
    fn note_item(&mut self, node: &DocumentNode<'_>, start: u16, end: u16) {
        if matches!(
            node,
            DocumentNode::CallItem { .. } | DocumentNode::ThinkingItem { .. }
        ) {
            self.render_cache.items.push((start, end));
        }
    }

    /// Tint the item under the pointer, painting a surface one step off the
    /// background across its rows — the terminal's version of the web client's
    /// hovered call card. Only background-coloured cells change, so a code block's
    /// own surface keeps its edge. `buf` is the document's own buffer, origin at the
    /// viewport's first row.
    pub(super) fn paint_item_hover(&self, buf: &mut Buffer) {
        let Some(pos) = self.viewport.cursor_pos else {
            return;
        };
        let Some(&(start, end)) = self
            .render_cache
            .items
            .iter()
            .find(|(start, end)| pos.y >= *start && pos.y < *end)
        else {
            return;
        };
        let bg = self.theme.document_bg_style.bg.unwrap_or_default();
        let fg = self.theme.status_style.fg.unwrap_or_default();
        let tint = super::theme::mix(bg, fg, 0.07);
        let scroll = self.viewport.scroll_offset;
        let height = buf.area.height;
        for row in start.max(scroll)..end.min(scroll.saturating_add(height)) {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell_mut((x, row - scroll))
                    && (cell.bg == bg || cell.bg == ratatui::style::Color::Reset)
                {
                    cell.bg = tint;
                }
            }
        }
    }
}
