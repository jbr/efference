use super::{display_width, wrap_points};

/// Split `text` at the byte boundaries `wrap_points` reports, for legible
/// assertions on the visual rows.
fn rows(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev = 0;
    for b in wrap_points(text, width) {
        out.push(text[prev..b].to_string());
        prev = b;
    }
    out.push(text[prev..].to_string());
    out
}

#[test]
fn short_text_is_one_row() {
    assert!(wrap_points("hello", 40).is_empty());
}

#[test]
fn breaks_at_word_boundaries() {
    // 30 cols: the break falls on a space, never mid-word, and the breaking
    // space stays on the preceding row so the next starts clean.
    let r = rows("the quick brown fox jumps over the lazy dog", 30);
    assert!(r.len() >= 2, "wrapped into multiple rows");
    for row in &r {
        assert!(display_width(row) <= 30, "row {row:?} within width");
        assert!(!row.starts_with(' '), "no leading space on {row:?}");
    }
    // Reassembling the rows reproduces the input exactly (no dropped chars) —
    // the property the composer's cursor mapping depends on.
    assert_eq!(r.concat(), "the quick brown fox jumps over the lazy dog");
}

#[test]
fn hard_breaks_a_word_longer_than_the_width() {
    // No whitespace to break on, so it cuts to make progress.
    let r = rows("superlongunbreakabletoken", 10);
    assert!(r.len() >= 3);
    assert_eq!(r.concat(), "superlongunbreakabletoken");
    assert!(r.iter().all(|row| display_width(row) <= 10));
}

#[test]
fn zero_width_never_loops() {
    assert!(wrap_points("anything", 0).is_empty());
}
