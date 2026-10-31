//! The catalog, config, and key helpers every form test drives with.

use super::*;
use crate::api::{ListedModel, Price};
use crate::client::{CatalogModel, CatalogStatus, ConnectionCatalog};
use crate::inference::{ConnectionInventory, InferenceRegistry, ResolvedOptions, SessionOptions};
use crate::log::Provider;
use crate::user_config::InferenceConfig;
use ratatui::{buffer::Buffer, layout::Rect};

/// The host's configuration behind both the inventory and the preview resolver.
/// `blank` is disabled, which is how an unavailable connection reaches a client:
/// listed, not omitted.
const CONFIG: &str = r#"
[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "TEST_KEY" }

[connections.lab]
provider = "openai"
auth = { kind = "env", key = "TEST_KEY" }

[connections.router]
provider = "openrouter"
auth = { kind = "env", key = "TEST_KEY" }

[connections.blank]
provider = "openai"
auth = { kind = "env", key = "TEST_KEY" }
disabled = true
"#;

fn registry() -> InferenceRegistry {
    let config = InferenceConfig::from_toml(CONFIG).unwrap();
    let env = crate::env_table::EnvSnapshot::new(
        [("TEST_KEY".to_string(), "test-secret".to_string())].into(),
    )
    .unwrap();
    InferenceRegistry::from_config(
        config,
        std::path::Path::new("/host"),
        &env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
}

/// Connection rows built the way the host builds them — from configuration,
/// through the registry — so the form is exercised against real inventory
/// output rather than a hand-written shape of it. Sorted by name: `blank`,
/// `lab`, `router`, `work`.
pub(in crate::tui::session_form) fn connections() -> Vec<ConnectionInventory> {
    registry().inventory()
}

/// The host's answer about a draft, produced by the same resolver the control
/// route runs, so the form folds in a real `effective`/`issues` pair. History
/// seeds the omitted main branch the way a lived-in store would: one past pick
/// of `work/claude-sonnet-4-5` in this form's scope, so the untouched draft
/// resolves (as `History`) rather than refusing.
pub(in crate::tui::session_form) fn resolve(options: &SessionOptions) -> ResolvedOptions {
    let registry = registry();
    let dir = crate::testing::temp_dir("session-form");
    let store = crate::history::History::at(dir.path().join("state.db"));
    let scope = crate::history::Scope {
        cwd: PathBuf::from("/code/example"),
        config_key: crate::history::BUILTIN_CONFIG_KEY.to_string(),
    };
    store.record(&crate::history::Pick {
        at: crate::history::now(),
        cwd: "/code/example".to_string(),
        config_key: crate::history::BUILTIN_CONFIG_KEY.to_string(),
        connection: "work".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-5".to_string(),
    });
    registry.resolve_options(options, None, Some((&store, &scope)))
}

/// One listed model as the catalog route serves it: a group where the provider
/// derives one from the id, and whichever facts the test names.
pub(in crate::tui::session_form) fn listed(
    provider: Provider,
    connection: &str,
    model: &str,
    name: Option<&str>,
    recent: Option<u32>,
) -> CatalogModel {
    CatalogModel {
        connection: connection.to_string(),
        row: ListedModel {
            group: provider.group_of(model),
            name: name.map(str::to_string),
            ..ListedModel::new(model)
        },
        recent,
    }
}

pub(in crate::tui::session_form) fn entry(
    provider: Provider,
    connection: &str,
    status: CatalogStatus,
    models: Vec<CatalogModel>,
) -> ConnectionCatalog {
    ConnectionCatalog {
        connection: connection.to_string(),
        provider,
        status,
        age: None,
        error: None,
        models,
    }
}

/// What the host would hold after a day of use: OpenRouter's authors, two
/// OpenAI models and two Anthropic ones, three of them recently picked, and the
/// disabled connection unavailable.
pub(in crate::tui::session_form) fn catalog() -> Vec<ConnectionCatalog> {
    let now = crate::history::now();
    let days_ago = |days: i64| Some(now - days * 86_400 - 3_600);
    let mut router = vec![
        listed(
            Provider::OpenRouter,
            "router",
            "z-ai/glm-5.3-flash",
            Some("GLM 5.3 Flash"),
            None,
        ),
        listed(
            Provider::OpenRouter,
            "router",
            "z-ai/glm-5.3",
            Some("GLM 5.3"),
            Some(2),
        ),
        listed(
            Provider::OpenRouter,
            "router",
            "anthropic/claude-sonnet-4.5",
            Some("Claude Sonnet 4.5"),
            None,
        ),
        listed(
            Provider::OpenRouter,
            "router",
            "moonshotai/kimi-k2",
            Some("Kimi K2 Instruct"),
            None,
        ),
        listed(
            Provider::OpenRouter,
            "router",
            "~moonshotai/kimi-latest",
            Some("Kimi Latest"),
            None,
        ),
    ];
    router[0].row.context = Some(200_000);
    router[0].row.price = Some(Price {
        input: 0.15,
        output: 0.5,
    });
    router[0].row.created = days_ago(3);
    router[1].row.context = Some(1_000_000);
    router[1].row.created = days_ago(40);
    router[2].row.context = Some(1_000_000);
    router[2].row.price = Some(Price {
        input: 3.0,
        output: 15.0,
    });
    router[2].row.created = days_ago(250);
    let mut lab = vec![
        listed(Provider::OpenAI, "lab", "gpt-5.6", Some("GPT-5.6"), Some(1)),
        listed(
            Provider::OpenAI,
            "lab",
            "gpt-5.6-mini",
            Some("GPT-5.6 mini"),
            None,
        ),
    ];
    lab[0].row.context = Some(272_000);
    let work = vec![
        listed(
            Provider::Anthropic,
            "work",
            "claude-sonnet-4-5",
            Some("Claude Sonnet 4.5"),
            Some(3),
        ),
        listed(
            Provider::Anthropic,
            "work",
            "claude-haiku-4-5",
            Some("Claude Haiku 4.5"),
            None,
        ),
    ];
    let mut blank = entry(
        Provider::OpenAI,
        "blank",
        CatalogStatus::Unavailable,
        Vec::new(),
    );
    blank.error = Some("no credential".into());
    vec![
        blank,
        entry(Provider::OpenAI, "lab", CatalogStatus::Fresh, lab),
        entry(Provider::OpenRouter, "router", CatalogStatus::Fresh, router),
        entry(Provider::Anthropic, "work", CatalogStatus::Fresh, work),
    ]
}

pub(in crate::tui::session_form) fn context() -> NewSessionContext {
    NewSessionContext {
        cwd: PathBuf::from("/code/example"),
        connections: connections(),
        catalog: catalog(),
    }
}

pub(in crate::tui::session_form) fn form() -> Form {
    let mut form = Form::new(context());
    // Every container resolves the untouched draft as it opens, so a form under
    // test starts where a real one does.
    let opening = form.initial_preview();
    settle(&mut form, opening);
    form
}

/// Answer whatever the form asked for, the way a container's dispatcher would:
/// resolve the options it sent and hand the reply back, repeating while the
/// form supersedes its own request. A catalog refresh has no scripted answer
/// here; the tests that care read the action themselves.
pub(in crate::tui::session_form) fn settle(form: &mut Form, mut action: Action) {
    while let Action::Preview(options) = action {
        action = form.preview_returned(Ok(resolve(&options)));
    }
}

/// Type text into the open page, one key at a time.
pub(in crate::tui::session_form) fn type_str(form: &mut Form, text: &str) {
    for c in text.chars() {
        press(form, KeyCode::Char(c));
    }
}

/// Open a role's picker from the form page: `Main` is the second row, spotter
/// the fifth, triage the sixth.
pub(in crate::tui::session_form) fn open_picker(form: &mut Form, role: Role) {
    let row = match role {
        Role::Main => 1,
        Role::Spotter => 4,
        Role::Triage => 5,
    };
    form.selected = row;
    press(form, KeyCode::Enter);
    assert!(matches!(form.page, Page::Picker { .. }), "{:?}", form.page);
}

pub(in crate::tui::session_form) fn key(form: &mut Form, code: KeyCode) -> Action {
    form.handle(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The same press with the reply folded in, for the cases that care about the
/// form's resulting state rather than the request it emitted.
pub(in crate::tui::session_form) fn press(form: &mut Form, code: KeyCode) {
    let action = key(form, code);
    settle(form, action);
}

pub(in crate::tui::session_form) fn drawn(form: &Form, height: u16) -> String {
    let area = Rect::new(0, 0, 60, height);
    let mut buf = Buffer::empty(area);
    let theme = crate::tui::theme::InteractiveTheme::from_render_context(
        &crate::tui::render_context::RenderContext::new(),
    );
    form.render(&mut buf, area, render::Styles::from(&theme));
    (0..height)
        .map(|y| (0..60).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
