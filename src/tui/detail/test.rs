use super::*;

#[test]
fn levels_follow_distance_from_the_tail() {
    assert_eq!(level_for(0, false), Level::Full);
    assert_eq!(level_for(RECENT - 1, false), Level::Full);
    assert_eq!(level_for(RECENT, false), Level::Trimmed);
    assert_eq!(level_for(ANCIENT - 1, false), Level::Trimmed);
    assert_eq!(level_for(ANCIENT, false), Level::Brief);
}

#[test]
fn a_call_that_must_show_is_whole_at_any_distance() {
    assert_eq!(level_for(ANCIENT * 10, true), Level::Full);
}

#[test]
fn words_are_counted_across_whitespace() {
    assert_eq!(word_count("  one two\n\nthree  "), 3);
    assert_eq!(word_count(""), 0);
}
