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

/// With nothing here to attach to, the picker asks nothing: its one entry would be
/// "create", and the new-session form that follows has its own cancel. A running
/// host and a terminal island then open the same form (docs/tui.md).
#[test]
fn the_empty_state_goes_straight_to_create() {
    let menu = menu(&listings(&[ELSEWHERE_SESSION]), &here());
    assert!(menu.rows.is_empty());
    let mut script = Script::new(vec![]);
    assert_eq!(pick(&mut script, &menu, &here()).unwrap(), Pick::Create);
    assert!(script.menus.is_empty());
    // The sessions elsewhere are still pointed at.
    assert!(script.said("in other directories"));
}

#[test]
fn escape_is_cancelled_rather_than_a_pick() {
    let menu = menu(&listings(&[HERE_SESSION]), &here());
    let mut script = Script::new(vec![None]);
    assert_eq!(pick(&mut script, &menu, &here()), Err(Cancelled));
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

const NOW: u64 = 1_800_000_000_000;
const HOUR: u64 = 3_600_000;

/// An inventory row as the route and the scan carry it, `ago_ms` before [`NOW`].
fn row(id: &str, title: Option<&str>, ago_ms: u64, problem: Option<&str>) -> InventoryRow {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "cwd": "/tmp/efference-picker/project",
        "title": title,
        "created_at": NOW - ago_ms - HOUR,
        "last_active_at": NOW - ago_ms,
        "problem": problem,
    }))
    .unwrap()
}

#[test]
fn ages_read_in_the_largest_whole_unit() {
    assert_eq!(age(NOW, NOW - 30_000), "just now");
    assert_eq!(age(NOW, NOW - 5 * 60_000), "5m ago");
    assert_eq!(age(NOW, NOW - 3 * HOUR), "3h ago");
    assert_eq!(age(NOW, NOW - 2 * 24 * HOUR - 1), "2d ago");
    assert_eq!(age(NOW, NOW + HOUR), "just now");
}

/// The section as the prompt renders it: live rows, archived rows (title or id,
/// then age), the create entry last. Rows with a problem are not there.
#[test]
fn archived_rows_follow_the_live_ones_and_hide_problems() {
    let menu = menu(&listings(&[HERE_SESSION]), &here()).with_archived(
        vec![
            row(
                "quiet-heron",
                Some("tracing the flaky socket test"),
                3 * HOUR,
                None,
            ),
            row("broken", Some("unreadable"), 4 * HOUR, Some("no log.jsonl")),
            row("plain-otter", None, 50 * HOUR, None),
        ],
        NOW,
    );
    let mut script = Script::new(vec![None]);
    let _ = pick(&mut script, &menu, &here());
    assert_eq!(
        script.menus[0],
        [
            "pickled-wombat: investigating the cause of the missing radish",
            "tracing the flaky socket test: archived · 3h ago",
            "plain-otter: archived · 2d ago",
            "create a new session: in /tmp/efference-picker/project",
        ]
    );
    assert!(!script.said("more archived"));
}

#[test]
fn past_ten_archived_rows_the_rest_are_counted() {
    let rows: Vec<InventoryRow> = (0..12)
        .map(|n| row(&format!("old-{n}"), None, (n + 1) * HOUR, None))
        .chain([row("broken", None, HOUR, Some("no log.jsonl"))])
        .collect();
    let menu = menu(&listings(&[]), &here()).with_archived(rows, NOW);
    assert_eq!(menu.archived.len(), ARCHIVED_SHOWN);
    assert_eq!(menu.archived_more, 2);
    let mut script = Script::new(vec![None]);
    let _ = pick(&mut script, &menu, &here());
    assert!(script.said("2 more archived sessions here are not listed."));
    // Ten rows and the create entry.
    assert_eq!(script.menus[0].len(), 11);
}

#[test]
fn picking_an_archived_row_resumes_it() {
    let menu = menu(&listings(&[HERE_SESSION]), &here()).with_archived(
        vec![row(
            "quiet-heron",
            Some("tracing the flaky socket test"),
            HOUR,
            None,
        )],
        NOW,
    );
    let mut script = Script::new(vec![Some("tracing")]);
    assert_eq!(
        pick(&mut script, &menu, &here()).unwrap(),
        Pick::Resume {
            id: "quiet-heron".to_string()
        }
    );
}

/// With nothing live here but something archived, the prompt is asked: the
/// create-only shortcut applies when there is nothing to choose between.
#[test]
fn archived_rows_alone_are_still_asked_about() {
    let menu = menu(&listings(&[]), &here())
        .with_archived(vec![row("quiet-heron", None, HOUR, None)], NOW);
    let mut script = Script::new(vec![Some("create")]);
    assert_eq!(pick(&mut script, &menu, &here()).unwrap(), Pick::Create);
    assert_eq!(script.menus.len(), 1);
}

/// Picking an archived row sends the PATCH to the host, with this directory, and
/// the target it hands back is the revived session on that host.
#[tokio::test]
async fn resuming_patches_the_host_and_attaches() {
    use std::sync::{Arc, Mutex};
    use trillium::Conn;

    let dir = std::env::current_dir().unwrap().join("tmp");
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join(format!("pk-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
    let record = Arc::clone(&seen);
    let swansong = trillium_tokio::Swansong::new();
    let server = trillium_tokio::config()
        .with_swansong(swansong.clone())
        .listeners()
        .bind_uds(&socket)
        .unwrap()
        .spawn(move |mut conn: Conn| {
            let record = Arc::clone(&record);
            async move {
                let body = conn.request_body_string().await.unwrap_or_default();
                record.lock().unwrap().push((
                    conn.method().to_string(),
                    conn.path().to_string(),
                    body,
                ));
                conn.with_status(200).with_body(r#"{"state":"active"}"#)
            }
        });
    server.info().await;

    let target = hosted_target(
        Pick::Resume {
            id: "quiet-heron".to_string(),
        },
        &socket,
    )
    .await
    .unwrap();
    assert_eq!(
        target,
        Some(ClientTarget::hosted(socket.clone(), "quiet-heron"))
    );
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "PATCH");
    assert_eq!(seen[0].1, "/sessions/quiet-heron");
    let body: serde_json::Value = serde_json::from_str(&seen[0].2).unwrap();
    assert_eq!(body["state"], "active");
    assert_eq!(
        body["cwd"],
        std::env::current_dir().unwrap().to_string_lossy().as_ref()
    );

    // A live row attaches without a request; the create entry has the form to run.
    assert_eq!(
        hosted_target(
            Pick::Attach {
                host: socket.clone(),
                id: "pickled-wombat".to_string()
            },
            &socket
        )
        .await
        .unwrap(),
        Some(ClientTarget::hosted(socket.clone(), "pickled-wombat"))
    );
    assert_eq!(hosted_target(Pick::Create, &socket).await.unwrap(), None);
    assert_eq!(seen.len(), 1);

    swansong.shut_down().await;
    let _ = std::fs::remove_file(&socket);
}
