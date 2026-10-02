//! Rendering a transcript item at its detail level (docs/tui.md, "Detail by
//! recency"; the rules are `tui/detail.rs`). A call item renders whole, trimmed —
//! its result capped under a dimmed last row, with a row that opens it — or brief:
//! its intent, its signature on one row, and the triage note. Any folded item opens
//! whole on a click or `⏎`.
//!
//! A trimmed body is laid out in full and drawn through a clip, so the cap is a
//! render decision over the same nodes a whole call renders: nothing about the
//! call's content is re-derived for the folded form.

use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

use super::detail::{Level, THINKING_BOX_ROWS, THINKING_FLOOR, THINKING_PREVIEW_ROWS, TRIM_ROWS};
use super::state::InteractiveState;
use crate::tui::document::{DocumentNode, EntryKind, Span, SpanStyle, TuiAction};

/// Whether a node opens a new visual group inside an item — the same rule the
/// document walk applies at top level: a result or an annotation continues the call
/// above it.
fn starts_group(node: &DocumentNode<'_>) -> bool {
    match node {
        DocumentNode::Entry { kind, .. } => kind.starts_group(),
        _ => true,
    }
}

fn span<'a>(text: impl Into<String>, style: SpanStyle) -> Span<'a> {
    Span {
        text: text.into().into(),
        style,
        action: None,
    }
}

/// Cut `text` to `width` columns, ending in `…` when it did not fit.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

impl<'a> InteractiveState<'a> {
    /// A [`DocumentNode::CallItem`] at its level: whole when it must show or was
    /// opened, else as the document walk's distance from the tail says.
    pub(super) fn render_call_item(&mut self, item: &DocumentNode<'a>, buf: &mut Buffer) {
        let DocumentNode::CallItem {
            key,
            must_show,
            intent,
            signature,
            note,
            nodes,
        } = item
        else {
            return;
        };
        let level = if *must_show || self.expanded.contains(key.as_ref()) {
            Level::Full
        } else {
            self.layout.level
        };
        match level {
            Level::Full => self.render_item_children(nodes, None, buf),
            Level::Trimmed => self.render_item_children(nodes, Some(key), buf),
            Level::Brief => {
                self.render_brief_call(key, intent.as_deref(), signature, note.as_deref(), buf)
            }
        }
    }

