use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

use super::*;

/// One scripted answer. The script is consumed in order; a prompt with no
/// answer left panics naming the question, which is the test's failure.
#[derive(Debug)]
enum Answer {
    Pick(&'static str),
    Type(&'static str),
    Secret(&'static str),
    Yes,
    No,
    Cancel,
}

struct Script {
    answers: VecDeque<Answer>,
    /// Every prompt asked and every line said, in order, for assertions.
    transcript: Vec<String>,
}

impl Script {
    fn new(answers: Vec<Answer>) -> Self {
        Self {
            answers: answers.into(),
            transcript: Vec::new(),
        }
    }

    fn next(&mut self, question: &str) -> Answer {
        self.answers
            .pop_front()
            .unwrap_or_else(|| panic!("no scripted answer for {question:?}"))
    }

    fn said(&self, needle: &str) -> bool {
        self.transcript.iter().any(|line| line.contains(needle))
    }
}

impl Prompter for Script {
    fn say(&mut self, tone: Tone, text: &str) {
        self.transcript.push(format!("{tone:?}: {text}"));
    }

    fn select(&mut self, question: &str, choices: &[Choice]) -> Result<usize, Cancelled> {
        self.transcript.push(format!("select: {question}"));
        match self.next(question) {
            Answer::Pick(label) => {
                let index = choices
                    .iter()
                    .position(|choice| choice.label == label)
                    .unwrap_or_else(|| panic!("no choice {label:?} for {question:?}"));
                assert!(
                    choices[index].unavailable.is_none(),
                    "{label:?} is unavailable"
                );
                Ok(index)
            }
            Answer::Cancel => Err(Cancelled),
            other => panic!("select {question:?} got {other:?}"),
        }
    }

    fn text(&mut self, question: &str, default: Option<&str>) -> Result<String, Cancelled> {
        self.transcript.push(format!("text: {question}"));
        match self.next(question) {
            Answer::Type("") => Ok(default.unwrap_or("").to_string()),
            Answer::Type(text) => Ok(text.to_string()),
            Answer::Cancel => Err(Cancelled),
            other => panic!("text {question:?} got {other:?}"),
        }
    }

    fn secret(&mut self, question: &str) -> Result<String, Cancelled> {
        self.transcript.push(format!("secret: {question}"));
        match self.next(question) {
            Answer::Secret(text) => Ok(text.to_string()),
            Answer::Cancel => Err(Cancelled),
            other => panic!("secret {question:?} got {other:?}"),
        }
    }

    fn confirm(&mut self, question: &str, _default: bool) -> Result<bool, Cancelled> {
        self.transcript.push(format!("confirm: {question}"));
        match self.next(question) {
            Answer::Yes => Ok(true),
            Answer::No => Ok(false),
            Answer::Cancel => Err(Cancelled),
            other => panic!("confirm {question:?} got {other:?}"),
        }
    }
}

/// A world with a fixed environment, one accepted key per provider, a fixed
/// Anthropic listing, and a set of known models. Records which keys it saw.
#[derive(Default)]
struct Table {
    env: BTreeMap<String, String>,
    accepted_keys: Vec<String>,
    known_models: Vec<String>,
    codex: Option<String>,
    seen_keys: RefCell<Vec<String>>,
}

impl Probe for Table {
    fn env_var(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn codex_cache(&self) -> Result<String, String> {
        self.codex.clone().ok_or("no auth.json".to_string())
    }

    async fn check_key(&self, _: Provider, key: &str) -> Result<Option<KeyReport>, String> {
        self.seen_keys.borrow_mut().push(key.to_string());
        if self.accepted_keys.iter().any(|accepted| accepted == key) {
            Ok(Some(KeyReport {
                label: Some("test key".into()),
            }))
        } else {
            Err("GET returned 401: {\"error\":\"bad key\"}".into())
        }
    }

    async fn list_models(&self, provider: Provider, _: &str) -> Result<Vec<ModelListing>, String> {
        if provider != Provider::Anthropic {
            return Ok(Vec::new());
        }
        Ok(vec![
            ModelListing {
                id: "claude-sonnet-4-5".into(),
                display_name: "Claude Sonnet 4.5".into(),
            },
            ModelListing {
                id: "claude-haiku-4-5".into(),
                display_name: "Claude Haiku 4.5".into(),
            },
        ])
    }

    async fn check_model(&self, _: Provider, _: &str, model: &str) -> Result<ModelReport, String> {
        if self.known_models.iter().any(|known| known == model) {
            Ok(ModelReport {
                context_length: 200_000,
                max_output_tokens: Some(64_000),
            })
        } else {
            Err(format!("GET returned 404: no model {model}"))
        }
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

async fn run_script(
    answers: Vec<Answer>,
    probe: &Table,
    existing: &str,
) -> (Result<Draft, Cancelled>, Script) {
    let mut script = Script::new(answers);
    let existing = Existing::from_toml(existing).unwrap();
    let draft = run(&mut script, probe, &existing).await;
    assert!(
        script.answers.is_empty(),
        "unused answers: {:?}",
        script.answers
    );
    (draft, script)
}

/// The first-run path with a key already in the shell: no secret prompt, a
/// live model menu, and a file whose bytes are pinned.
#[tokio::test]
async fn first_run_with_key_in_environment() {
    let probe = Table {
        env: BTreeMap::from([("ANTHROPIC_API_KEY".to_string(), "sk-real".to_string())]),
        accepted_keys: strings(&["sk-real"]),
        known_models: strings(&["claude-sonnet-4-5", "claude-haiku-4-5"]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Anthropic"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::Pick("claude-sonnet-4-5"),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("ANTHROPIC_API_KEY is set in this shell"));
    assert!(script.said("accepted the key (test key)"));
    assert!(script.said("claude-sonnet-4-5: 200k context, 64k output"));
    assert!(
        !script
            .transcript
            .iter()
            .any(|line| line.starts_with("secret:"))
    );
    assert_eq!(draft.default_connection.as_deref(), Some("anthropic"));
    assert_eq!(
        render("", &draft).unwrap(),
        r#"# efference host configuration, written by `efference setup`.
# Connections name a provider and where its credential comes from; the
# credential itself is never in this file. Sessions select a model as
# <connection>/<native model>; [defaults] fills what a session omits.
# Reference: docs/operating.md.

[defaults]
model = "anthropic/claude-sonnet-4-5"
spotter = "anthropic/claude-haiku-4-5"

[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }
"#
    );
    let parsed = crate::inference::InferenceConfig::from_toml(&render("", &draft).unwrap());
    assert!(parsed.is_ok());
}

/// A key not in the shell can be pasted for the check alone: the probe sees it,
/// the file never does.
#[tokio::test]
async fn pasted_key_is_checked_and_not_recorded() {
    let probe = Table {
        accepted_keys: strings(&["sk-or-pasted"]),
        known_models: strings(&["anthropic/claude-sonnet-4-5", "anthropic/claude-haiku-4-5"]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("OpenRouter"),
            Answer::Type("router"),
            Answer::Pick("An environment variable"),
            Answer::Type("MY_ROUTER_KEY"),
            Answer::Yes,
            Answer::Secret("sk-or-pasted"),
            Answer::Type(""),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("MY_ROUTER_KEY is not set in this shell"));
    assert_eq!(*probe.seen_keys.borrow(), strings(&["sk-or-pasted"]));
    let text = render("", &draft).unwrap();
    assert!(!text.contains("sk-or-pasted"));
    assert!(text.contains(r#"[connections.router]"#));
    assert!(text.contains(r#"auth = { kind = "env", key = "MY_ROUTER_KEY" }"#));
    assert!(text.contains(r#"model = "router/anthropic/claude-sonnet-4-5""#));
}

/// A rejected key is said, and declining to keep the connection cancels.
#[tokio::test]
async fn rejected_key_can_cancel() {
    let probe = Table {
        env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "sk-wrong".to_string())]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("OpenAI"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    assert_eq!(draft, Err(Cancelled));
    assert!(script.said("rejected the key: 401: bad key"));
}

/// Keeping a connection past a rejected key means no live checks after it:
/// the model is taken as typed.
#[tokio::test]
async fn rejected_key_kept_skips_model_checks() {
    let probe = Table {
        env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "sk-wrong".to_string())]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("OpenAI"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::Yes,
            Answer::Type("gpt-5.6"),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(draft.connections[0].1.model, "gpt-5.6");
    assert_eq!(draft.connections[0].1.spotter, "gpt-5.6-luna");
    assert!(!script.said("does not know"));
}

/// Codex needs no key prompt: the cache is checked, and a missing login is a
/// note rather than a stop.
#[tokio::test]
async fn codex_without_login_is_recorded_on_request() {
    let probe = Table::default();
    let (draft, script) = run_script(
        vec![
            Answer::Pick("ChatGPT subscription via Codex"),
            Answer::Type(""),
            Answer::Yes,
            Answer::Type(""),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("No Codex login found: no auth.json"));
    assert_eq!(draft.connections[0].0, "openai-codex");
    assert_eq!(draft.connections[0].1.auth, AuthSource::CodexCache);
    let text = render("", &draft).unwrap();
    assert!(text.contains(r#"auth = { kind = "codex-cache" }"#));
    assert!(text.contains(r#"model = "openai-codex/gpt-5.6""#));
}

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
            Answer::Pick("ChatGPT subscription via Codex"),
            Answer::Type("anthropic"),
            Answer::Type("codex"),
            Answer::Type(""),
            Answer::Type(""),
            Answer::No,
            Answer::No,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("Existing connections: anthropic (anthropic)"));
    assert!(script.said("\"anthropic\" is already a connection"));
    assert!(script.said("Found a Codex login for account acct_1"));
    assert_eq!(draft.default_connection, None);
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
            Answer::No,
            Answer::Type(""),
            Answer::Type(""),
            Answer::Yes,
            Answer::Pick("ChatGPT subscription via Codex"),
            Answer::Type(""),
            Answer::Type(""),
            Answer::Type(""),
            Answer::No,
            Answer::Pick("openai-codex"),
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(draft.default_connection.as_deref(), Some("openai-codex"));
    let text = render("", &draft).unwrap();
    assert!(text.contains("model = \"openai-codex/gpt-5.6\""));
    assert!(text.contains("spotter = \"openai-codex/gpt-5.6-luna\""));
    assert!(text.contains("[connections.anthropic]"));
    assert!(text.contains("[connections.openai-codex]"));
}

/// The keychain entry is offered and refused in this build.
#[test]
fn keychain_is_shown_unavailable() {
    struct Capture(Vec<Choice>);
    impl Prompter for Capture {
        fn say(&mut self, _: Tone, _: &str) {}
        fn select(&mut self, _: &str, choices: &[Choice]) -> Result<usize, Cancelled> {
            self.0 = choices.to_vec();
            Err(Cancelled)
        }
        fn text(&mut self, _: &str, _: Option<&str>) -> Result<String, Cancelled> {
            Ok("x".into())
        }
        fn secret(&mut self, _: &str) -> Result<String, Cancelled> {
            Err(Cancelled)
        }
        fn confirm(&mut self, _: &str, _: bool) -> Result<bool, Cancelled> {
            Err(Cancelled)
        }
    }
    let mut capture = Capture(Vec::new());
    let probe = Table::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    // The first select is the provider menu; the capture cancels there, so
    // drive the key-source question directly.
    let result = runtime.block_on(super::flow::api_key_source(
        &mut capture,
        &probe,
        Provider::Anthropic,
        "ANTHROPIC_API_KEY",
    ));
    assert_eq!(result.unwrap_err(), Cancelled);
    let keychain = capture
        .0
        .iter()
        .find(|choice| choice.label.contains("keychain"))
        .unwrap();
    assert!(
        keychain
            .unavailable
            .as_deref()
            .unwrap()
            .contains("signed binary")
    );
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
            Answer::Pick("Anthropic"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type("ANTHROPIC_PERSONAL_KEY"),
            Answer::No,
            Answer::Type(""),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(draft.connections[0].0, "anthropic-2");
    // No standing default, so the new connection becomes it.
    assert_eq!(draft.default_connection.as_deref(), Some("anthropic-2"));
    let text = render(existing, &draft).unwrap();
    assert!(
        text.starts_with("[defaults]\nmodel = \"anthropic-2/claude-sonnet-4-5\""),
        "{text}"
    );
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
