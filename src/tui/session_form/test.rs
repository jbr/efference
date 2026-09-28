use super::*;
use crate::inference::{
    ConnectionInventory, InferenceConfig, InferenceRegistry, ResolvedOptions, SessionOptions,
};
use crate::tui::{TuiEvent, harness::Screenplay, state::UiMode};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

/// Connection rows built the way the host builds them — from configuration,
/// through the registry — so the form is exercised against real inventory
/// output rather than a hand-written shape of it. `blank` is disabled, which is
/// how an unavailable connection reaches a client: listed, not omitted.
fn connections() -> Vec<ConnectionInventory> {
    let config = InferenceConfig::from_toml(
        r#"
[defaults]
model = "work/claude-sonnet-4-5"

[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "TEST_KEY" }

[connections.lab]
provider = "openai"
auth = { kind = "env", key = "TEST_KEY" }

[connections.blank]
provider = "openai"
auth = { kind = "env", key = "TEST_KEY" }
disabled = true
"#,
    )
    .unwrap();
    let env = crate::env_table::EnvSnapshot::new(
        [("TEST_KEY".to_string(), "test-secret".to_string())].into(),
    )
    .unwrap();
    InferenceRegistry::from_config(
        config,
        std::path::Path::new("/host"),
        env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
    .inventory()
}

/// The host's answer about a draft, produced by the same resolver the control
/// route runs, so the form folds in a real `effective`/`issues` pair.
fn resolve(options: &SessionOptions) -> ResolvedOptions {
    let config = InferenceConfig::from_toml(
        r#"
[defaults]
model = "work/claude-sonnet-4-5"

[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "TEST_KEY" }

[connections.lab]
provider = "openai"
auth = { kind = "env", key = "TEST_KEY" }

[connections.blank]
provider = "openai"
auth = { kind = "env", key = "TEST_KEY" }
disabled = true
"#,
    )
    .unwrap();
    let env = crate::env_table::EnvSnapshot::new(
        [("TEST_KEY".to_string(), "test-secret".to_string())].into(),
    )
    .unwrap();
    InferenceRegistry::from_config(
        config,
        std::path::Path::new("/host"),
        env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
    .resolve_options(options, None)
}

fn context() -> NewSessionContext {
    NewSessionContext {
        cwd: std::path::PathBuf::from("/code/example"),
        connections: connections(),
    }
}

fn form() -> Form {
    let mut form = Form::new(context());
    // Every container resolves the untouched draft as it opens, so a form under
    // test starts where a real one does.
    let opening = form.initial_preview();
    settle(&mut form, opening);
    form
}

/// Answer whatever the form asked for, the way a container's dispatcher would:
/// resolve the options it sent and hand the reply back, repeating while the
/// form supersedes its own request.
fn settle(form: &mut Form, mut action: Action) {
    while let Action::Preview(options) = action {
        action = form.preview_returned(Ok(resolve(&options)));
    }
}

fn key(form: &mut Form, code: KeyCode) -> Action {
    form.handle(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The same press with the reply folded in, for the cases that care about the
/// form's resulting state rather than the request it emitted.
fn press(form: &mut Form, code: KeyCode) {
    let action = key(form, code);
    settle(form, action);
}

/// The index of the row a field's issue attaches to is not what this asserts —
/// only that the untouched draft resolves, so `Start session` is live without
/// the operator editing anything.
#[test]
fn an_untouched_draft_resolves_from_the_hosts_defaults() {
    let mut form = form();
    let resolved = form.resolved.as_ref().expect("the opening preview landed");
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
    assert_eq!(
        resolved
            .effective
            .main
            .value
            .as_ref()
            .map(|s| (s.connection.as_str(), s.model.as_str())),
        Some(("work", "claude-sonnet-4-5"))
    );
    // Nothing was edited, so creation sends no overrides at all: the host
    // resolves the same defaults again (docs/control-api.md).
    assert_eq!(
        key(&mut form, KeyCode::Enter),
        Action::Create(SessionOptions::default())
    );
}

/// Creation is refused while a field the host flagged is still unresolved,
/// rather than spending a round trip to be told what is already on screen.
#[test]
fn a_flagged_field_holds_creation_and_names_the_control() {
    let mut form = form();
    // Main → `blank`, the disabled connection, whose credential did not load.
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Text { .. }));
    form.paste("gpt-5.6\n");
    press(&mut form, KeyCode::Enter);
    let resolved = form.resolved.as_ref().expect("a preview landed");
    assert_eq!(
        resolved
            .issues
            .iter()
            .map(|i| i.field.as_ref())
            .collect::<Vec<_>>(),
        ["model"]
    );
    // A field with an issue has neither a value nor a source: the two nulls are
    // what distinguishes it from a field nothing named.
    assert!(resolved.effective.main.value.is_none());
    assert!(resolved.effective.main.source.is_none());
    for _ in 0..1 {
        press(&mut form, KeyCode::Up);
    }
    assert_eq!(key(&mut form, KeyCode::Enter), Action::Continue);
    assert!(!form.creating);
}

/// The draft carries the connection and the native model id as two fields, so a
/// model id containing a slash is never re-split as a connection prefix.
#[test]
fn a_native_id_with_a_slash_keeps_its_connection_separate() {
    let mut form = form();
    // Triage → the `work` connection, which this build compiles no catalog for,
    // so the picker goes straight to the native-id editor.
    for _ in 0..5 {
        press(&mut form, KeyCode::Down);
    }
    press(&mut form, KeyCode::Enter);
    // Past "use the default" and "share the main client", then to `work` —
    // third of `blank`, `lab`, `work`.
    for _ in 0..4 {
        press(&mut form, KeyCode::Down);
    }
    press(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Text { .. }));
    // An empty id is refused locally and does not leave the editor.
    press(&mut form, KeyCode::Enter);
    assert!(form.note.is_some());
    assert!(matches!(form.page, Page::Text { .. }));
    for c in "vendor/new-model".chars() {
        press(&mut form, KeyCode::Char(c));
    }
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.role(Role::Triage),
        Some(&RoleDraft {
            connection: "work".into(),
            model: "vendor/new-model".into(),
        })
    );
    assert_eq!(
        form.draft.to_options().triage,
        Some(crate::inference::TriageChoice::Separate {
            connection: Some("work".into()),
            model: "vendor/new-model".into(),
            thinking: false,
            auto_choice: false,
            effort: None,
        })
    );
}

