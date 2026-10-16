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
fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}
fn undo(c: &mut Composer) {
    c.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL));
}
fn redo(c: &mut Composer) {
    c.handle_key(KeyEvent::new(KeyCode::Char('_'), KeyModifiers::ALT));
}

#[test]
fn undo_and_redo_restore_a_typing_burst() {
    let mut c = Composer::new();
    for ch in "hello".chars() {
        c.handle_key(key(ch));
    }

    undo(&mut c);
    assert_eq!(c.text(), "");
    redo(&mut c);
    assert_eq!(c.text(), "hello");
}

#[test]
fn cursor_motion_splits_typing_bursts() {
    let mut c = Composer::new();
    c.handle_key(key('a'));
    c.handle_key(key('b'));
    c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    c.handle_key(key('X'));

    undo(&mut c);
    assert_eq!(c.text(), "ab");
    undo(&mut c);
    assert_eq!(c.text(), "");
}

#[test]
fn newline_and_tab_are_separate_history_entries() {
    let mut c = Composer::new();
    c.handle_key(key('a'));
    c.handle_key(ctrl('j'));
    c.handle_key(key('b'));
    c.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    undo(&mut c);
    assert_eq!(c.text(), "a\nb");
    undo(&mut c);
    assert_eq!(c.text(), "a\n");
    undo(&mut c);
    assert_eq!(c.text(), "a");
    undo(&mut c);
    assert_eq!(c.text(), "");
}

#[test]
fn selection_replacement_undo_restores_selection() {
    let mut c = Composer::new();
    c.set_text("hello");
    c.handle_key(shift(KeyCode::Left));
    c.handle_key(shift(KeyCode::Left));
    c.handle_key(key('X'));

    undo(&mut c);
    assert_eq!(c.text(), "hello");
    assert_eq!(c.selection_range(), Some(3..5));
    redo(&mut c);
    assert_eq!(c.text(), "helX");
    assert_eq!(c.selection_range(), None);
}

#[test]
fn paste_and_cut_are_atomic_history_entries() {
    let mut c = Composer::new();
    c.paste_text("one\ntwo");
    undo(&mut c);
    assert_eq!(c.text(), "");
    redo(&mut c);
    assert_eq!(c.text(), "one\ntwo");

    c.handle_key(ctrl('h'));
    assert_eq!(c.cut_selection(), Some("one\ntwo".into()));
    undo(&mut c);
    assert_eq!(c.text(), "one\ntwo");
    assert_eq!(c.selection_range(), Some(0..c.text.len()));
}

#[test]
fn backward_and_forward_deletions_coalesce_separately() {
    let mut c = Composer::new();
    c.set_text("abcdef");
    c.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    c.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    undo(&mut c);
    assert_eq!(c.text(), "abcdef");

    c.handle_key(ctrl('a'));
    c.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    c.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    undo(&mut c);
    assert_eq!(c.text(), "abcdef");
}

#[test]
fn new_edit_after_undo_discards_redo() {
    let mut c = Composer::new();
    c.handle_key(key('a'));
    c.handle_key(ctrl('j'));
    c.handle_key(key('b'));
    undo(&mut c);
    c.handle_key(key('X'));
    redo(&mut c);
    assert_eq!(c.text(), "a\nX");
}

#[test]
fn clear_and_set_text_reset_history() {
    let mut c = Composer::new();
    c.handle_key(key('a'));
    c.clear();
    undo(&mut c);
    assert_eq!(c.text(), "");

    c.handle_key(key('b'));
    c.set_text("staged");
    undo(&mut c);
    assert_eq!(c.text(), "staged");
}

#[test]
fn history_is_bounded() {
    let mut c = Composer::new();
    for _ in 0..=HISTORY_LIMIT {
        c.handle_key(key('a'));
        c.handle_key(ctrl('j'));
    }
    for _ in 0..HISTORY_LIMIT {
        undo(&mut c);
    }
    let after_retained_undos = c.text();
    undo(&mut c);
    assert_eq!(c.text(), after_retained_undos);
}

