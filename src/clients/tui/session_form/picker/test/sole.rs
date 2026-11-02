//! The root when the role can choose from exactly one connection: it lists that
//! connection's own level (docs/tui.md, *The one-connection root*).

use super::*;

/// A form whose host serves only the named connections.
fn serving(names: &[&str]) -> Form {
    let mut context = context();
    context
        .connections
        .retain(|c| names.contains(&c.name.as_str()));
    context
        .catalog
        .retain(|e| names.contains(&e.connection.as_str()));
    let mut form = Form::new(context);
    let opening = form.initial_preview();
    settle(&mut form, opening);
    form
}

fn into_path(form: &mut Form, taken: &[&str]) {
    let Page::Picker { path, .. } = &mut form.page else {
        panic!("expected the picker");
    };
    *path = match taken {
        [connection] => vec![Chip::Connection((*connection).into())],
        [connection, group] => vec![
            Chip::Connection((*connection).into()),
            Chip::Group((*group).into()),
        ],
        _ => Vec::new(),
    };
}

#[test]
fn the_only_connections_root_lists_what_its_chip_lists() {
    for role in [Role::Main, Role::Spotter] {
        let mut form = serving(&["router"]);
        open_picker(&mut form, role);
        let at_root = form.picker_rows();
        assert!(chips(&form).is_empty());
        into_path(&mut form, &["router"]);
        assert_eq!(at_root, form.picker_rows(), "{role:?}");
    }
    let mut form = serving(&["router"]);
    open_picker(&mut form, Role::Main);
    // Recents lead, then the groups; no connection row.
    assert_eq!(
        ids(&form),
        ["z-ai/glm-5.3", "anthropic", "moonshotai", "z-ai"]
    );
    assert_eq!(form.picker_rows()[0].trailer, "recent");

    let mut ungrouped = serving(&["work"]);
    open_picker(&mut ungrouped, Role::Main);
    assert_eq!(ids(&ungrouped), ["claude-sonnet-4-5", "claude-haiku-4-5"]);
}

#[test]
fn two_connections_keep_the_connection_rows_at_the_root() {
    let mut form = serving(&["router", "lab"]);
    open_picker(&mut form, Role::Spotter);
    assert_eq!(ids(&form), ["lab", "router"]);
}

#[test]
fn an_unavailable_only_connection_keeps_its_row() {
    let mut form = serving(&["blank"]);
    open_picker(&mut form, Role::Main);
    assert_eq!(ids(&form), ["blank"]);
    let rows = form.picker_rows();
    assert_eq!(rows[0].trailer, "unavailable");
    assert!(rows[0].dim);
}

#[test]
fn a_group_row_at_the_only_connections_root_descends_into_it() {
    let mut form = serving(&["router"]);
    open_picker(&mut form, Role::Main);
    press(&mut form, KeyCode::Down);
    assert_eq!(ids(&form)[1], "anthropic");
    press(&mut form, KeyCode::Enter);
    assert_eq!(chips(&form), ["router", "anthropic"]);
    assert_eq!(ids(&form), ["claude-sonnet-4.5"]);
    // Backspace walks out to the root, which is again the connection's level.
    press(&mut form, KeyCode::Backspace);
    assert_eq!(chips(&form), ["router"]);
    press(&mut form, KeyCode::Backspace);
    assert!(chips(&form).is_empty());
    assert_eq!(ids(&form)[1], "anthropic");
}

#[test]
fn a_model_row_at_the_only_connections_root_picks_it() {
    let mut form = serving(&["router"]);
    open_picker(&mut form, Role::Main);
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.role(Role::Main),
        Some(&RoleDraft {
            connection: "router".into(),
            model: "z-ai/glm-5.3".into()
        })
    );
}

#[test]
fn the_typed_id_at_the_only_connections_root_targets_it() {
    let mut form = serving(&["router"]);
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "glm-9");
    assert!(chips(&form).is_empty());
    assert_eq!(ids(&form), ["use `glm-9` as model id"]);
    press(&mut form, KeyCode::Enter);
    assert_eq!(
        form.draft.role(Role::Main),
        Some(&RoleDraft {
            connection: "router".into(),
            model: "glm-9".into()
        })
    );
    // With two connections the root has nothing to attach an id to.
    let mut two = serving(&["router", "lab"]);
    open_picker(&mut two, Role::Main);
    type_str(&mut two, "zzz");
    assert!(two.picker_rows().is_empty());
}

#[test]
fn search_at_the_only_connections_root_is_the_roots_search() {
    let mut form = serving(&["router"]);
    open_picker(&mut form, Role::Main);
    type_str(&mut form, "glm");
    assert_eq!(
        &ids(&form)[..2],
        ["router/z-ai/glm-5.3", "router/z-ai/glm-5.3-flash"]
    );
}

#[test]
fn an_empty_only_connection_says_to_type_an_id() {
    let mut context = context();
    context.connections.retain(|c| c.name == "lab");
    context.catalog.retain(|e| e.connection == "lab");
    context.catalog[0].models.clear();
    let mut form = Form::new(context);
    open_picker(&mut form, Role::Main);
    assert!(form.picker_rows().is_empty());
    assert_eq!(
        form.picker_placeholder(),
        "no models listed — type a model id"
    );
}
