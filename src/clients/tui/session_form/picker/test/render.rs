//! How the picker draws: the window, the input line, the facts.

use super::*;

#[test]
fn the_window_scrolls_and_reports_its_position() {
    let mut context = context();
    let lab = context
        .catalog
        .iter_mut()
        .find(|e| e.connection == "lab")
        .unwrap();
    lab.models = (0..30)
        .map(|n| {
            listed(
                Provider::OpenAI,
                "lab",
                &format!("model-{n:02}"),
                None,
                None,
            )
        })
        .collect();
    let mut form = Form::new(context);
    open_picker(&mut form, Role::Spotter);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    for _ in 0..29 {
        press(&mut form, KeyCode::Down);
    }
    let text = drawn(&form, 12);
    assert!(text.contains("❯ model-29"), "{text}");
    assert!(text.contains("  30/30"), "{text}");
    assert!(!text.contains("model-00"), "{text}");
}

#[test]
fn the_input_line_shows_chips_and_the_query() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    type_str(&mut form, "gl");
    let text = drawn(&form, 12);
    assert!(text.contains("? main model"), "{text}");
    assert!(text.contains("[router] [z-ai] gl▏"), "{text}");
}

#[test]
fn the_picker_fits_the_viewport_the_form_reserves() {
    let form = form();
    // Title, input, the rows the root needs for five recents and every
    // connection, the status line, and the key hint.
    assert!(usize::from(form.viewport_height()) >= 4 + 5 + form.connections().len());
}

#[test]
fn facts_are_formatted_for_a_row() {
    use super::super::picker_render::{age_label, context_label, price_label};
    assert_eq!(context_label(1_000_000), "1M");
    assert_eq!(context_label(1_048_576), "1M");
    assert_eq!(context_label(1_500_000), "1.5M");
    assert_eq!(context_label(272_000), "272k");
    assert_eq!(context_label(131_072), "131k");
    assert_eq!(context_label(512), "512");
    assert_eq!(
        price_label(crate::inference::api::Price {
            input: 0.15,
            output: 0.5
        }),
        "$0.15/$0.50"
    );
    assert_eq!(
        price_label(crate::inference::api::Price {
            input: 3.0,
            output: 15.0
        }),
        "$3.00/$15.00"
    );
    let now = 1_000_000_000;
    let day = 86_400;
    assert_eq!(age_label(now - 3 * day - 5, now), "3d");
    assert_eq!(age_label(now - 5 * 7 * day, now), "5w");
    assert_eq!(age_label(now - 8 * 30 * day - 1, now), "8mo");
    assert_eq!(age_label(now - 2 * 365 * day - 1, now), "2y");
    assert_eq!(age_label(now + 100, now), "0d");
}

#[test]
fn a_name_shows_only_when_it_adds_information() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    // "GLM 5.3 Flash" repeats `glm-5.3-flash`.
    assert!(form.picker_rows().iter().all(|row| row.name.is_empty()));
    press(&mut form, KeyCode::Backspace);
    press(&mut form, KeyCode::Backspace);
    type_str(&mut form, "router");
    press(&mut form, KeyCode::Enter);
    type_str(&mut form, "kimi-k2");
    let rows = form.picker_rows();
    assert_eq!(rows[0].id, "moonshotai/kimi-k2");
    assert_eq!(rows[0].name, "Kimi K2 Instruct");
}

#[test]
fn narrow_widths_drop_facts_then_the_name_before_the_id() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    let row = |width: u16| {
        let area = ratatui::layout::Rect::new(0, 0, width, 10);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        form.render(
            &mut buf,
            area,
            super::super::render::Styles::from(
                &crate::clients::tui::theme::InteractiveTheme::from_render_context(
                    &crate::clients::tui::render_context::RenderContext::new(),
                ),
            ),
        );
        (0..10)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .find(|line| line.contains("glm-5.3-flash"))
            .unwrap_or_default()
    };
    let wide = row(90);
    assert!(
        wide.contains("200k") && wide.contains("$0.15/$0.50") && wide.contains("3d"),
        "{wide}"
    );
    let tight = row(30);
    assert!(
        tight.contains("glm-5.3-flash") && !tight.contains("$0.15"),
        "{tight}"
    );
}

