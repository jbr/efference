//! The management flow over connections an earlier run recorded: readiness,
//! re-authorize, rename, disable, delete, move, reveal.

use super::*;

/// The management list states readiness from local checks only: an unset
/// variable, a store item present or absent, and a Codex cache that fails to
/// parse — never a network call (backlog/setup-edits-connections.md).
#[test]
fn management_list_states_readiness_locally() {
    /// Passes the top-level menu into Connections, then captures that list.
    #[derive(Default)]
    struct Capture {
        rows: Vec<Choice>,
        seen: usize,
    }
    impl Prompter for Capture {
        fn say(&mut self, _: Tone, _: &str) {}
        fn select(&mut self, _: &str, choices: &[Choice]) -> Result<usize, Cancelled> {
            self.seen += 1;
            if self.seen == 1 {
                return Ok(0);
            }
            self.rows = choices.to_vec();
            Err(Cancelled)
        }
        fn text(&mut self, _: &str, _: Option<&str>) -> Result<String, Cancelled> {
            Err(Cancelled)
        }
        fn secret(&mut self, _: &str) -> Result<String, Cancelled> {
            Err(Cancelled)
        }
        fn confirm(&mut self, _: &str, _: bool) -> Result<bool, Cancelled> {
            Err(Cancelled)
        }
    }
    let secrets = Secrets::in_memory();
    secrets.set("inference:store-ready", "sk-x").unwrap();
    let probe = Table {
        env: BTreeMap::from([("SET_KEY".to_string(), "value".to_string())]),
        secrets: Some(secrets),
        ..Default::default()
    };
    let existing = Existing::from_toml(
        r#"
[connections.env-ready]
provider = "anthropic"
auth = { kind = "env", key = "SET_KEY" }

[connections.env-missing]
provider = "anthropic"
auth = { kind = "env", key = "MISSING_KEY" }

[connections.store-ready]
provider = "anthropic"
auth = { kind = "keychain" }

[connections.cache-broken]
provider = "openai-codex"
auth = { kind = "codex-cache" }

[connections.grant-missing]
provider = "openai-codex"
auth = { kind = "oauth" }
"#,
    )
    .unwrap();
    let mut capture = Capture::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _ = runtime.block_on(run(&mut capture, &probe, &existing));
    let find = |name: &str| {
        capture
            .rows
            .iter()
            .find(|choice| choice.label.starts_with(name))
            .unwrap_or_else(|| panic!("no row for {name:?} among {:?}", capture.rows))
    };
    assert_eq!(find("env-ready").detail.as_deref(), Some("env · SET_KEY"));
    assert_eq!(
        find("env-missing").detail.as_deref(),
        Some("env · MISSING_KEY — not set")
    );
    assert_eq!(
        find("store-ready").detail.as_deref(),
        Some("keychain · inference:store-ready")
    );
    let cache = find("cache-broken").detail.clone().unwrap();
    assert!(cache.starts_with("cache · codex login — "), "{cache}");
    assert_eq!(
        find("grant-missing").detail.as_deref(),
        Some("oauth · oauth:grant-missing — no stored login")
    );
}

/// An oauth connection's edit menu offers the login again, not a move: there
/// is no grant value to export to a variable. Re-authorizing stores a fresh
/// grant under the same item and leaves the file alone.
#[tokio::test]
async fn re_authorizing_an_oauth_connection_keeps_the_file_unchanged() {
    let secrets = Secrets::in_memory();
    secrets.set("oauth:work", "{}").unwrap();
    let probe = Table {
        login: Some(ChatGptLogin {
            account_id: "acct-9".into(),
            plan: None,
        }),
        secrets: Some(secrets),
        ..Default::default()
    };
    let existing = r#"
[connections.work]
provider = "openai-codex"
auth = { kind = "oauth" }

[updates]
check = true
"#;
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("work (openai-codex)"),
            Answer::Pick("Edit"),
            Answer::Pick("Re-authorize"),
            Answer::Pick("Back"),
            Answer::Pick("Done"),
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(*probe.logged_in.borrow(), strings(&["work"]));
    assert!(script.said("Logged in as account acct-9."));
    assert_eq!(draft.edits, vec![]);
    assert_eq!(render(existing, &draft).unwrap(), existing);
}