/// Sharing main's client and inheriting are different answers, and the effective
/// view reports them differently even though both render as the main client
/// here: `request` versus `shared_main` (docs/control-api.md).
#[test]
fn explicitly_sharing_main_is_not_the_same_as_inheriting() {
    use crate::inference::SelectionSource;
    let mut form = form();
    assert_eq!(
        form.resolved.as_ref().unwrap().effective.triage.source,
        Some(SelectionSource::SharedMain)
    );
    for _ in 0..5 {
        press(&mut form, KeyCode::Down);
    }
    press(&mut form, KeyCode::Enter);
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    assert_eq!(form.draft.triage, TriageDraft::Shared);
    assert_eq!(
        form.resolved.as_ref().unwrap().effective.triage.source,
        Some(SelectionSource::Request)
    );
    // `r` clears the override back to inheriting, not to sharing.
    press(&mut form, KeyCode::Char('r'));
    assert_eq!(form.draft.triage, TriageDraft::Inherit);
    assert_eq!(
        form.resolved.as_ref().unwrap().effective.triage.source,
        Some(SelectionSource::SharedMain)
    );
}

/// A reply that arrives after the draft moved on describes a draft nobody is
/// looking at: it is dropped, and the current draft is re-sent.
#[test]
fn a_superseded_preview_is_dropped_and_re_requested() {
    let mut form = form();
    // `r` resets a field, so the cursor has to be on one.
    press(&mut form, KeyCode::Down);
    let sent = match key(&mut form, KeyCode::Char('r')) {
        Action::Preview(options) => options,
        other => panic!("expected a preview request, got {other:?}"),
    };
    // Edit again before answering, then answer the earlier request.
    form.draft.temperature = Some(42);
    let stale = resolve(&sent);
    match form.preview_returned(Ok(stale)) {
        Action::Preview(options) => assert_eq!(options.temperature, Some(42)),
        other => panic!("expected a re-request, got {other:?}"),
    }
}

