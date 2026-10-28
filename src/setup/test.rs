use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

use super::*;
use crate::secrets::Secrets;

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

    fn confirm(&mut self, question: &str, default: bool) -> Result<bool, Cancelled> {
        let default = if default { "yes" } else { "no" };
        self.transcript
            .push(format!("confirm: {question} [default {default}]"));
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
    /// What a ChatGPT login returns; `None` is a login that does not finish.
    login: Option<ChatGptLogin>,
    seen_keys: RefCell<Vec<String>>,
    /// The connections `chatgpt_login` was called for, in order.
    logged_in: RefCell<Vec<String>>,
    /// `None` is a machine with no store to offer.
    secrets: Option<Secrets>,
}

impl Probe for Table {
    fn env_var(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn secret_store(&self) -> Result<String, String> {
        self.secrets
            .as_ref()
            .map(|secrets| secrets.describe().to_string())
            .ok_or("no store on this platform".to_string())
    }

    fn store_secret(&self, name: &str, value: &str) -> Result<(), String> {
        let secrets = self.secrets.as_ref().ok_or("no store on this platform")?;
        secrets.set(name, value).map_err(|error| error.to_string())
    }

    fn secret_value(&self, name: &str) -> Result<Option<String>, String> {
        let secrets = self.secrets.as_ref().ok_or("no store on this platform")?;
        secrets.get(name).map_err(|error| error.to_string())
    }

    fn delete_secret(&self, name: &str) -> Result<(), String> {
        let secrets = self.secrets.as_ref().ok_or("no store on this platform")?;
        secrets.delete(name).map_err(|error| error.to_string())
    }

    fn codex_cache(&self) -> Result<String, String> {
        self.codex.clone().ok_or("no auth.json".to_string())
    }

    /// No browser and no socket: the canned answer stands in for the dance.
    /// The grant the live probe would store is stood in for by a store item
    /// under the same name, so the readiness row reads it the same way.
    async fn chatgpt_login(
        &self,
        connection: &str,
        _say: &mut dyn FnMut(&str),
    ) -> Result<ChatGptLogin, String> {
        let login = self
            .login
            .clone()
            .ok_or_else(|| "the browser never came back".to_string())?;
        self.logged_in.borrow_mut().push(connection.to_string());
        if let Some(secrets) = &self.secrets {
            secrets
                .set(&crate::secrets::oauth_name(connection), "{}")
                .map_err(|error| error.to_string())?;
        }
        Ok(login)
    }

    async fn check_key(&self, _: &Target, key: &str) -> Result<Option<KeyReport>, String> {
        self.seen_keys.borrow_mut().push(key.to_string());
        if self.accepted_keys.iter().any(|accepted| accepted == key) {
            Ok(Some(KeyReport {
                label: Some("test key".into()),
            }))
        } else {
            Err("GET returned 401: {\"error\":\"bad key\"}".into())
        }
    }

    async fn list_models(&self, target: &Target, _: &str) -> Result<Vec<ModelListing>, String> {
        // A described endpoint's listing stands in for the real one: ids only, as
        // no listing carries a display name.
        if let Provider::Custom(_) = target.provider {
            return Ok(self
                .known_models
                .iter()
                .map(|id| ModelListing {
                    id: id.clone(),
                    display_name: id.clone(),
                })
                .collect());
        }
        if target.provider != Provider::Anthropic {
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

    async fn check_model(&self, _: &Target, _: &str, model: &str) -> Result<ModelReport, String> {
        if self.known_models.iter().any(|known| known == model) {
            Ok(ModelReport {
                context_length: Some(200_000),
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

mod described;
mod file;
mod keys;
mod manage;
mod subscription;
mod updates;

/// The hand-off names what it costs before asking, offers where to run, and a
/// decline says where to run it instead.
#[test]
fn the_handoff_names_the_cost_and_a_decline_says_where_to_go() {
    let mut script = Script::new(vec![Answer::Pick("Not now")]);
    let chosen =
        offer_project_setup(&mut script, "~/code/app", Some("anthropic/claude-sonnet-5")).unwrap();
    assert_eq!(chosen, None);
    assert!(script.said("billed as ordinary use of anthropic/claude-sonnet-5"));
    assert!(script.said("select: Set up ~/code/app now?"));
    assert!(script.said("Run `efference` in the project you want to set up"));

    let mut script = Script::new(vec![Answer::Pick("In this terminal")]);
    assert_eq!(
        offer_project_setup(&mut script, "~/code/app", None).unwrap(),
        Some(Surface::Terminal)
    );
    assert!(script.said("you'll pick the model in the form"));

    let mut script = Script::new(vec![Answer::Pick("In the browser")]);
    assert_eq!(
        offer_project_setup(&mut script, "~/code/app", None).unwrap(),
        Some(Surface::Browser)
    );
}
