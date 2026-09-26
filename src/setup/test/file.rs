//! What reaches the config file: an existing file's other contents, default
//! selection, naming, cancellation, and the errors reading one raises.

use super::*;

/// Adding to an existing file: the name collision is refused, the standing
/// default is kept, comments survive, and the new table is appended.
#[tokio::test]
async fn adding_to_an_existing_file_keeps_it_intact() {
    let existing = r#"# my notes
[defaults]
model = "anthropic/claude-sonnet-4-5" # pinned

[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }
"#;
    let probe = Table {
        codex: Some("acct_1".into()),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("Add another connection"),
            Answer::Pick("ChatGPT subscription"),
            Answer::Type("anthropic"),
            Answer::Type("codex"),
            Answer::Pick("Piggyback on Codex's login"),
            Answer::No,
            Answer::Pick("Back"),
            Answer::Pick("Done"),
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("select: Connections"));
    assert!(script.said("\"anthropic\" is already a connection"));
    assert!(script.said("Found a Codex login for account acct_1"));
    assert_eq!(draft.decided_default(DefaultRole::Model), None);
    assert_eq!(
        render(existing, &draft).unwrap(),
        r#"# my notes
[defaults]
model = "anthropic/claude-sonnet-4-5" # pinned

[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }

[connections.codex]
provider = "openai-codex"
auth = { kind = "codex-cache" }
"#
    );
}

/// Two connections in one run: the default is a menu, and the file's
/// `[defaults]` follows the pick.
#[tokio::test]
async fn two_connections_choose_a_default() {
    let probe = Table {
        codex: Some("acct_1".into()),
        ..Default::default()
    };
    let (draft, _) = run_script(
        vec![
            Answer::Pick("Anthropic"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            // The variable is unset; decline the paste-for-the-check offer.
            Answer::No,
            Answer::Yes,
            Answer::Pick("ChatGPT subscription"),
            Answer::Type(""),
            Answer::Pick("Piggyback on Codex's login"),
            Answer::No,
            // Two connections, so the model question asks which one first.
            Answer::Pick("openai-codex"),
            Answer::Type("gpt-5.6"),
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(
        draft
            .decided_default(DefaultRole::Model)
            .flatten()
            .is_some_and(|model| model.starts_with("openai-codex/")),
        "{:?}",
        draft.decided_default(DefaultRole::Model)
    );
    let text = render("", &draft).unwrap();
    assert!(text.contains("model = \"openai-codex/gpt-5.6\""));
    // The first run asks no spotter question, so no key is written for it: the
    // role resolves to its default at session creation.
    assert!(!text.contains("spotter ="), "{text}");
    assert!(text.contains("[connections.anthropic]"));
    assert!(text.contains("[connections.openai-codex]"));
}

/// A second connection on a provider already named is suggested a numbered name.
#[tokio::test]
async fn a_second_connection_on_one_provider_gets_a_numbered_name() {
    let existing = r#"
[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "A" }
"#;
    let probe = Table::default();
    let (draft, _) = run_script(
        vec![
            Answer::Pick("Connections"),
            Answer::Pick("Add another connection"),
            Answer::Pick("Anthropic"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type("ANTHROPIC_PERSONAL_KEY"),
            Answer::No,
            Answer::No,
            Answer::Pick("Back"),
            Answer::Pick("Done"),
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(draft.connections[0].0, "anthropic-2");
    // Adding a connection says nothing about defaults: the standing one is left
    // exactly as it was, and the new connection does not become it.
    assert_eq!(draft.decided_default(DefaultRole::Model), None);
    let text = render(existing, &draft).unwrap();
    assert!(text.contains("[connections.anthropic-2]"), "{text}");
    assert!(!text.contains("[defaults]"), "{text}");
}

/// Backing out at any prompt is `Cancelled`, with the draft discarded.
#[tokio::test]
async fn cancel_at_any_prompt_writes_nothing() {
    let probe = Table::default();
    let (draft, _) = run_script(
        vec![
            Answer::Pick("Anthropic"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Cancel,
        ],
        &probe,
        "",
    )
    .await;
    assert_eq!(draft, Err(Cancelled));
}

#[test]
fn provider_errors_read_as_one_line() {
    let describe = super::flow::describe;
    assert_eq!(
        describe(
            r#"GET https://api.openai.com/v1/models returned 401 Unauthorized: {"error":{"message":"Incorrect API key","type":"invalid_request_error"}}"#
        ),
        "401 Unauthorized: Incorrect API key"
    );
    assert_eq!(
        describe(
            r#"GET https://api.anthropic.com/v1/models/x returned 404 Not Found: {"type":"error","error":{"type":"not_found_error","message":"model: x"}}"#
        ),
        "404 Not Found: model: x"
    );
    assert_eq!(
        describe("GET https://x failed: connection refused"),
        "GET https://x failed: connection refused"
    );
    assert_eq!(
        describe("model \"x\" is not in the OpenAI catalog\nknown: a, b"),
        "model \"x\" is not in the OpenAI catalog"
    );
}

/// A connection setup cannot read is named, not skipped. Silently omitting it
/// would hide the connection most likely to need repairing behind a list that
/// looks complete — and the auth kinds grow, so an older build meeting a newer
/// file has to say so (docs/operating.md).
#[test]
fn a_connection_setup_cannot_read_is_an_error_naming_it() {
    let unreadable =
        |toml: &str| Existing::from_toml(toml).expect_err("an unreadable connection is refused");

    let error = unreadable(
        r#"
[connections.future]
provider = "openai-codex"
auth = { kind = "vault" }
"#,
    );
    assert!(error.contains("connections.future"), "{error}");
    assert!(error.contains("vault"), "names the kind it met: {error}");
    assert!(
        error.contains("env") && error.contains("keychain"),
        "and the kinds it does understand: {error}"
    );

    for (toml, expected) in [
        (
            "[connections.a]\nauth = { kind = \"codex-cache\" }\n",
            "neither a `provider` nor a `dialect`",
        ),
        // A described endpoint is as repairable as a named one, so an unreadable
        // dialect says so rather than dropping the row from the list.
        (
            "[connections.a]\ndialect = \"completions\"\nbase-url = \"https://x/v1\"\nauth = { kind = \"env\", key = \"K\" }\n",
            "completions",
        ),
        (
            "[connections.a]\ndialect = 7\nauth = { kind = \"env\", key = \"K\" }\n",
            "non-string `dialect`",
        ),
        ("[connections.a]\nprovider = \"anthropic\"\n", "no `auth`"),
        (
            "[connections.a]\nprovider = \"anthropic\"\nauth = { kind = \"env\" }\n",
            "no `key`",
        ),
        (
            "[connections.a]\nprovider = \"anthropic\"\ndisabled = \"yes\"\nauth = { kind = \"codex-cache\" }\n",
            "non-boolean `disabled`",
        ),
    ] {
        let error = unreadable(toml);
        assert!(error.contains(expected), "expected {expected:?} in {error}");
        assert!(error.contains("connections.a"), "{error}");
    }
}