/// Renaming a connection whose keychain item name was implicit moves the
/// store item: write the new one, confirm it reads back, then delete the old.
#[tokio::test]
async fn renaming_an_implicit_keychain_connection_moves_the_secret_item() {
    let secrets = Secrets::in_memory();
    secrets.set("inference:work", "sk-1").unwrap();
    let probe = Table {
        secrets: Some(secrets),
        ..Default::default()
    };
    let existing = r#"
[connections.work]
provider = "anthropic"
auth = { kind = "keychain" }
"#;
    let (draft, _) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("work (anthropic)"),
            Answer::Pick("Edit"),
            Answer::Pick("Rename"),
            Answer::Type("job"),
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(
        draft.edits,
        vec![Change::Rename {
            from: "work".into(),
            to: "job".into()
        }]
    );
    let text = render(existing, &draft).unwrap();
    assert!(text.contains("[connections.job]"), "{text}");
    assert!(!text.contains("[connections.work]"), "{text}");
    let secrets = probe.secrets.as_ref().unwrap();
    assert_eq!(
        secrets.get("inference:job").unwrap().as_deref(),
        Some("sk-1")
    );
    assert_eq!(secrets.get("inference:work").unwrap(), None);
}

/// Disabling the connection [defaults] names offers to repoint the spotter,
/// and the repointed file still loads.
#[tokio::test]
async fn disabling_a_connection_offers_to_repoint_dangling_defaults() {
    let probe = Table::default();
    let existing = r#"
[defaults]
spotter = "primary/haiku"

[connections.primary]
provider = "anthropic"
auth = { kind = "env", key = "PRIMARY_KEY" }

[connections.backup]
provider = "anthropic"
auth = { kind = "env", key = "BACKUP_KEY" }
"#;
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("primary (anthropic)"),
            Answer::Pick("Disable"),
            Answer::Yes,
            Answer::Pick("backup"),
            Answer::Type("haiku-fresh"),
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("[defaults].spotter names primary"));
    let text = render(existing, &draft).unwrap();
    assert!(text.contains(r#"spotter = "backup/haiku-fresh""#), "{text}");
    assert!(text.contains("disabled = true"), "{text}");
    let parsed = crate::config::user_config::InferenceConfig::from_toml(&text);
    assert!(parsed.is_ok(), "{text}");
}

/// Deleting a connection offers to repoint defaults and to delete its stored
/// secret; declining the secret leaves it in the store.
#[tokio::test]
async fn deleting_a_connection_offers_its_secret_and_leaves_it_if_declined() {
    let secrets = Secrets::in_memory();
    secrets.set("inference:work", "sk-1").unwrap();
    let probe = Table {
        secrets: Some(secrets),
        ..Default::default()
    };
    let existing = r#"
[connections.work]
provider = "anthropic"
auth = { kind = "keychain" }
"#;
    let (draft, _) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("work (anthropic)"),
            Answer::Pick("Delete"),
            Answer::Yes,
            Answer::No,
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(
        draft.edits,
        vec![Change::Delete {
            name: "work".into()
        }]
    );
    let text = render(existing, &draft).unwrap();
    assert!(!text.contains("[connections.work]"), "{text}");
    assert_eq!(
        probe
            .secrets
            .as_ref()
            .unwrap()
            .get("inference:work")
            .unwrap()
            .as_deref(),
        Some("sk-1")
    );
}

/// A scheme-shaped variable value (an `[env]` indirection a resolver expands
/// at read time, e.g. `op://…`) is offered as unavailable to move: storing
/// what the variable literally says would put the reference in the keychain,
/// where nothing expands it.
#[test]
fn moving_an_env_value_refuses_a_scheme_reference() {
    struct Capture {
        captured: Vec<Choice>,
    }
    impl Prompter for Capture {
        fn say(&mut self, _: Tone, _: &str) {}
        fn select(&mut self, question: &str, choices: &[Choice]) -> Result<usize, Cancelled> {
            match question {
                "Setup" => Ok(0),
                "Connections" => Ok(choices
                    .iter()
                    .position(|choice| choice.label.starts_with("work"))
                    .unwrap()),
                "work" => Ok(choices
                    .iter()
                    .position(|choice| choice.label == "Edit")
                    .unwrap()),
                "Edit work" => {
                    self.captured = choices.to_vec();
                    Err(Cancelled)
                }
                other => panic!("unexpected select: {other:?}"),
            }
        }
        fn text(&mut self, _: &str, _: Option<&str>) -> Result<String, Cancelled> {
            Err(Cancelled)
        }
        fn secret(&mut self, _: &str) -> Result<String, Cancelled> {
            Err(Cancelled)
        }
        fn confirm(&mut self, _: &str, _: bool) -> Result<bool, Cancelled> {
            Err(Cancelled)
        }
    }
    let probe = Table {
        env: BTreeMap::from([("OP_KEY".to_string(), "op://vault/item/field".to_string())]),
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    };
    let existing = Existing::from_toml(
        r#"
[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "OP_KEY" }
"#,
    )
    .unwrap();
    let mut capture = Capture {
        captured: Vec::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let result = runtime.block_on(run(&mut capture, &probe, &existing));
    assert_eq!(result.unwrap_err(), Cancelled);
    let move_choice = capture
        .captured
        .iter()
        .find(|choice| choice.label.contains("keychain"))
        .unwrap();
    let why = move_choice.unavailable.as_deref().unwrap();
    assert!(why.contains("op://vault/item/field"), "{why}");
}

/// A plain variable value moves straight into the keychain: no paste, and the
/// file's auth kind switches to keychain.
#[tokio::test]
async fn moving_a_plain_env_value_into_the_keychain() {
    let probe = Table {
        env: BTreeMap::from([("PLAIN_KEY".to_string(), "sk-live".to_string())]),
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    };
    let existing = r#"
[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "PLAIN_KEY" }
"#;
    let (draft, _) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("work (anthropic)"),
            Answer::Pick("Edit"),
            Answer::Pick("Move the key into the keychain"),
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(
        draft.edits,
        vec![Change::SetAuth {
            name: "work".into(),
            auth: AuthSource::Keychain,
        }]
    );
    let text = render(existing, &draft).unwrap();
    assert!(text.contains(r#"auth = { kind = "keychain" }"#), "{text}");
    assert_eq!(
        probe
            .secrets
            .as_ref()
            .unwrap()
            .get("inference:work")
            .unwrap()
            .as_deref(),
        Some("sk-live")
    );
}

/// The reveal path asks the same question through the same check: a key typed
/// as the variable name is refused, and what was typed is not repeated.
#[tokio::test]
async fn reveal_refuses_a_key_typed_as_the_variable_name() {
    let secrets = Secrets::in_memory();
    secrets.set("inference:work", "sk-1").unwrap();
    let probe = Table {
        secrets: Some(secrets),
        ..Default::default()
    };
    let existing = r#"
[connections.work]
provider = "anthropic"
auth = { kind = "keychain" }
"#;
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("work (anthropic)"),
            Answer::Pick("Edit"),
            Answer::Pick("Reveal the key and switch to an environment variable"),
            Answer::Type("sk-typed-by-mistake"),
            Answer::Type("WORK_KEY"),
            Answer::No,
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("Bad: That looks like a key, not a variable name"));
    assert!(
        !script.said("sk-typed-by-mistake"),
        "{:?}",
        script.transcript
    );
    assert_eq!(
        draft.edits,
        vec![Change::SetAuth {
            name: "work".into(),
            auth: AuthSource::Env {
                key: "WORK_KEY".into()
            },
        }]
    );
}

/// The reverse move is reveal-then-switch, never a silent delete: the stored
/// key is shown, the config switches to a variable, and the store item is
/// left unless the operator separately asks to clear it.
#[tokio::test]
async fn revealing_a_keychain_secret_switches_to_env_and_can_clear_it() {
    let secrets = Secrets::in_memory();
    secrets.set("inference:work", "sk-1").unwrap();
    let probe = Table {
        secrets: Some(secrets),
        ..Default::default()
    };
    let existing = r#"
[connections.work]
provider = "anthropic"
auth = { kind = "keychain" }
"#;
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("work (anthropic)"),
            Answer::Pick("Edit"),
            Answer::Pick("Reveal the key and switch to an environment variable"),
            Answer::Type("WORK_KEY"),
            Answer::Yes,
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("inference:work: sk-1"));
    assert_eq!(
        draft.edits,
        vec![Change::SetAuth {
            name: "work".into(),
            auth: AuthSource::Env {
                key: "WORK_KEY".into()
            },
        }]
    );
    let text = render(existing, &draft).unwrap();
    assert!(
        text.contains(r#"auth = { kind = "env", key = "WORK_KEY" }"#),
        "{text}"
    );
    assert_eq!(
        probe
            .secrets
            .as_ref()
            .unwrap()
            .get("inference:work")
            .unwrap(),
        None
    );
}

/// A connection added in this same run is not in the file yet, so a rename of
/// it is applied to the pending draft rather than queued as an edit: queueing
/// one would fail the whole write at render and lose the run.
#[tokio::test]
async fn renaming_a_connection_added_in_the_same_run_still_writes_it() {
    let secrets = Secrets::in_memory();
    let probe = Table {
        login: Some(ChatGptLogin {
            account_id: "acct-3".into(),
            plan: Some("plus".into()),
        }),
        secrets: Some(secrets),
        ..Default::default()
    };
    let existing = r#"
[connections.other]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }
"#;
    let (draft, _script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("Add another connection"),
            Answer::Pick("ChatGPT subscription"),
            Answer::Type("openai-codex"),
            Answer::Pick("Log in here"),
            Answer::No,
            Answer::Pick("openai-codex (openai-codex)"),
            Answer::Pick("Edit"),
            Answer::Pick("Rename"),
            Answer::Type("codex"),
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(draft.edits, vec![]);
    assert_eq!(draft.connections[0].0, "codex");
    let text = render(existing, &draft).unwrap();
    assert!(text.contains("[connections.codex]"), "{text}");
    assert!(!text.contains("openai-codex]"), "{text}");
    let parsed = crate::config::user_config::InferenceConfig::from_toml(&text);
    assert!(parsed.is_ok(), "{text}");
    let secrets = probe.secrets.as_ref().unwrap();
    assert!(secrets.get("oauth:codex").unwrap().is_some());
    assert!(secrets.get("oauth:openai-codex").unwrap().is_none());
}

/// Deleting a connection added in the same run drops it from the draft, so
/// nothing is queued against a table the file never had.
#[tokio::test]
async fn deleting_a_connection_added_in_the_same_run_leaves_the_file_alone() {
    let probe = Table {
        codex: Some("acct_1".into()),
        ..Default::default()
    };
    // A file that has answered the update question is not asked it again.
    let existing = r#"
[defaults]
spotter = "other/claude-haiku-4-5"

[updates]
check = false

[connections.other]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }
"#;
    let (draft, _script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("Add another connection"),
            Answer::Pick("ChatGPT subscription"),
            Answer::Type("codex"),
            Answer::Pick("Piggyback on Codex's login"),
            Answer::No,
            Answer::Pick("codex (openai-codex)"),
            Answer::Pick("Delete"),
            Answer::Yes,
            Answer::Pick("Back"),
            Answer::Pick("Done"),
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(draft.edits, vec![]);
    assert!(draft.connections.is_empty());
    assert_eq!(render(existing, &draft).unwrap(), existing);
}

/// Delete a connection, add a replacement under a different name, then rename
/// the addition to the deleted one's name. The name is free by then because
/// `Working` dropped it, and the file ends up with one table under it because
/// `render` applies every edit before it appends what the run added.
#[tokio::test]
async fn a_deleted_name_can_be_reused_by_a_connection_added_in_the_same_run() {
    let probe = Table {
        login: Some(ChatGptLogin {
            account_id: "acct-3".into(),
            plan: Some("plus".into()),
        }),
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    };
    let existing = r#"
[defaults]
spotter = "other/claude-haiku-4-5"

[connections.other]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }

[connections.codex]
provider = "openai-codex"
auth = { kind = "codex-cache" }
"#;
    let (draft, _script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("codex (openai-codex)"),
            Answer::Pick("Delete"),
            Answer::Yes,
            Answer::Pick("Add another connection"),
            Answer::Pick("ChatGPT subscription"),
            Answer::Type("openai-codex"),
            Answer::Pick("Log in here"),
            Answer::No,
            Answer::Pick("openai-codex (openai-codex)"),
            Answer::Pick("Edit"),
            Answer::Pick("Rename"),
            Answer::Type("codex"),
            Answer::Pick("Back"),
            Answer::Pick("Done"),
            Answer::Yes,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(
        draft.edits,
        vec![Change::Delete {
            name: "codex".to_string()
        }]
    );
    assert_eq!(draft.connections[0].0, "codex");
    let text = render(existing, &draft).unwrap();
    assert_eq!(text.matches("[connections.codex]").count(), 1, "{text}");
    assert!(!text.contains(r#"kind = "codex-cache""#), "{text}");
    assert!(text.contains(r#"auth = { kind = "oauth" }"#), "{text}");
    let parsed = crate::config::user_config::InferenceConfig::from_toml(&text);
    assert!(parsed.is_ok(), "{text}");
}
