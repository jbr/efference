use super::super::test::{
    catalog, context, drawn, entry, form, key, listed, open_picker, press, settle, type_str,
};
use super::super::*;
use crate::api::Provider;
use crate::client::CatalogStatus;
use crate::tui::{TuiEvent, harness::Screenplay};

/// The ids of the rows on screen, in order.
fn ids(form: &Form) -> Vec<String> {
    form.picker_rows().into_iter().map(|row| row.id).collect()
}

fn chips(form: &Form) -> Vec<String> {
    match &form.page {
        Page::Picker { path, .. } => path.iter().map(|c| c.label().to_string()).collect(),
        other => panic!("expected the picker, found {other:?}"),
    }
}

fn query(form: &Form) -> String {
    match &form.page {
        Page::Picker { query, .. } => query.text(),
        other => panic!("expected the picker, found {other:?}"),
    }
}

fn selected(form: &Form) -> usize {
    match &form.page {
        Page::Picker { selected, .. } => *selected,
        other => panic!("expected the picker, found {other:?}"),
    }
}

/// Chip `router` and then its `z-ai` author, the way a keyboard would.
fn into_zai(form: &mut Form) {
    type_str(form, "router");
    press(form, KeyCode::Enter);
    type_str(form, "z-ai");
    press(form, KeyCode::Enter);
    assert_eq!(chips(form), ["router", "z-ai"]);
}

#[test]
fn the_root_shows_recents_then_every_connection_for_main() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    assert_eq!(
        ids(&form),
        [
            "lab/gpt-5.6",
            "router/z-ai/glm-5.3",
            "work/claude-sonnet-4-5",
            "blank",
            "lab",
            "router",
            "work"
        ]
    );
    let rows = form.picker_rows();
    assert_eq!(rows[0].trailer, "recent");
    assert_eq!(rows[3].trailer, "unavailable");
    assert!(rows[3].dim);
    assert_eq!(rows[5].name, "openrouter");
}

#[test]
fn the_root_for_spotter_and_triage_is_the_connections_alone() {
    for role in [Role::Spotter, Role::Triage] {
        let mut form = form();
        open_picker(&mut form, role);
        assert_eq!(ids(&form), ["blank", "lab", "router", "work"], "{role:?}");
    }
}

#[test]
fn the_root_lists_at_most_five_recents() {
    let mut context = context();
    let router = context
        .catalog
        .iter_mut()
        .find(|e| e.connection == "router")
        .unwrap();
    for (rank, model) in router.models.iter_mut().enumerate() {
        model.recent = Some(10 + u32::try_from(rank).unwrap());
    }
    let mut form = Form::new(context);
    open_picker(&mut form, Role::Main);
    let recents = form
        .picker_rows()
        .iter()
        .filter(|row| row.trailer == "recent")
        .count();
    assert_eq!(recents, 5);
}

#[test]
fn a_query_at_the_root_finds_a_model_across_connections() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "glm");
    assert_eq!(
        ids(&form),
        ["router/z-ai/glm-5.3", "router/z-ai/glm-5.3-flash"]
    );
}

#[test]
fn a_root_query_also_offers_groups_named_with_their_connection() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "moonshot");
    let rows = ids(&form);
    assert_eq!(rows[0], "router/moonshotai");
    // Its models are not found through the group's name.
    assert_eq!(rows.len(), 1, "{rows:?}");
}

#[test]
fn tab_and_slash_complete_the_highlighted_row_into_a_chip() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "rou");
    assert_eq!(ids(&form)[0], "router");
    press(&mut form, KeyCode::Tab);
    assert_eq!(chips(&form), ["router"]);
    assert_eq!(query(&form), "");
    assert_eq!(selected(&form), 0);
    // Back out and take the same chip with `/`.
    press(&mut form, KeyCode::Backspace);
    type_str(&mut form, "rou/");
    assert_eq!(chips(&form), ["router"]);
    assert_eq!(query(&form), "");
    // The next segment completes the same way.
    type_str(&mut form, "z-");
    press(&mut form, KeyCode::Char('/'));
    assert_eq!(chips(&form), ["router", "z-ai"]);
}

#[test]
fn a_group_reached_from_the_root_brings_its_connection() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "router/moonshot");
    // `router/` chipped the connection; `moonshot` now highlights its group.
    assert_eq!(chips(&form), ["router"]);
    press(&mut form, KeyCode::Tab);
    assert_eq!(chips(&form), ["router", "moonshotai"]);
    assert_eq!(ids(&form), ["kimi-k2", "~kimi-latest"]);
}