#[test]
fn a_typed_model_id_is_accepted_and_reset_and_back() {
    let mut form = form();
    // Main → `lab`, the available connection: second of `blank`, `lab`, `work`.
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    for _ in 0..2 {
        press(&mut form, KeyCode::Down);
    }
    press(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Text { .. }));
    for c in "gpt-5.6-luna".chars() {
        press(&mut form, KeyCode::Char(c));
    }
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.role(Role::Main),
        Some(&RoleDraft {
            connection: "lab".into(),
            model: "gpt-5.6-luna".into(),
        })
    );
    press(&mut form, KeyCode::Char('r'));
    assert_eq!(form.draft.main, None);
    // Escape backs out of an editor to its picker before it cancels the form.
    press(&mut form, KeyCode::Enter);
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    press(&mut form, KeyCode::Esc);
    assert!(matches!(form.page, Page::Connections { .. }));
    press(&mut form, KeyCode::Esc);
    assert_eq!(form.page, Page::Form);
}

/// Every connection opens the native-id editor: this build compiles no catalog
/// to suggest from, and the host's discovery route is what will offer a menu
/// (backlog/session-model-discovery.md).
#[test]
fn every_connection_opens_the_id_editor() {
    let mut form = form();
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    // `blank`, `lab`, `work` — sorted by name, so `work` is the third row.
    for _ in 0..3 {
        press(&mut form, KeyCode::Down);
    }
    press(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Text { .. }));
}

#[test]
fn temperature_validation_does_not_exit_editor() {
    let mut form = form();
    for _ in 0..3 {
        press(&mut form, KeyCode::Down);
    }
    press(&mut form, KeyCode::Enter);
    form.paste("101");
    press(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Temperature { .. }));
    press(&mut form, KeyCode::Backspace);
    press(&mut form, KeyCode::Enter);
    assert_eq!(form.draft.temperature, Some(10));
    assert_eq!(
        form.resolved.as_ref().unwrap().effective.temperature.value,
        Some(10)
    );
}

#[test]
fn failed_creation_preserves_every_edit() {
    let mut form = form();
    // Main → `lab`, with a model id typed into the editor.
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    for _ in 0..2 {
        press(&mut form, KeyCode::Down);
    }
    press(&mut form, KeyCode::Enter);
    form.paste("gpt-5.6\n");
    press(&mut form, KeyCode::Enter);
    assert_eq!(form.page, Page::Form);
    let draft = form.draft.clone();
    press(&mut form, KeyCode::Up);
    let Action::Create(options) = key(&mut form, KeyCode::Enter) else {
        panic!("a resolved draft starts creating")
    };
    assert!(form.creating);
    form.creation_failed("the host refused: demo".into());
    assert_eq!(form.draft, draft);
    assert!(!form.creating);
    assert_eq!(form.note.as_deref(), Some("the host refused: demo"));
    // The same draft is still what a retry would send.
    assert_eq!(key(&mut form, KeyCode::Enter), Action::Create(options));
}

#[test]
fn paste_is_text_not_form_commands() {
    let mut form = form();
    press(&mut form, KeyCode::Down);
    form.paste("r\n");
    assert_eq!(form.draft.main, None);
    press(&mut form, KeyCode::Enter);
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Enter);
    form.paste("gpt-5.6-luna\n");
    // The newline is not a keystroke: the pasted text lands in the editor and
    // Enter is still the operator's to press.
    assert!(matches!(&form.page, Page::Text { value, .. } if value == "gpt-5.6-luna"));
}

#[test]
fn render_is_rect_relative_and_survives_small_viewports() {
    let styles = render::Styles {
        text: Style::default(),
        title: Style::default(),
        hint: Style::default(),
        selected: Style::default(),
    };
    let mut form = form();
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
    assert!(text.contains("triage model"), "{text}");
}

