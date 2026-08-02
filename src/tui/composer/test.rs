use super::*;
use crossterm::event::KeyCode;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
fn alt_backspace() -> KeyEvent {
    KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)
}
fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

#[test]
fn unicode_width_handles_representative_graphemes() {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    for (text, expected_width) in [("e\u{301}", 1), ("🏳️‍🌈", 2), ("🇺🇸", 2), ("👨‍👩‍👧‍👦", 2)]
    {
        assert_eq!(text.graphemes(true).collect::<Vec<_>>(), vec![text]);
        assert_eq!(text.width(), expected_width, "{text:?}");
    }
}

#[test]
fn grapheme_motion_and_deletion_are_atomic() {
    let mut c = Composer::new();
    c.set_text("e\u{301}🏳️‍🌈🇺🇸");
    assert_eq!(c.cursor_col(), 3);

    c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(c.cursor_col(), 2);
    c.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(c.text(), "e\u{301}🏳️‍🌈");
    assert_eq!(c.cursor_col(), 2);

    c.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(c.text(), "e\u{301}");
    assert_eq!(c.cursor_col(), 1);

    c.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(c.text(), "");
    assert_eq!(c.cursor, 0);
}

#[test]
fn combining_key_events_leave_the_cursor_after_the_combined_grapheme() {
    let mut c = Composer::new();
    c.handle_key(key('e'));
    c.handle_key(key('\u{301}'));
    assert_eq!(c.text(), "e\u{301}");
    assert_eq!(c.cursor_col(), 1);

    c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(c.cursor, 0);
    c.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(c.text(), "");
}

#[test]
fn vertical_motion_uses_grapheme_columns() {
    let mut c = Composer::new();
    c.set_text("a🏳️‍🌈b\nxy\n🇺🇸z");
    c.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(c.cursor_col(), 2);
    c.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(c.cursor_col(), 2);
    c.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(c.cursor_col(), 2);
    c.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(c.cursor_col(), 2);
}

#[test]
fn visual_rows_do_not_split_graphemes() {
    use unicode_segmentation::UnicodeSegmentation;

    let mut c = Composer::new();
    c.set_text("🏳️‍🌈🇺🇸");
    let rows = c.visual_rows(2);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| &c.text[row.start..row.end])
            .collect::<String>(),
        c.text()
    );
    let boundaries: Vec<_> = c
        .text
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .chain(std::iter::once(c.text.len()))
        .collect();
    for row in rows {
        assert!(boundaries.contains(&row.start));
        assert!(boundaries.contains(&row.end));
    }
}

/// Screenplay scenario: a line longer than the composer width. This captures
/// the *current* render so the wrap fix has a visible before/after. Watch the
/// right border in the snapshot — today the long line is clipped at the edge
/// instead of wrapping onto a second gutter row.
#[test]
fn composer_wraps_long_line() {
    use crate::tui::harness::Screenplay;
    let mut play = Screenplay::new(32, 7);
    play.type_str("the quick brown fox jumps over the lazy dog");
    play.snapshot("composer_wraps_long_line");
}

/// The caret (`^` beneath the row) sits just past the last char and advances
/// as text is typed — the behavior that was hard to confirm blind.
#[test]
fn cursor_advances_while_typing() {
    use crate::tui::harness::Screenplay;
    let mut play = Screenplay::new(32, 7);
    play.type_str("hello");
    play.snapshot("cursor_advances_while_typing");
}

/// After wrapping, the caret follows the text onto the second visual row
/// rather than staying stuck on the first.
#[test]
fn cursor_follows_text_across_a_wrap() {
    use crate::tui::harness::Screenplay;
    let mut play = Screenplay::new(32, 7);
    play.type_str("the quick brown fox jumps over the lazy");
    play.snapshot("cursor_follows_text_across_a_wrap");
}

/// The insertion point moves independently of the tail: three Left arrows
/// from the end land the caret inside the word, with text unchanged.
#[test]
fn cursor_moves_independently_of_the_tail() {
    use crate::tui::harness::Screenplay;
    use crossterm::event::KeyCode;
    let mut play = Screenplay::new(32, 7);
    play.type_str("hello world");
    for _ in 0..3 {
        play.key(KeyCode::Left, KeyModifiers::NONE);
    }
    play.snapshot("cursor_moves_independently_of_the_tail");
}

#[test]
fn types_and_submits() {
    let mut c = Composer::new();
    for ch in "hello".chars() {
        assert!(matches!(c.handle_key(key(ch)), ComposerAction::Consumed));
    }
    assert_eq!(c.text(), "hello");
    match c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
        ComposerAction::Submit(t) => assert_eq!(t, "hello"),
        _ => panic!("expected submit"),
    }
}

#[test]
fn empty_enter_does_not_submit() {
    let mut c = Composer::new();
    assert!(matches!(
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ComposerAction::Consumed
    ));
}

#[test]
fn ctrl_j_inserts_newline_and_text_joins() {
    let mut c = Composer::new();
    for ch in "ab".chars() {
        c.handle_key(key(ch));
    }
    c.handle_key(ctrl('j'));
    for ch in "cd".chars() {
        c.handle_key(key(ch));
    }
    assert_eq!(c.text(), "ab\ncd");
    assert_eq!(c.line_count(80), 2);
}

#[test]
fn emacs_motion_and_kill() {
    let mut c = Composer::new();
    for ch in "hello world".chars() {
        c.handle_key(key(ch));
    }
    c.handle_key(ctrl('a')); // line start
    c.handle_key(ctrl('k')); // kill to end
    assert_eq!(c.text(), "");
}

#[test]
fn ctrl_w_deletes_word_back() {
    let mut c = Composer::new();
    for ch in "foo bar".chars() {
        c.handle_key(key(ch));
    }
    c.handle_key(ctrl('w'));
    assert_eq!(c.text(), "foo ");
}

#[test]
fn alt_backspace_deletes_word_back() {
    let mut c = Composer::new();
    for ch in "foo bar".chars() {
        c.handle_key(key(ch));
    }
    c.handle_key(alt_backspace());
    assert_eq!(c.text(), "foo ");
}

#[test]
fn word_motion_treats_punctuation_as_separators() {
    let mut c = Composer::new();
    c.set_text("multi-word");
    c.handle_key(alt('b'));
    assert_eq!(c.cursor_col(), 6);

    c.set_text("multi_word");
    c.handle_key(alt('b'));
    assert_eq!(c.cursor_col(), 6);
}

#[test]
fn alt_d_deletes_only_the_next_word() {
    let mut c = Composer::new();
    c.set_text("word---next");
    c.handle_key(ctrl('a'));
    c.handle_key(alt('d'));
    assert_eq!(c.text(), "---next");

    c.handle_key(alt('d'));
    assert_eq!(c.text(), "");
}

#[test]
fn backspace_merges_lines() {
    let mut c = Composer::new();
    c.handle_key(key('a'));
    c.handle_key(ctrl('j'));
    c.handle_key(key('b'));
    // cursor after 'b' on line 2; move to line start then backspace merges.
    c.handle_key(ctrl('a'));
    c.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(c.text(), "ab");
    assert_eq!(c.line_count(80), 1);
}

#[test]
fn non_composer_keys_are_ignored() {
    let mut c = Composer::new();
    assert!(matches!(
        c.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
        ComposerAction::Ignored
    ));
}
