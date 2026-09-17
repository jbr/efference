use std::collections::VecDeque;

use super::*;

/// A scripted [`Prompter`], as `src/setup/test.rs` scripts the setup flow: the
/// answers are consumed in order and every line said is kept for assertions.
struct Script {
    answers: VecDeque<Option<&'static str>>,
    transcript: Vec<String>,
    /// The entries of each menu asked, by label.
    menus: Vec<Vec<String>>,
}

impl Script {
    /// `None` is Esc.
    fn new(answers: Vec<Option<&'static str>>) -> Self {
        Self {
            answers: answers.into(),
            transcript: Vec::new(),
            menus: Vec::new(),
        }
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
        self.menus.push(
            choices
                .iter()
                .map(|choice| match &choice.detail {
                    Some(detail) => format!("{}: {detail}", choice.label),
                    None => choice.label.clone(),
                })
                .collect(),
        );
        let answer = self
            .answers
            .pop_front()
            .unwrap_or_else(|| panic!("no scripted answer for {question:?}"));
        let Some(answer) = answer else {
            return Err(Cancelled);
        };
        choices
            .iter()
            .position(|choice| choice.label.starts_with(answer))
            .ok_or(Cancelled)
    }

    fn text(&mut self, question: &str, _default: Option<&str>) -> Result<String, Cancelled> {
        panic!("the picker asks no text question ({question})")
    }

    fn secret(&mut self, question: &str) -> Result<String, Cancelled> {
        panic!("the picker asks no secret ({question})")
    }

    fn confirm(&mut self, question: &str, _default: bool) -> Result<bool, Cancelled> {
        panic!("the picker asks no confirmation ({question})")
    }
}

/// One listing as the wire carries it, decoded the way the client decodes
/// `GET /sessions`.
fn listing(json: &str) -> SessionSummary {
    serde_json::from_str(json).unwrap()
}

fn host() -> PathBuf {
    PathBuf::from("/tmp/efference-picker/host.sock")
}

fn here() -> PathBuf {
    PathBuf::from("/tmp/efference-picker/project")
}

fn listings(entries: &[&str]) -> Vec<(PathBuf, Vec<SessionSummary>)> {
    vec![(host(), entries.iter().map(|json| listing(json)).collect())]
}

const HERE_SESSION: &str = r#"{
  "id": "pickled-wombat",
  "title": "investigating the cause of the missing radish",
  "cwd": "/tmp/efference-picker/project",
  "status": "idle"
}"#;

const ELSEWHERE_SESSION: &str = r#"{
  "id": "frigid-salamander",
  "title": "considering the nature of existence",
  "cwd": "/tmp/efference-picker/elsewhere",
  "status": "idle"
}"#;

#[test]
fn rows_are_this_directorys_sessions_and_the_rest_is_a_count() {
    let menu = menu(&listings(&[HERE_SESSION, ELSEWHERE_SESSION]), &here());
    assert_eq!(menu.rows.len(), 1);
    assert_eq!(menu.rows[0].id, "pickled-wombat");
    assert_eq!(menu.rows[0].host, host());
    assert_eq!(menu.elsewhere, 1);
}

/// A listing with no cwd cannot be placed, so it is kept rather than counted
/// away — the same rule `hosted_here` applies.
#[test]
fn a_listing_without_a_cwd_is_kept() {
    let menu = menu(
        &listings(&[r#"{"id": "placeless", "status": "idle"}"#]),
        &here(),
    );
    assert_eq!(menu.rows.len(), 1);
    assert_eq!(menu.elsewhere, 0);
}

#[test]
fn a_row_carries_the_title_and_the_create_entry_is_last() {
    let menu = menu(&listings(&[HERE_SESSION]), &here());
    let choices = choices(&menu, &here());
    assert_eq!(choices[0].label, "pickled-wombat");
    assert_eq!(
        choices[0].detail.as_deref(),
        Some("investigating the cause of the missing radish")
    );
    assert_eq!(choices[1].label, "create a new session");
    assert_eq!(
        choices[1].detail.as_deref(),
        Some("in /tmp/efference-picker/project")
    );
    assert_eq!(choices.len(), 2);
}

#[test]
fn an_untitled_session_says_so_and_names_its_creator() {
    let menu = menu(
        &listings(&[
            r#"{"id": "fresh", "cwd": "/tmp/efference-picker/project", "created_by": "pickled-wombat", "status": "idle"}"#,
        ]),
        &here(),
    );
    assert_eq!(
        choices(&menu, &here())[0].detail.as_deref(),
        Some("untitled · created by pickled-wombat")
    );
}

#[test]
fn picking_a_row_attaches_to_it_on_its_host() {
    let menu = menu(&listings(&[HERE_SESSION, ELSEWHERE_SESSION]), &here());
    let mut script = Script::new(vec![Some("pickled-wombat")]);
    let pick = pick(&mut script, &menu, &here()).unwrap();
    assert_eq!(
        pick,
        Pick::Attach {
            host: host(),
            id: "pickled-wombat".to_string()
        }
    );
    assert!(script.said("1 session on this host is in other directories"));
}

#[test]
fn picking_the_last_entry_creates_here() {
    let menu = menu(&listings(&[HERE_SESSION]), &here());
    let mut script = Script::new(vec![Some("create a new session")]);
    assert_eq!(pick(&mut script, &menu, &here()).unwrap(), Pick::Create);
}

/// With nothing here the menu is still asked: it says the directory is empty and
/// offers the one thing there is to do.
#[test]
fn the_empty_state_offers_only_create() {
    let menu = menu(&listings(&[ELSEWHERE_SESSION]), &here());
    assert!(menu.rows.is_empty());
    let mut script = Script::new(vec![Some("create a new session")]);
    assert_eq!(pick(&mut script, &menu, &here()).unwrap(), Pick::Create);
    assert!(script.said("no efference sessions in /tmp/efference-picker/project"));
    assert_eq!(script.menus[0].len(), 1);
}

#[test]
fn escape_is_cancelled_rather_than_a_pick() {
    let menu = menu(&listings(&[HERE_SESSION]), &here());
    let mut script = Script::new(vec![None]);
    assert_eq!(pick(&mut script, &menu, &here()), Err(Cancelled));
}

/// No host is a message naming both sockets and the command that binds one,
/// rather than a bare resolution error.
#[test]
fn no_host_names_what_was_tried_and_what_to_run() {
    let message = no_host_message(&[
        PathBuf::from(".efference/host.sock"),
        PathBuf::from("/home/someone/.efference/host.sock"),
    ]);
    assert!(message.contains(".efference/host.sock"), "{message}");
    assert!(
        message.contains("/home/someone/.efference/host.sock"),
        "{message}"
    );
    assert!(message.contains("`efference host`"), "{message}");
}

#[test]
fn a_path_under_home_is_written_with_a_tilde() {
    let home = Some(Path::new("/home/someone"));
    assert_eq!(
        under_home(Path::new("/home/someone/code/thing"), home),
        "~/code/thing"
    );
    assert_eq!(under_home(Path::new("/home/someone"), home), "~");
    assert_eq!(under_home(Path::new("/var/tmp"), home), "/var/tmp");
    assert_eq!(
        under_home(Path::new("/home/someone-else"), home),
        "/home/someone-else"
    );
    assert_eq!(
        under_home(Path::new("/home/someone"), None),
        "/home/someone"
    );
}