#[test]
fn unicode_undo_restores_cursor_and_selection() {
    let mut c = Composer::new();
    c.set_text("e\u{301}🏳️‍🌈");
    c.handle_key(shift(KeyCode::Left));
    c.handle_key(key('x'));
    undo(&mut c);
    assert_eq!(c.text(), "e\u{301}🏳️‍🌈");
    assert_eq!(c.selection_range(), Some(3..c.text.len()));
    assert_eq!(c.cursor_col(), 1);
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
fn shift_motion_selects_graphemes_and_unmodified_motion_collapses() {
    let mut c = Composer::new();
    c.set_text("e\u{301}🏳️‍🌈x");

    c.handle_key(shift(KeyCode::Left));
    c.handle_key(shift(KeyCode::Left));
    assert_eq!(c.selection_range(), Some(3..c.text.len()));

    c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(c.cursor, 3);
    assert_eq!(c.selection_range(), None);

    c.handle_key(shift(KeyCode::Right));
    c.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(c.cursor, 17);
    assert_eq!(c.selection_range(), None);
}

#[test]
fn selected_text_is_replaced_or_deleted_before_ordinary_edits() {
    let mut c = Composer::new();
    c.set_text("abc\ndef");
    c.handle_key(ctrl('h'));
    c.handle_key(key('X'));
    assert_eq!(c.text(), "X");
    assert_eq!(c.selection_range(), None);

    c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    c.handle_key(shift(KeyCode::Right));
    c.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(c.text(), "");

    c.set_text("one two");
    c.handle_key(shift(KeyCode::Left));
    c.handle_key(shift(KeyCode::Left));
    c.handle_key(ctrl('w'));
    assert_eq!(c.text(), "one t");
}

#[test]
fn mouse_hit_testing_and_drag_selection_follow_wrapped_rows() {
    let mut c = Composer::new();
    c.set_text("a🏳️‍🌈bcdef");
    let area = Rect::new(0, 0, 6, 2); // prompt leaves four content columns

    c.set_cursor_from_mouse(c.cursor_for_screen_position(area, 2, 0), false);
    c.extend_selection_from_mouse(c.cursor_for_screen_position(area, 3, 1));
    assert_eq!(c.selection_range(), Some(0..17));

    c.set_cursor_from_mouse(c.cursor_for_screen_position(area, 3, 0), false);
    assert_eq!(c.cursor, 1);
    assert_eq!(c.selection_range(), None);
}

#[test]
fn screenplay_mouse_drag_and_shift_click_select_composer_text() {
    use crate::tui::harness::Screenplay;

    let mut play = Screenplay::new(24, 7);
    play.type_str("abcdef").drag(2, 5, 5, 5);
    assert_eq!(play.state.composer.selection_range(), Some(0..3));

    play.click_with_modifiers(7, 5, KeyModifiers::SHIFT);
    assert_eq!(play.state.composer.selection_range(), Some(0..5));
}

#[test]
fn super_z_routes_undo_and_redo() {
    use crate::tui::harness::Screenplay;

    let mut play = Screenplay::new(24, 7);
    play.type_str("hello")
        .route_key(KeyCode::Char('z'), KeyModifiers::SUPER);
    assert_eq!(play.state.composer.text(), "");

    play.route_key(
        KeyCode::Char('z'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    );
    assert_eq!(play.state.composer.text(), "hello");
}

#[test]
fn routed_clipboard_bindings_use_in_process_fallback() {
    use crate::tui::harness::Screenplay;

    let mut play = Screenplay::new(24, 7);
    play.disable_system_clipboard()
        .type_str("hello")
        .composer_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL))
        .composer_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT));
    assert_eq!(play.state.clipboard.fallback(), Some("hello"));
    assert_eq!(play.state.composer.text(), "hello");

    play.route_key(KeyCode::Char('x'), KeyModifiers::SUPER);
    assert_eq!(play.state.composer.text(), "");
    play.route_key(KeyCode::Char('v'), KeyModifiers::SUPER);
    assert_eq!(play.state.composer.text(), "hello");
    // `C-v` is the same clipboard read — the key that reaches the app where ⌘V is the
    // terminal's own and carries no image.
    play.route_key(KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert_eq!(play.state.composer.text(), "hellohello");

    play.composer_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL))
        .route_key(KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(play.state.composer.text(), "");
}

