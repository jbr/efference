
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
