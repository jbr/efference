//! Small shared helpers: node lookup, truncation points, and cursor shape (docs/tui.md).

use crate::clients::tui::document::DocumentNode;
use crossterm::{queue, style::Print};
use ratatui::prelude::Backend;
use std::{env, io};

/// Detect if the terminal supports mouse cursor shape changes
pub(super) fn supports_cursor_shape() -> bool {
    // Kitty, WezTerm, and some other modern terminals support OSC 22
    env::var("TERM_PROGRAM").is_ok_and(|t| t == "kitty" || t == "WezTerm")
        || env::var("TERM").is_ok_and(|t| t.contains("kitty"))
}

/// Set the mouse cursor shape (for terminals that support it)
pub(super) fn set_cursor_shape<B: Backend + io::Write>(backend: &mut B, shape: &str) {
    // OSC 22 sequence: \x1b]22;<shape>\x07
    // Supported shapes: default, pointer, text, etc.
    let _ = queue!(backend, Print(format!("\x1b]22;{shape}\x07")));
    let _ = Backend::flush(backend);
}

/// The node a [`NodePath`](crate::clients::tui::document::NodePath) addresses, read-only —
/// the shared-borrow twin of [`find_node_at_path_mut`], for callers that only
/// inspect (the block menu asking what verbs the focused unit carries).
pub(super) fn find_node_at_path<'a, 'b>(
    nodes: &'a [DocumentNode<'b>],
    path: &[u16],
) -> Option<&'a DocumentNode<'b>> {
    let (&idx, rest) = path.split_first()?;
    let node = nodes.get(idx as usize)?;
    if rest.is_empty() {
        return Some(node);
    }
    match node {
        DocumentNode::Section { nodes, .. }
        | DocumentNode::BlockQuote { nodes }
        | DocumentNode::Captioned { nodes, .. }
        | DocumentNode::CallItem { nodes, .. } => find_node_at_path(nodes, rest),
        DocumentNode::Entry { blocks, .. } => find_node_at_path(blocks, rest),
        DocumentNode::ThinkingItem { entry, .. } => {
            find_node_at_path(std::slice::from_ref(entry), rest)
        }
        DocumentNode::List { items, .. } => {
            let (&item_idx, rest) = rest.split_first()?;
            find_node_at_path(&items.get(item_idx as usize)?.content, rest)
        }
        _ => None,
    }
}

pub(super) fn find_node_at_path_mut<'a, 'b>(
    nodes: &'a mut [DocumentNode<'b>],
    path: &[u16],
) -> Option<&'a mut DocumentNode<'b>> {
    if path.is_empty() {
        return None;
    }

    let idx = path[0] as usize;
    if idx >= nodes.len() {
        return None;
    }

    if path.len() == 1 {
        // This is the target node
        return Some(&mut nodes[idx]);
    }

    // Recurse into children
    let remaining_path = &path[1..];
    match &mut nodes[idx] {
        DocumentNode::Section { nodes, .. }
        | DocumentNode::BlockQuote { nodes }
        | DocumentNode::Captioned { nodes, .. }
        | DocumentNode::CallItem { nodes, .. } => find_node_at_path_mut(nodes, remaining_path),
        DocumentNode::Entry { blocks, .. } => find_node_at_path_mut(blocks, remaining_path),
        DocumentNode::ThinkingItem { entry, .. } => {
            find_node_at_path_mut(std::slice::from_mut(entry), remaining_path)
        }
        DocumentNode::List { items, .. } => {
            // Path into list items
            if remaining_path.is_empty() {
                return None;
            }
            let item_idx = remaining_path[0] as usize;
            if item_idx >= items.len() {
                return None;
            }
            find_node_at_path_mut(&mut items[item_idx].content, &remaining_path[1..])
        }
        _ => None,
    }
}

/// Find the best truncation point for Brief mode at second paragraph break
/// Returns the node index to stop at, or None to fall back to line-based truncation
pub(super) fn find_paragraph_truncation_point(
    _nodes: &[DocumentNode],
    _max_lines: u16,
    _screen_width: u16,
) -> Option<usize> {
    // There are no Span nodes to detect a paragraph break between: all inline
    // content sits in Paragraph nodes.
    None
}

/// Estimate how many lines a node will consume when rendered. (Width-independent
/// today — no arm wraps by width; the parameter was dropped after it went unused.)
pub(super) fn estimate_node_lines(node: &DocumentNode) -> u16 {
    match node {
        DocumentNode::Heading { .. } => 3, // Title + underline + spacing
        DocumentNode::CodeBlock { code, .. } => {
            code.lines().count() as u16 + 2 // Lines + spacing
        }
        DocumentNode::GeneratedCode { spans } => {
            // Count newlines in the spans
            let newlines = spans.iter().filter(|s| s.text.contains('\n')).count() as u16;
            newlines.max(1) + 1 // At least 1 line + spacing
        }
        DocumentNode::HorizontalRule => 1,
        DocumentNode::List { items, .. } => items.len() as u16, // Rough estimate
        DocumentNode::Diff { lines } => lines.len() as u16 + 1,
        DocumentNode::Entry { blocks, header, .. } => {
            blocks.iter().map(estimate_node_lines).sum::<u16>()
                + blocks.len().saturating_sub(1) as u16
                + u16::from(header.is_some())
        }
        _ => 2, // Default estimate for other nodes
    }
}
