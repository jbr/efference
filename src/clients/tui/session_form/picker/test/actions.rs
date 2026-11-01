//! What picks, refreshes, and catalog answers do to the form.

use super::*;

#[test]
fn a_miss_asks_for_one_refresh_per_connection_per_visit() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    let mut refreshes = Vec::new();
    for c in "gpt-9-new".chars() {
        let action = key(&mut form, KeyCode::Char(c));
        if let Action::RefreshCatalog {
            connection,
            max_age,
        } = action
        {
            refreshes.push((connection, max_age));
        }
    }
    // The first miss was at "gpt-9"; later keystrokes stay quiet.
    assert_eq!(refreshes, [("lab".to_string(), Some(60))]);
    // Leaving the picker and coming back is a new visit.
    press(&mut form, KeyCode::Esc);
    press(&mut form, KeyCode::Esc);
    assert_eq!(form.page, Page::Form);
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    type_str(&mut form, "gpt-");
    let action = key(&mut form, KeyCode::Char('9'));
    assert!(
        matches!(action, Action::RefreshCatalog { .. }),
        "{action:?}"
    );
}

#[test]
fn a_match_or_an_unavailable_connection_asks_for_nothing() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    for c in "gpt".chars() {
        assert_eq!(key(&mut form, KeyCode::Char(c)), Action::Continue);
    }
    let mut blank = self::form();
    open_picker(&mut blank, Role::Main);
    type_str(&mut blank, "blank");
    press(&mut blank, KeyCode::Enter);
    for c in "gpt".chars() {
        assert_eq!(key(&mut blank, KeyCode::Char(c)), Action::Continue);
    }
}

#[test]
fn enter_on_a_leaf_sets_the_draft_and_asks_for_a_preview() {
    let mut form = form();
    open_picker(&mut form, Role::Spotter);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    press(&mut form, KeyCode::Down);
    let action = key(&mut form, KeyCode::Enter);
    assert!(matches!(action, Action::Preview(_)), "{action:?}");
    assert_eq!(form.page, Page::Form);
    assert_eq!(
        form.draft.role(Role::Spotter),
        Some(&RoleDraft {
            connection: "lab".into(),
            model: "gpt-5.6-mini".into()
        })
    );
    settle(&mut form, action);
}

#[test]
fn enter_on_a_recent_row_takes_both_halves() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.role(Role::Main),
        Some(&RoleDraft {
            connection: "lab".into(),
            model: "gpt-5.6".into()
        })
    );
}

#[test]
fn tab_on_a_leaf_fills_the_query_with_its_relative_id() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    type_str(&mut form, "fl");
    press(&mut form, KeyCode::Tab);
    assert_eq!(query(&form), "glm-5.3-flash");
    assert_eq!(chips(&form), ["router", "z-ai"]);
    // The cursor sits at the end, so a variant is typed straight on.
    type_str(&mut form, "-x");
    assert_eq!(ids(&form), ["use `z-ai/glm-5.3-flash-x` as model id"]);
}

#[test]
fn escape_clears_a_query_before_it_leaves() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    type_str(&mut form, "gl");
    press(&mut form, KeyCode::Esc);
    assert_eq!(query(&form), "");
    assert_eq!(chips(&form), ["router", "z-ai"]);
    press(&mut form, KeyCode::Esc);
    assert_eq!(form.page, Page::Form);
    assert_eq!(form.draft.main, None);
}

#[test]
fn a_changed_query_resets_the_selection_and_movement_does_not() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    press(&mut form, KeyCode::Down);
    press(&mut form, KeyCode::Down);
    assert_eq!(selected(&form), 2);
    type_str(&mut form, "l");
    assert_eq!(selected(&form), 0);
    press(&mut form, KeyCode::Down);
    let action = form.handle(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(action, Action::Continue);
    assert_eq!(selected(&form), 2);
    form.handle(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(selected(&form), 1);
}

#[test]
fn the_selection_stays_inside_the_rows() {
    let mut form = form();
    open_picker(&mut form, Role::Spotter);
    for _ in 0..20 {
        press(&mut form, KeyCode::Down);
    }
    assert_eq!(selected(&form), 3);
    for _ in 0..20 {
        press(&mut form, KeyCode::Up);
    }
    assert_eq!(selected(&form), 0);
}

#[test]
fn a_catalog_answer_replaces_the_entry_and_bounds_the_selection() {
    let mut form = form();
    open_picker(&mut form, Role::Spotter);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    press(&mut form, KeyCode::Down);
    assert_eq!(selected(&form), 1);
    let fewer = entry(
        Provider::OpenAI,
        "lab",
        CatalogStatus::Fresh,
        vec![listed(Provider::OpenAI, "lab", "gpt-6", None, None)],
    );
    form.catalog_returned("lab", Ok(fewer));
    assert_eq!(ids(&form), ["gpt-6"]);
    assert_eq!(selected(&form), 0);
    assert_eq!(
        form.context
            .catalog
            .iter()
            .filter(|e| e.connection == "lab")
            .count(),
        1
    );
}

#[test]
fn a_failed_read_keeps_the_rows_and_says_so() {
    let mut form = form();
    open_picker(&mut form, Role::Spotter);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    form.catalog_returned("lab", Err("reading lab's models: timed out".into()));
    assert_eq!(ids(&form), ["gpt-5.6", "gpt-5.6-mini"]);
    assert!(drawn(&form, 18).contains("lab: reading lab's models: timed out"));
}

#[test]
fn a_connection_with_nothing_yet_says_it_is_loading() {
    let mut context = context();
    let lab = context
        .catalog
        .iter_mut()
        .find(|e| e.connection == "lab")
        .unwrap();
    lab.status = CatalogStatus::Missing;
    lab.models.clear();
    let mut form = Form::new(context);
    open_picker(&mut form, Role::Spotter);
    assert_eq!(form.picker_rows()[1].trailer, "loading…");
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    assert!(drawn(&form, 18).contains("reading lab's models…"));
    // The answer lands while the operator is looking.
    let landed = entry(
        Provider::OpenAI,
        "lab",
        CatalogStatus::Fresh,
        vec![listed(Provider::OpenAI, "lab", "gpt-6", None, None)],
    );
    form.catalog_returned("lab", Ok(landed));
    assert_eq!(ids(&form), ["gpt-6"]);
}

#[test]
fn an_unavailable_connection_can_still_be_chipped() {
    let mut form = form();
    open_picker(&mut form, Role::Spotter);
    press(&mut form, KeyCode::Enter);
    assert_eq!(chips(&form), ["blank"]);
    assert!(drawn(&form, 18).contains("blank: no credential"));
    type_str(&mut form, "gpt-5.6");
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft
            .role(Role::Spotter)
            .map(|d| d.connection.as_str()),
        Some("blank")
    );
}

#[test]
fn a_form_with_no_host_behind_it_offers_nothing_but_says_so() {
    let mut form = Form::new(NewSessionContext::default());
    open_picker(&mut form, Role::Main);
    assert!(drawn(&form, 18).contains("no connections read from this host"));
    press(&mut form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Picker { .. }));
}
