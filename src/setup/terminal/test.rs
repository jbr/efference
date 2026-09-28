use super::*;

/// The row count behind `erase` measures what the screen shows: the prompter's
/// own color escapes take no columns, and a wide character takes two.
#[test]
fn visible_width_skips_escapes_and_counts_display_columns() {
    let painted = format!("{} {}", "✓".green(), "Set up ~/code now?".bold());
    assert_eq!(visible_width(&painted), 20);
    assert_eq!(visible_width("日本"), 4);
    assert_eq!(visible_width(""), 0);
}
