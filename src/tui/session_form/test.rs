use super::*;
use crate::tui::{TuiEvent, harness::Screenplay, state::UiMode};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

fn key(form: &mut Form, code: KeyCode) -> Action {
    form.handle(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn defaults_are_one_enter_and_cancel_has_no_draft() {
    let mut form = Form::default();
    assert_eq!(
        key(&mut form, KeyCode::Enter),
        Action::Start(Draft::default())
    );
    assert_eq!(key(&mut form, KeyCode::Esc), Action::Cancel);
}

#[test]
fn model_filter_select_reset_and_back() {
    let mut form = Form::default();
    for code in [KeyCode::Down, KeyCode::Enter, KeyCode::Down, KeyCode::Enter] {
        key(&mut form, code);
    }
    for c in "haiku".chars() {
        key(&mut form, KeyCode::Char(c));
    }
    key(&mut form, KeyCode::Enter);
    assert_eq!(form.draft.main.as_deref(), Some("work/claude-haiku-4-5"));
    key(&mut form, KeyCode::Char('r'));
    assert_eq!(form.draft.main, None);
    key(&mut form, KeyCode::Enter);
    key(&mut form, KeyCode::Down);
    key(&mut form, KeyCode::Enter);
    key(&mut form, KeyCode::Esc);
    assert!(matches!(form.page, Page::Connections { .. }));
    key(&mut form, KeyCode::Esc);
    assert_eq!(form.page, Page::Form);
}

#[test]
fn freeform_and_failed_creation_preserve_edits() {
    let mut form = Form::default();
    for _ in 0..5 {
        key(&mut form, KeyCode::Down);
    }
    key(&mut form, KeyCode::Enter);
    for _ in 0..3 {
        key(&mut form, KeyCode::Down);
    }
    key(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Text { .. }));
    key(&mut form, KeyCode::Enter);
    assert!(form.note.is_some());
    for c in "vendor/new-model".chars() {
        key(&mut form, KeyCode::Char(c));
    }
    key(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.triage.as_deref(),
        Some("router/vendor/new-model")
    );
    let draft = form.draft.clone();
    form.creation_failed("demo error".into());
    assert_eq!(form.draft, draft);
    for _ in 0..5 {
        key(&mut form, KeyCode::Up);
    }
    assert_eq!(key(&mut form, KeyCode::Enter), Action::Start(draft));
}

#[test]
fn temperature_validation_does_not_exit_editor() {
    let mut form = Form::default();
    for _ in 0..3 {
        key(&mut form, KeyCode::Down);
    }
    key(&mut form, KeyCode::Enter);
    form.paste("101");
    key(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Temperature { .. }));
    key(&mut form, KeyCode::Backspace);
    key(&mut form, KeyCode::Enter);
    assert_eq!(form.draft.temperature, Some(10));
}

#[test]
fn embedded_cancel_returns_to_sessions_without_host_command_or_quit() {
    let mut play = Screenplay::new(100, 30);
    play.event(TuiEvent::SessionFormPrototype);
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_some()));
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_some()));
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_none()));
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(!play.key_quits(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_none()));
    assert!(play.took_host_cmd().is_none());
}

#[test]
fn render_is_rect_relative_and_survives_small_viewports() {
    let styles = render::Styles {
        text: Style::default(),
        title: Style::default(),
        hint: Style::default(),
        selected: Style::default(),
    };
    let mut form = Form::default();
    for (w, h) in [(80, 15), (40, 9), (1, 1), (0, 0)] {
        let mut origin = Buffer::empty(Rect::new(0, 0, w, h));
        let area = Rect::new(3, 4, w, h);
        let mut shifted = Buffer::empty(area);
        form.render(&mut origin, Rect::new(0, 0, w, h), styles);
        form.render(&mut shifted, area, styles);
        assert_eq!(origin.content, shifted.content);
    }
    form.selected = 5;
    let area = Rect::new(0, 0, 50, 9);
    let mut buf = Buffer::empty(area);
    form.render(&mut buf, area, styles);
    let text: String = buf.content.iter().map(|c| c.symbol()).collect();
    assert!(text.contains("Triage model"));
}

#[test]
fn paste_is_text_not_form_commands() {
    let mut form = Form::default();
    form.draft.main = Some("personal/gpt-5.6".into());
    key(&mut form, KeyCode::Down);
    form.paste("r\n");
    assert_eq!(form.draft.main.as_deref(), Some("personal/gpt-5.6"));
    key(&mut form, KeyCode::Enter);
    key(&mut form, KeyCode::Down);
    key(&mut form, KeyCode::Enter);
    form.paste("haiku\n");
    assert!(matches!(&form.page, Page::Models { query, .. } if query == "haiku"));
}

#[test]
fn embedded_form_and_creation_error_snapshots() {
    let mut play = Screenplay::new(100, 30);
    play.event(TuiEvent::SessionFormPrototype);
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    play.snapshot("session_form_embedded");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.snapshot("session_form_creation_error");
    assert!(play.took_host_cmd().is_none());
}