    /// The item's entries in order, spaced as they are at top level. With `trim`,
    /// the result entry is capped (see [`render_capped`](Self::render_capped)).
    fn render_item_children(
        &mut self,
        nodes: &[DocumentNode<'a>],
        trim: Option<&str>,
        buf: &mut Buffer,
    ) {
        for (idx, child) in nodes.iter().enumerate() {
            if idx > 0 && starts_group(child) {
                self.layout.pos.y += 1;
            }
            let saved_path = self.layout.node_path;
            self.layout.node_path.push(idx);
            let is_result = matches!(
                child,
                DocumentNode::Entry {
                    kind: EntryKind::ToolResult,
                    ..
                }
            );
            match trim.filter(|_| is_result) {
                Some(key) => self.render_capped(child, key, buf),
                None => self.render_node(child, buf),
            }
            self.layout.node_path = saved_path;
        }
    }

    /// A result entry with its body cut at [`TRIM_ROWS`]: laid out in full, drawn
    /// through a clip. When it overflowed, the last row shown is dimmed — the
    /// terminal's stand-in for the web client's fade — and a row under it opens the
    /// item whole.
    fn render_capped(&mut self, node: &DocumentNode<'a>, key: &str, buf: &mut Buffer) {
        let start = self.layout.pos.y;
        let cap = start.saturating_add(1 + TRIM_ROWS); // the header row, then the body
        let action_base = self.render_cache.actions.len();
        let outer = self.layout.clip;
        let (top, bottom) = outer.unwrap_or((0, u16::MAX));
        // One row of slack past the cap: a body that ends there — often in a blank
        // row before an empty block — is shown whole rather than cut for nothing.
        self.layout.clip = Some((top, bottom.min(cap + 1)));
        self.render_node(node, buf);
        self.layout.clip = outer;
        if self.layout.pos.y <= cap + 1 {
            return;
        }
        // Nothing past the cut is on screen, so nothing past it may be clicked.
        let kept: Vec<_> = self
            .render_cache
            .actions
            .drain(action_base..)
            .filter(|(rect, _)| rect.y < cap)
            .collect();
        self.render_cache.actions.extend(kept);
        self.layout.pos.y = cap;

        let last = cap - 1;
        if self.row_visible(last) {
            let screen_row = last - self.viewport.scroll_offset;
            for x in 0..self.layout.area.width {
                if let Some(cell) = buf.cell_mut((x, screen_row)) {
                    cell.modifier.insert(Modifier::DIM);
                }
            }
        }
        // The slack row was drawn; the opening row replaces it.
        if self.row_visible(cap) {
            let screen_row = cap - self.viewport.scroll_offset;
            for x in 0..self.layout.area.width {
                if let Some(cell) = buf.cell_mut((x, screen_row)) {
                    cell.set_char(' ').set_style(self.theme.document_bg_style);
                }
            }
        }
        self.layout.pos.x = self.layout.indent;
        self.render_span(
            &Span {
                text: "⋯ show the whole result".into(),
                style: SpanStyle::Comment,
                action: Some(TuiAction::ExpandItem(key.to_string().into())),
            },
            buf,
        );
        self.layout.pos.y += 1;
    }

    /// A call folded to a glance: its intent, its signature on one row after the
    /// prompt, and the triage note. The whole of it is one target that opens the
    /// item, by click or `⏎` — and a stop on the `↑`/`↓` axis, as an entry is.
    fn render_brief_call(
        &mut self,
        key: &str,
        intent: Option<&str>,
        signature: &str,
        note: Option<&str>,
        buf: &mut Buffer,
    ) {
        let start = self.layout.pos.y;
        let indent = self.layout.indent;
        let width = self.layout.area.width.saturating_sub(indent + 1) as usize;
        // Pushed before the rows render, so it holds the item's first index: the
        // focus order is the visual order.
        let action_index = self.render_cache.actions.len();
        self.render_cache.actions.push((
            Rect::new(0, start, self.layout.area.width, 1),
            TuiAction::ExpandItem(key.to_string().into()),
        ));

        if let Some(intent) = intent {
            self.layout.pos.x = indent;
            self.render_span(&span(truncate(intent, width), SpanStyle::Plain), buf);
            self.layout.pos.y += 1;
        }
        self.layout.pos.x = indent;
        // `flat_signature` leads a shell command with its own prompt.
        let signature = signature.strip_prefix("› ").unwrap_or(signature);
        self.render_span(&span("› ", SpanStyle::UserAccent), buf);
        self.render_span(
            &span(
                truncate(signature, width.saturating_sub(2)),
                SpanStyle::Comment,
            ),
            buf,
        );
        self.layout.pos.y += 1;
        if let Some(note) = note {
            self.layout.pos.x = indent + 2;
            let saved_indent = self.layout.indent;
            self.layout.indent = indent + 2;
            self.render_span(&span(note, SpanStyle::Comment), buf);
            self.layout.indent = saved_indent;
            self.layout.pos.y += 1;
        }
        // The target spans every row of the brief form.
        if let Some((rect, _)) = self.render_cache.actions.get_mut(action_index) {
            rect.height = self.layout.pos.y.saturating_sub(start);
        }
    }

    /// A committed thinking block at its level. Short ones render whole whatever the
    /// distance, since folding them saves nothing. A long one at the whole level is
    /// held to its last [`THINKING_BOX_ROWS`] — the box it streamed in, stopped at
    /// its last line — under a header that opens it; trimmed, it shows its first
    /// [`THINKING_PREVIEW_ROWS`]; brief, it is one `Thought · N words` row.
    pub(super) fn render_thinking_item(
        &mut self,
        key: &str,
        words: usize,
        entry: &DocumentNode<'a>,
        buf: &mut Buffer,
    ) {
        let saved_path = self.layout.node_path;
        self.layout.node_path.push(0);
        match entry {
            DocumentNode::Entry { blocks, .. }
                if words > THINKING_FLOOR && !self.expanded.contains(key) =>
            {
                self.render_folded_thinking(key, words, blocks, buf);
            }
            _ => self.render_node(entry, buf),
        }
        self.layout.node_path = saved_path;
    }

    fn render_folded_thinking(
        &mut self,
        key: &str,
        words: usize,
        blocks: &[DocumentNode<'a>],
        buf: &mut Buffer,
    ) {
        let open = |text: String| Span {
            text: text.into(),
            style: SpanStyle::Comment,
            action: Some(TuiAction::ExpandItem(key.to_string().into())),
        };
        self.layout.pos.x = self.layout.indent;
        match self.layout.level {
            Level::Brief => {
                self.render_span(&span("✻ ", SpanStyle::Comment), buf);
                self.render_span(&open(format!("Thought · {words} words")), buf);
                self.layout.pos.y += 1;
            }
            Level::Trimmed => {
                self.render_span(&span("✻ thinking", SpanStyle::Comment), buf);
                self.layout.pos.y += 1;
                if self.render_blocks_clipped(blocks, THINKING_PREVIEW_ROWS, buf) {
                    self.layout.pos.x = self.layout.indent;
                    self.render_span(&open(format!("⋯ show all {words} words")), buf);
                    self.layout.pos.y += 1;
                }
            }
            Level::Full => {
                self.render_span(
                    &span(format!("✻ thinking · {words} words · "), SpanStyle::Comment),
                    buf,
                );
                self.render_span(&open("show all".to_string()), buf);
                self.layout.pos.y += 1;
                self.render_blocks_pinned(blocks, THINKING_BOX_ROWS, buf);
            }
        }
    }

    /// An entry body's blocks, spaced as [`DocumentNode::Entry`] spaces them.
    fn render_blocks(&mut self, blocks: &[DocumentNode<'a>], buf: &mut Buffer) {
        for (idx, child) in blocks.iter().enumerate() {
            if idx > 0 {
                self.layout.pos.y += 1;
            }
            let saved_path = self.layout.node_path;
            self.layout.node_path.push(idx);
            self.render_node(child, buf);
            self.layout.node_path = saved_path;
        }
    }

    /// Lay `blocks` out without drawing them and return their height. Actions the
    /// walk recorded are discarded: nothing was placed.
    fn measure_blocks(&mut self, blocks: &[DocumentNode<'a>]) -> u16 {
        let saved_pos = self.layout.pos;
        let saved_measuring = std::mem::replace(&mut self.layout.measuring, true);
        let action_base = self.render_cache.actions.len();
        let mut sink = Buffer::empty(Rect::default());
        self.render_blocks(blocks, &mut sink);
        let height = self.layout.pos.y.saturating_sub(saved_pos.y);
        self.render_cache.actions.truncate(action_base);
        self.layout.measuring = saved_measuring;
        self.layout.pos = saved_pos;
        height
    }

    /// Render `blocks` drawing only rows `[shown_from, shown_from + rows)` of their
    /// own extent, placed at the current row, and leave the cursor below them.
    /// Actions outside the shown rows are dropped. `shown_from` may not exceed the
    /// current row: the hidden rows are laid out above it, through the clip.
    fn render_blocks_window(
        &mut self,
        blocks: &[DocumentNode<'a>],
        shown_from: u16,
        rows: u16,
        buf: &mut Buffer,
    ) {
        let start = self.layout.pos.y;
        let end = start.saturating_add(rows);
        let action_base = self.render_cache.actions.len();
        let outer = self.layout.clip;
        let (top, bottom) = outer.unwrap_or((0, u16::MAX));
        self.layout.clip = Some((top.max(start), bottom.min(end)));
        self.layout.pos.y = start - shown_from;
        self.render_blocks(blocks, buf);
        self.layout.clip = outer;
        let kept: Vec<_> = self
            .render_cache
            .actions
            .drain(action_base..)
            .filter(|(rect, _)| rect.y >= start && rect.y < end)
            .collect();
        self.render_cache.actions.extend(kept);
        self.layout.pos.y = end;
    }

    /// The first `rows` rows of `blocks`; true when there was more.
    fn render_blocks_clipped(
        &mut self,
        blocks: &[DocumentNode<'a>],
        rows: u16,
        buf: &mut Buffer,
    ) -> bool {
        let height = self.measure_blocks(blocks);
        if height <= rows {
            self.render_blocks(blocks, buf);
            false
        } else {
            self.render_blocks_window(blocks, 0, rows, buf);
            true
        }
    }

    /// The last `rows` rows of `blocks` — the box a long block streamed in, held at
    /// its last line. A body that fits renders whole.
    fn render_blocks_pinned(&mut self, blocks: &[DocumentNode<'a>], rows: u16, buf: &mut Buffer) {
        let height = self.measure_blocks(blocks);
        let start = self.layout.pos.y;
        if height <= rows {
            self.render_blocks(blocks, buf);
        } else if start >= height - rows {
            self.render_blocks_window(blocks, height - rows, rows, buf);
        } else {
            // Too near the top of the document to place the body above the box;
            // show its first rows instead.
            self.render_blocks_window(blocks, 0, rows, buf);
        }
    }
}
