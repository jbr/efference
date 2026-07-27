
use super::*;

fn plain_cell(text: &'static str) -> TableCell<'static> {
    TableCell {
        spans: vec![Span::plain(text)],
    }
}

fn multi_span_cell(parts: &[&'static str]) -> TableCell<'static> {
    TableCell {
        spans: parts.iter().map(|p| Span::plain(*p)).collect(),
    }
}

fn line_text(line: &[Span]) -> String {
    line.iter().map(|s| s.text.as_ref()).collect()
}

#[test]
fn natural_widths_when_table_fits() {
    let header = vec![plain_cell("a"), plain_cell("b")];
    let rows = vec![vec![plain_cell("hi"), plain_cell("there")]];
    let layout = lay_out(Some(&header), &rows, 80);
    assert_eq!(layout.col_widths, vec![2, 5]);
}

#[test]
fn shrinks_widest_column_when_overflowing() {
    let header = vec![plain_cell("a"), plain_cell("b")];
    let rows = vec![vec![
        plain_cell("short"),
        plain_cell("a much longer chunk of prose that wants to wrap"),
    ]];
    let layout = lay_out(Some(&header), &rows, 30);
    // budget = 30 - 3 borders = 27 columns; shorter col stays at natural.
    assert!(layout.col_widths[0] <= 5);
    assert!(layout.col_widths[1] >= 8);
    assert!(layout.col_widths.iter().sum::<usize>() <= 27);
}

#[test]
fn wraps_long_cell_across_multiple_lines() {
    let rows = vec![vec![plain_cell(
        "Skip the Alt-Svc cache and dial QUIC directly.",
    )]];
    let layout = lay_out(None, &rows, 25);
    let cell = &layout.rows[0][0];
    assert!(cell.lines.len() > 1, "expected wrap, got {:?}", cell.lines);
    for line in &cell.lines {
        let w: usize = line.iter().map(span_display_width).sum();
        assert!(
            w <= layout.col_widths[0],
            "line {w} > {}",
            layout.col_widths[0]
        );
    }
    let joined: String = cell
        .lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains("Skip the Alt-Svc"));
    assert!(joined.contains("directly."));
}

#[test]
fn multi_span_cell_keeps_bytes_intact() {
    // Cell has three spans like a markdown link: "Falls back to " + "Client::new" + "."
    // The bug was that each span was independently truncated to col_width,
    // letting the cell exceed col_width. With wrapping, total width per
    // line stays within col_width.
    let cell = multi_span_cell(&["Falls back to ", "Client::new_with_quic", "."]);
    let layout = lay_out(None, &[vec![cell]], 25);
    let laid = &layout.rows[0][0];
    for line in &laid.lines {
        let w: usize = line.iter().map(span_display_width).sum();
        assert!(
            w <= layout.col_widths[0],
            "line width {w} > col {}",
            layout.col_widths[0]
        );
    }
    let joined: String = laid
        .lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains("Falls back to"));
    assert!(joined.contains("Client::new_with_quic"));
}

#[test]
fn handles_unicode_emdash_correctly() {
    // The em-dash is 3 bytes / 1 column; byte-based truncation would have
    // miscounted width and could split it.
    let rows = vec![vec![plain_cell(
        "No fallback — a non-h2-speaking server surfaces an error.",
    )]];
    let layout = lay_out(None, &rows, 25);
    for line in &layout.rows[0][0].lines {
        let w: usize = line.iter().map(span_display_width).sum();
        assert!(w <= layout.col_widths[0]);
        // No partial UTF-8 sequences anywhere.
        for span in line {
            assert!(span.text.is_char_boundary(0));
            assert!(span.text.is_char_boundary(span.text.len()));
        }
    }
}

#[test]
fn hard_break_word_longer_than_column() {
    let rows = vec![vec![plain_cell("supercalifragilisticexpialidocious")]];
    let layout = lay_out(None, &rows, 14); // budget 14 - 2 borders = 12
    let cell = &layout.rows[0][0];
    assert!(cell.lines.len() >= 2);
    for line in &cell.lines {
        let w: usize = line.iter().map(span_display_width).sum();
        assert!(w <= layout.col_widths[0]);
    }
    let joined: String = cell
        .lines
        .iter()
        .flat_map(|l| l.iter().map(|s| s.text.as_ref()))
        .collect();
    assert_eq!(joined, "supercalifragilisticexpialidocious");
}

#[test]
fn explicit_newline_forces_break() {
    let rows = vec![vec![plain_cell("line one\nline two")]];
    let layout = lay_out(None, &rows, 80);
    let cell = &layout.rows[0][0];
    assert_eq!(cell.lines.len(), 2);
    assert_eq!(line_text(&cell.lines[0]), "line one");
    assert_eq!(line_text(&cell.lines[1]), "line two");
}

#[test]
fn empty_cell_yields_one_empty_line() {
    let rows = vec![vec![plain_cell("")]];
    let layout = lay_out(None, &rows, 80);
    assert_eq!(layout.rows[0][0].lines.len(), 1);
    assert!(layout.rows[0][0].lines[0].is_empty());
}
