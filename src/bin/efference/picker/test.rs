use super::*;

/// One listing as the wire carries it, decoded the way the client decodes
/// `GET /sessions`.
fn listing(json: &str) -> ListedSession {
    serde_json::from_str(json).unwrap()
}

fn here() -> PathBuf {
    PathBuf::from("/tmp/efference-picker/project")
}

fn listings(entries: &[&str]) -> Vec<ListedSession> {
    entries.iter().map(|json| listing(json)).collect()
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
    assert_eq!(menu.rows[0].status, "idle");
    assert_eq!(menu.elsewhere, 1);
}

/// A status word this build does not name, from a host newer than the picker, is shown as
/// the host sent it.
#[test]
fn a_status_this_build_does_not_name_is_shown_as_it_came() {
    let menu = menu(
        &listings(&[
            r#"{"id": "odd", "cwd": "/tmp/efference-picker/project", "status": "hibernating"}"#,
        ]),
        &here(),
    );
    assert_eq!(menu.rows.len(), 1);
    assert_eq!(menu.rows[0].status, "hibernating");
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

/// Every resumable archived session is kept, in the order given; a row with a
/// problem is not offered.
#[test]
fn archived_rows_are_all_kept_and_problems_are_hidden() {
    let rows: Vec<InventoryRow> = (0..12)
        .map(|n| row(&format!("old-{n}"), None, (n + 1) * HOUR, None))
        .chain([row(
            "broken",
            Some("unreadable"),
            HOUR,
            Some("no log.jsonl"),
        )])
        .collect();
    let menu = menu(&listings(&[]), &here()).with_archived(rows, NOW);
    assert_eq!(menu.archived.len(), 12);
    assert_eq!(menu.archived[0].id, "old-0");
    assert_eq!(menu.archived[0].age.as_deref(), Some("1h ago"));
    assert!(menu.archived.iter().all(|a| a.id != "broken"));
}

#[test]
fn the_other_directories_line_counts_and_agrees() {
    assert_eq!(app::elsewhere_note(0), None);
    assert!(
        app::elsewhere_note(1)
            .unwrap()
            .starts_with("1 session on this host is in other directories")
    );
    assert!(
        app::elsewhere_note(3)
            .unwrap()
            .starts_with("3 sessions on this host are in other directories")
    );
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