#[test]
fn ctrl_w_without_selection_still_deletes_a_word() {
    use crate::tui::harness::Screenplay;

    let mut play = Screenplay::new(24, 7);
    play.disable_system_clipboard()
        .type_str("foo bar")
        .route_key(KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(play.state.composer.text(), "foo ");
}

#[test]
fn ctrl_x_h_selects_the_whole_buffer() {
    let mut c = Composer::new();
    c.set_text("a🏳️‍🌈\nb");
    c.handle_key(ctrl('h'));
    assert_eq!(c.selection_range(), Some(0..c.text.len()));
    c.handle_key(key('x'));
    assert_eq!(c.text(), "x");
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

/// Routed through the real key path, an empty `⏎` at the operator's turn hands
/// control back (docs/tui.md): the continue fires and no turn is submitted, so
/// nothing reaches the wire.
#[test]
fn empty_enter_hands_control_back() {
    use crate::tui::harness::Screenplay;
    let mut play = Screenplay::new(80, 12);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(play.took_continue(), "the continue verb fired");
    assert_eq!(play.took_turn(), None, "no turn was submitted");
    assert!(play.render().contains("continuing"));
}

/// While the loop is running, the empty `⏎` stays the no-op it always was —
/// control is not the operator's to hand back, and the daemon would decline.
#[test]
fn empty_enter_is_inert_while_the_loop_runs() {
    use crate::tui::TuiEvent;
    use crate::tui::harness::Screenplay;
    let mut play = Screenplay::new(80, 12);
    play.event(TuiEvent::Phase(crate::daemon::LoopPhase::Requesting));
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!play.took_continue(), "nothing fired mid-turn");
    assert_eq!(play.took_turn(), None);
}

/// The `verbs` topic decides the empty `⏎` (docs/control-api.md): a refusal at idle is
/// stated on the status line without a round trip, and a continue the route accepts
/// mid-trajectory — a wait on the upstream it retries now — fires.
#[test]
fn empty_enter_follows_the_published_continue() {
    use crate::daemon::LoopPhase;
    use crate::tui::harness::Screenplay;
    use crate::tui::{TuiEvent, VerbsView};
    let mut play = Screenplay::new(80, 12);
    play.event(TuiEvent::Verbs(VerbsView {
        continue_refused: Some("nothing to continue: the last turn completed".into()),
        ..VerbsView::default()
    }));
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!play.took_continue(), "a refused continue is not sent");
    assert!(play.render().contains("nothing to continue"));

    play.event(TuiEvent::Phase(LoopPhase::Delayed { until: 0 }));
    play.event(TuiEvent::Verbs(VerbsView::default()));
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        play.took_continue(),
        "the retry-now continue fired mid-trajectory"
    );
}

/// A composer with text is unaffected: `⏎` still submits that text.
#[test]
fn a_typed_enter_still_submits() {
    use crate::tui::harness::Screenplay;
    let mut play = Screenplay::new(80, 12);
    play.type_str("hello");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(play.took_turn().as_deref(), Some("hello"));
    assert!(!play.took_continue());
}

/// The daemon's decline reaches the status line rather than the transcript.
#[test]
fn a_declined_continue_lands_in_the_status_line() {
    use crate::tui::TuiEvent;
    use crate::tui::harness::Screenplay;
    let mut play = Screenplay::new(80, 12);
    play.event(TuiEvent::Notice(
        "nothing to continue: the last turn completed".into(),
    ));
    assert!(play.render().contains("nothing to continue"));
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