#[test]
fn slash_is_a_character_where_nothing_interior_is_highlighted() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "glm/");
    assert_eq!(query(&form), "glm/");
    assert!(chips(&form).is_empty());
}

#[test]
fn a_connection_shows_recents_then_its_groups() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "router");
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        ids(&form),
        ["z-ai/glm-5.3", "anthropic", "moonshotai", "z-ai"]
    );
    let rows = form.picker_rows();
    assert_eq!(rows[1].trailer, "1 model");
    assert_eq!(rows[2].trailer, "2 models");
    assert_eq!(rows[3].trailer, "2 models");
}

#[test]
fn a_connection_without_groups_lists_its_models_with_recents_first() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "work");
    press(&mut form, KeyCode::Enter);
    assert_eq!(ids(&form), ["claude-sonnet-4-5", "claude-haiku-4-5"]);
    // Spotter ignores recent ranks: the catalog's order stands, and the
    // trailer stays empty.
    let mut spotter = self::form();
    open_picker(&mut spotter, Role::Spotter);
    type_str(&mut spotter, "lab");
    press(&mut spotter, KeyCode::Enter);
    assert_eq!(ids(&spotter), ["gpt-5.6", "gpt-5.6-mini"]);
    assert!(
        spotter
            .picker_rows()
            .iter()
            .all(|row| row.trailer.is_empty())
    );
    type_str(&mut spotter, "mini");
    assert_eq!(ids(&spotter)[0], "gpt-5.6-mini");
}

#[test]
fn a_group_chip_lists_its_models_without_the_author() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    assert_eq!(ids(&form), ["glm-5.3", "glm-5.3-flash"]);
    type_str(&mut form, "fl");
    assert_eq!(ids(&form)[0], "glm-5.3-flash");
}

#[test]
fn four_backspaces_walk_from_the_query_back_to_the_root() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    type_str(&mut form, "gl");
    press(&mut form, KeyCode::Backspace);
    press(&mut form, KeyCode::Backspace);
    assert_eq!(query(&form), "");
    assert_eq!(chips(&form), ["router", "z-ai"]);
    press(&mut form, KeyCode::Backspace);
    assert_eq!(chips(&form), ["router"]);
    press(&mut form, KeyCode::Backspace);
    assert!(chips(&form).is_empty());
    // At the root with nothing typed, backspace has nothing left to take.
    press(&mut form, KeyCode::Backspace);
    assert!(chips(&form).is_empty());
    assert!(matches!(form.page, Page::Picker { .. }));
}

#[test]
fn a_typed_id_is_the_last_row_under_a_connection_chip() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    type_str(&mut form, "gpt-5");
    let rows = form.picker_rows();
    assert_eq!(rows.last().unwrap().id, "use `gpt-5` as model id");
    assert!(rows.len() > 1, "{:?}", ids(&form));
    // An id the listing already has is offered once, as its listed row.
    type_str(&mut form, ".6");
    assert!(
        form.picker_rows()
            .iter()
            .all(|row| !matches!(row.target, picker_rows::Target::Typed { .. })),
        "{:?}",
        ids(&form)
    );
    // With whitespace it is a two-token search, not an id.
    type_str(&mut form, " ");
    assert!(
        form.picker_rows()
            .iter()
            .all(|row| !matches!(row.target, picker_rows::Target::Typed { .. }))
    );
    // The root has no connection to attach an id to.
    let mut root = self::form();
    open_picker(&mut root, Role::Main);
    type_str(&mut root, "zzz");
    assert!(root.picker_rows().is_empty());
}

#[test]
fn the_typed_id_is_the_only_row_when_nothing_matches() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "lab");
    press(&mut form, KeyCode::Enter);
    type_str(&mut form, "gpt-9");
    assert_eq!(ids(&form), ["use `gpt-9` as model id"]);
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.role(Role::Main),
        Some(&RoleDraft {
            connection: "lab".into(),
            model: "gpt-9".into()
        })
    );
}

#[test]
fn under_a_group_chip_a_bare_typed_id_carries_the_author() {
    let mut form = form();
    open_picker(&mut form, Role::Main);
    into_zai(&mut form);
    type_str(&mut form, "glm-9");
    assert_eq!(ids(&form), ["use `z-ai/glm-9` as model id"]);
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.role(Role::Main).map(|d| d.model.as_str()),
        Some("z-ai/glm-9")
    );
}

mod actions;
mod render;