#[test]
fn a_refresh_action_reaches_the_host_from_either_container() {
    let (commands, mut received) = tokio::sync::mpsc::unbounded_channel();
    inline::dispatch(
        &commands,
        Action::RefreshCatalog {
            connection: "lab".into(),
            max_age: Some(60),
        },
    );
    assert!(matches!(
        received.try_recv(),
        Ok(crate::clients::tui::HostCmd::Catalog { connection, max_age: Some(60) }) if connection == "lab"
    ));
    // The full-screen container: a miss typed into the embedded picker.
    let mut play = Screenplay::new(100, 30);
    play.state.open_sessions();
    play.event(TuiEvent::Sessions(Ok(Vec::new())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    play.event(TuiEvent::NewSession(Ok(context())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    for c in "lab".chars() {
        play.route_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    for c in "gpt-9".chars() {
        play.route_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert!(matches!(
        play.took_host_cmd(),
        Some(crate::clients::tui::HostCmd::Catalog { connection, max_age: Some(60) }) if connection == "lab"
    ));
    // And its answer, delivered as an event, replaces the entry in the open form.
    let landed = entry(
        Provider::OpenAI,
        "lab",
        CatalogStatus::Fresh,
        vec![listed(
            Provider::OpenAI,
            "lab",
            "gpt-9",
            Some("GPT-9"),
            None,
        )],
    );
    play.event(TuiEvent::Catalog {
        connection: "lab".into(),
        result: Ok(landed),
    });
    let crate::clients::tui::state::UiMode::Sessions(menu) = &play.state.ui_mode else {
        panic!("the switcher is open")
    };
    let form = menu.form.as_ref().expect("the form is open");
    assert_eq!(ids(form)[0], "gpt-9");
}

#[test]
fn only_connections_worth_a_read_are_read_as_the_form_opens() {
    let mut entries = catalog();
    entries[1].status = CatalogStatus::Stale;
    entries[2].status = CatalogStatus::Missing;
    entries[3].status = CatalogStatus::Failed;
    assert_eq!(
        crate::clients::tui::host_dispatch::unfresh_connections(&entries),
        ["lab", "router", "work"]
    );
    assert!(crate::clients::tui::host_dispatch::unfresh_connections(&catalog()).is_empty());
}

#[test]
fn picker_snapshots() {
    let mut play = Screenplay::new(100, 30);
    play.state.open_sessions();
    play.event(TuiEvent::Sessions(Ok(Vec::new())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Char('n'), KeyModifiers::NONE);
    play.event(TuiEvent::NewSession(Ok(context())));
    while play.took_host_cmd().is_some() {}
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.snapshot("session_form_picker_root");
    for c in "glm".chars() {
        play.route_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    play.snapshot("session_form_picker_root_query");
    for _ in 0..3 {
        play.route_key(KeyCode::Backspace, KeyModifiers::NONE);
    }
    for c in "router".chars() {
        play.route_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.snapshot("session_form_picker_connection");
    for c in "z-ai".chars() {
        play.route_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    for c in "gl".chars() {
        play.route_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    play.snapshot("session_form_picker_group_query");
}

#[test]
fn tab_on_a_leaf_pushes_its_ancestors_and_keeps_the_alias_marker() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "kimi-lat");
    assert_eq!(ids(&form)[0], "router/~moonshotai/kimi-latest");
    press(&mut form, KeyCode::Tab);
    assert_eq!(chips(&form), ["router", "moonshotai"]);
    assert_eq!(query(&form), "~kimi-latest");
    // Under the connection chip, the group chip is pushed the same way.
    press(&mut form, KeyCode::Backspace);
    for _ in 0..12 {
        press(&mut form, KeyCode::Backspace);
    }
    assert_eq!(chips(&form), ["router"]);
    type_str(&mut form, "kimi-k2");
    press(&mut form, KeyCode::Tab);
    assert_eq!(chips(&form), ["router", "moonshotai"]);
    assert_eq!(query(&form), "kimi-k2");
}

#[test]
fn a_connection_view_caps_its_recents_before_the_groups() {
    let mut context = context();
    let router = context
        .catalog
        .iter_mut()
        .find(|e| e.connection == "router")
        .unwrap();
    router.models = (0..12)
        .map(|n| {
            listed(
                Provider::OpenRouter,
                "router",
                &format!("a/m{n:02}"),
                None,
                Some(n + 1),
            )
        })
        .collect();
    let mut form = Form::new(context);
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "router");
    press(&mut form, KeyCode::Enter);
    let rows = ids(&form);
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert_eq!(rows[5], "a");
}

#[test]
fn an_empty_result_says_nothing_matches_unless_there_are_no_connections() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "zzz");
    assert!(drawn(&form, 18).contains("nothing matches"));
    let mut bare = Form::new(NewSessionContext::default());
    open_picker(&mut bare, Role::Main);
    type_str(&mut bare, "zzz");
    assert!(drawn(&bare, 18).contains("no connections read from this host"));
}

#[test]
fn a_free_price_and_an_unset_date_read_plainly() {
    use super::super::picker_render::price_label;
    assert_eq!(
        price_label(crate::inference::api::Price {
            input: 0.0,
            output: 0.0
        }),
        "free"
    );
    let mut context = context();
    let lab = context
        .catalog
        .iter_mut()
        .find(|e| e.connection == "lab")
        .unwrap();
    lab.models[0].row.created = Some(0);
    let mut form = Form::new(context);
    open_picker(&mut form, Role::Main);
    let text = drawn(&form, 18);
    assert!(!text.contains("56y"), "{text}");
}
