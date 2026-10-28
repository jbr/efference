//! Adding a ChatGPT subscription connection by either route — Codex's cached
//! login, or a login performed here — and what a closed route states.

use super::*;

/// The subscription needs no key prompt, and piggybacking on Codex records the
/// kind that reads its cache.
#[tokio::test]
async fn piggybacking_on_codex_records_the_cache_kind() {
    let probe = Table {
        codex: Some("acct_1".into()),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("ChatGPT subscription"),
            Answer::Type(""),
            Answer::Pick("Piggyback on Codex's login"),
            Answer::No,
            Answer::Yes,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("Found a Codex login for account acct_1"));
    assert!(probe.logged_in.borrow().is_empty());
    assert_eq!(draft.connections[0].0, "openai-codex");
    assert_eq!(draft.connections[0].1.auth, AuthSource::CodexCache);
    let text = render("", &draft).unwrap();
    assert!(text.contains(r#"auth = { kind = "codex-cache" }"#));
    // A first run ends with no default model: the new-session form names the
    // current one.
    assert!(!text.contains("\n[defaults]"), "{text}");
}

/// Logging in here stores the grant and records the oauth kind; the file names
/// only the kind, and no Codex install was consulted.
#[tokio::test]
async fn logging_in_here_records_the_oauth_kind() {
    let probe = Table {
        login: Some(ChatGptLogin {
            account_id: "acct-7".into(),
            plan: Some("plus".into()),
        }),
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("ChatGPT subscription"),
            Answer::Type("chatgpt"),
            Answer::Pick("Log in here"),
            Answer::No,
            Answer::Yes,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(*probe.logged_in.borrow(), strings(&["chatgpt"]));
    assert!(script.said("Logged in as account acct-7, plus plan"));
    assert!(script.said("Kept as `oauth:chatgpt`"));
    assert_eq!(draft.connections[0].1.auth, AuthSource::Oauth);
    assert!(
        probe
            .secrets
            .as_ref()
            .unwrap()
            .get("oauth:chatgpt")
            .unwrap()
            .is_some()
    );
    let text = render("", &draft).unwrap();
    assert!(text.contains(r#"auth = { kind = "oauth" }"#), "{text}");
    let parsed = crate::inference::InferenceConfig::from_toml(&text);
    assert!(parsed.is_ok(), "{text}");
}

/// A login that does not finish is offered again, and declining the retry
/// leaves nothing recorded: a connection with no stored grant fails at launch.
#[tokio::test]
async fn a_failed_login_is_not_recorded() {
    let probe = Table {
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("ChatGPT subscription"),
            Answer::Type(""),
            Answer::Pick("Log in here"),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    assert_eq!(draft, Err(Cancelled));
    assert!(script.said("The login did not finish: the browser never came back"));
}

/// Each way of holding the login is shown with its own reason when it cannot
/// work here, rather than hidden: no Codex cache to read, or no store to keep
/// a grant in.
#[test]
fn each_chatgpt_source_states_why_it_is_closed() {
    struct Capture(Vec<Choice>);
    impl Prompter for Capture {
        fn say(&mut self, _: Tone, _: &str) {}
        fn select(&mut self, _: &str, choices: &[Choice]) -> Result<usize, Cancelled> {
            self.0 = choices.to_vec();
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let offered = |probe: &Table| {
        let mut capture = Capture(Vec::new());
        let result = runtime.block_on(super::flow::chatgpt_source(&mut capture, probe, "codex"));
        assert_eq!(result.unwrap_err(), Cancelled);
        let of = |label: &str| {
            capture
                .0
                .iter()
                .find(|choice| choice.label == label)
                .unwrap_or_else(|| panic!("no {label:?} among {:?}", capture.0))
                .unavailable
                .clone()
        };
        (of("Piggyback on Codex's login"), of("Log in here"))
    };

    // A store but no Codex: only the piggyback is closed.
    let (piggyback, log_in) = offered(&Table {
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    });
    assert_eq!(piggyback.as_deref(), Some("no auth.json"));
    assert_eq!(log_in, None);

    // Codex but no store: only the grant is closed.
    let (piggyback, log_in) = offered(&Table {
        codex: Some("acct_1".into()),
        ..Default::default()
    });
    assert_eq!(piggyback, None);
    assert_eq!(log_in.as_deref(), Some("no store on this platform"));
}

/// Neither way available is said with both reasons, since there is nothing to
/// pick: an empty menu would leave the person guessing.
#[test]
fn neither_chatgpt_source_available_says_both_reasons() {
    struct Silent(Vec<String>);
    impl Prompter for Silent {
        fn say(&mut self, _: Tone, text: &str) {
            self.0.push(text.to_string());
        }
        fn select(&mut self, question: &str, _: &[Choice]) -> Result<usize, Cancelled> {
            panic!("asked {question:?} with nothing to pick");
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
    let mut silent = Silent(Vec::new());
    let probe = Table::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let result = runtime.block_on(super::flow::chatgpt_source(&mut silent, &probe, "codex"));
    assert_eq!(result.unwrap_err(), Cancelled);
    let said = silent.0.join("\n");
    assert!(said.contains("no auth.json"), "{said}");
    assert!(said.contains("no store on this platform"), "{said}");
}