/// `n` does not open the form on its own: it asks the host where a session would
/// be created and what connections exist, and the reply is what opens it.
#[test]
fn opening_the_form_reads_the_host_first() {
    let mut play = Screenplay::new(100, 30);
    play.state.open_sessions();
    play.event(TuiEvent::Sessions(Ok(Vec::new())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(
        play.took_host_cmd(),
        Some(crate::tui::HostCmd::NewSession)
    ));
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_none()));
    play.event(TuiEvent::NewSession(Ok(context())));
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_some()));
    // Opening resolves the untouched draft without an edit.
    assert!(matches!(
        play.took_host_cmd(),
        Some(crate::tui::HostCmd::Preview(_))
    ));
}

/// The form owns Escape and Ctrl-C: neither reaches the attached session's loop,
/// and cancelling returns to the listing rather than quitting.
#[test]
fn embedded_cancel_returns_to_sessions_without_quitting() {
    let mut play = Screenplay::new(100, 30);
    play.state.open_sessions();
    play.event(TuiEvent::Sessions(Ok(Vec::new())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    play.event(TuiEvent::NewSession(Ok(context())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_some()));
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_none()));
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    play.event(TuiEvent::NewSession(Ok(context())));
    assert!(!play.key_quits(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_none()));
}

/// A created session is the one the TUI attaches to next, in the working root
/// the form created it in (docs/inbox.md).
#[test]
fn creation_switches_to_the_new_session() {
    let mut play = Screenplay::new(100, 30);
    play.state.open_sessions();
    play.event(TuiEvent::Sessions(Ok(Vec::new())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    play.event(TuiEvent::NewSession(Ok(context())));
    assert!(play.event_quits(TuiEvent::Created(Ok("brave-otter".into()))));
    assert_eq!(
        play.state.switch_to,
        Some(crate::tui::Exit::Switch {
            id: "brave-otter".into(),
        })
    );
}

/// A refused creation keeps the form open with its edits and the host's reason,
/// because nothing was minted to reconcile with.
#[test]
fn a_refused_creation_keeps_the_form_open() {
    let mut play = Screenplay::new(100, 30);
    play.state.open_sessions();
    play.event(TuiEvent::Sessions(Ok(Vec::new())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    play.event(TuiEvent::NewSession(Ok(context())));
    play.event(TuiEvent::Created(Err("model: not configured".into())));
    assert!(matches!(&play.state.ui_mode, UiMode::Sessions(menu) if menu.form.is_some()));
    assert_eq!(play.state.switch_to, None);
}

#[test]
fn embedded_form_snapshots() {
    let mut play = Screenplay::new(100, 30);
    play.state.open_sessions();
    play.event(TuiEvent::Sessions(Ok(Vec::new())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(
        play.took_host_cmd(),
        Some(crate::tui::HostCmd::NewSession)
    ));
    play.event(TuiEvent::NewSession(Ok(context())));
    play.snapshot("session_form_embedded_unresolved");
    let options = match play.took_host_cmd() {
        Some(crate::tui::HostCmd::Preview(options)) => options,
        other => panic!("expected a preview request, got {other:?}"),
    };
    play.event(TuiEvent::Preview(Ok(resolve(&options))));
    play.snapshot("session_form_embedded");
    play.event(TuiEvent::Created(Err(
        "model: connection \"blank\" is unavailable".into(),
    )));
    play.snapshot("session_form_creation_error");
}

/// The form borrows the full-screen theme's styles, each of which carries its own
/// surface's background; lent as-is they painted a band under every gutter, a box
/// under each source label, and a filled blank status row. No cell of the drawn
/// form carries a background: its surface is whatever it is drawn on.
#[test]
fn the_form_paints_no_backgrounds_of_its_own() {
    let theme = crate::tui::theme::InteractiveTheme::from_render_context(
        &crate::tui::render_context::RenderContext::new(),
    );
    let styles = render::Styles::from(&theme);
    let area = Rect::new(0, 0, 80, 15);
    let mut buffer = Buffer::empty(area);
    form().render(&mut buffer, area, styles);
    let painted: Vec<_> = buffer
        .content
        .iter()
        .filter(|cell| cell.bg != ratatui::style::Color::Reset)
        .map(|cell| cell.symbol().to_string())
        .collect();
    assert!(painted.is_empty(), "cells with a background: {painted:?}");
}
