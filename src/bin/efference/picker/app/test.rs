use super::*;

const NOW: u64 = 1_800_000_000_000;
const MINUTE: u64 = 60_000;
const HOUR: u64 = 3_600_000;

fn here() -> PathBuf {
    PathBuf::from("/tmp/efference-picker/project")
}

fn archived(id: &str, title: Option<&str>, ago_ms: u64) -> InventoryRow {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "cwd": "/tmp/efference-picker/project",
        "title": title,
        "created_at": NOW - ago_ms - HOUR,
        "last_active_at": NOW - ago_ms,
    }))
    .unwrap()
}

fn live(id: &str, title: Option<&str>, status: &str) -> ListedSession {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "title": title,
        "cwd": "/tmp/efference-picker/project",
        "status": status,
    }))
    .unwrap()
}

/// One live titled session, and three archived: titled, untitled, titled.
fn full_menu() -> Menu {
    menu(
        &[(
            PathBuf::from("/tmp/efference-picker/host.sock"),
            vec![live(
                "pickled-wombat",
                Some("investigating the cause of the missing radish"),
                "running",
            )],
        )],
        &here(),
    )
    .with_archived(
        vec![
            archived(
                "quiet-heron",
                Some("tracing the flaky socket test"),
                16 * MINUTE,
            ),
            archived("plain-otter", None, 50 * HOUR),
            archived(
                "brisk-newt",
                Some("porting the radish parser"),
                3 * 24 * HOUR,
            ),
        ],
        NOW,
    )
}

fn archived_only() -> Menu {
    menu(&[], &here()).with_archived(vec![archived("quiet-heron", None, HOUR)], NOW)
}

fn press(app: &mut App, code: KeyCode) -> Outcome {
    app.handle(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(app: &mut App, c: char) -> Outcome {
    app.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn styles() -> Styles {
    let plain = ratatui::style::Style::default();
    Styles {
        text: plain,
        title: plain,
        hint: plain,
        selected: plain,
    }
}

fn drawn(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    app.render(&mut buf, area, styles());
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_cursor_starts_on_create() {
    let menu = full_menu();
    let mut app = App::new(&menu, &here());
    assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Pick(Pick::Create));
}

#[test]
fn control_keys_move_to_a_live_row_and_back() {
    let menu = full_menu();
    let mut app = App::new(&menu, &here());
    ctrl(&mut app, 'n');
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Outcome::Pick(Pick::Attach {
            host: PathBuf::from("/tmp/efference-picker/host.sock"),
            id: "pickled-wombat".to_string()
        })
    );
    ctrl(&mut app, 'p');
    assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Pick(Pick::Create));
    // The cursor stops at the last entry rather than wrapping.
    for _ in 0..5 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.page, Page::Resume);
}

#[test]
fn the_resume_entry_opens_the_resume_page_and_escape_backs_out() {
    let menu = full_menu();
    let mut app = App::new(&menu, &here());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
    assert_eq!(app.page, Page::Resume);
    assert_eq!(press(&mut app, KeyCode::Esc), Outcome::Continue);
    assert_eq!(app.page, Page::Top);
    assert_eq!(press(&mut app, KeyCode::Esc), Outcome::Cancel);
}

#[test]
fn escape_clears_a_filter_before_it_leaves_the_resume_page() {
    let menu = full_menu();
    let mut app = App::new(&menu, &here());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    type_str(&mut app, "radish");
    assert_eq!(press(&mut app, KeyCode::Esc), Outcome::Continue);
    assert_eq!(app.page, Page::Resume);
    assert!(drawn(&app, 100, 8).contains("type to filter"));
    assert_eq!(press(&mut app, KeyCode::Esc), Outcome::Continue);
    assert_eq!(app.page, Page::Top);
}

#[test]
fn control_c_cancels_from_either_page() {
    let menu = full_menu();
    let mut app = App::new(&menu, &here());
    assert_eq!(ctrl(&mut app, 'c'), Outcome::Cancel);
    app.page = Page::Resume;
    assert_eq!(ctrl(&mut app, 'c'), Outcome::Cancel);
}

#[test]
fn the_top_page_without_live_or_archived_rows_has_only_create() {
    let menu = Menu::default();
    let app = App::new(&menu, &here());
    assert_eq!(app.items(), [Item::Create]);
}

#[test]
fn the_top_page_renders_one_shape_per_row() {
    let menu = full_menu();
    let mut app = App::new(&menu, &here());
    let text = drawn(&app, 100, 8);
    println!("{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "? sessions in /tmp/efference-picker/project");
    assert_eq!(lines[1], "❯ create a new session");
    assert_eq!(lines[2], "  ── running here");
    assert_eq!(
        lines[3],
        "  investigating the cause of the missing radish  pickled-wombat  running"
    );
    assert_eq!(
        lines[4],
        "  resume an archived session…                                    3"
    );
    assert!(lines[6].contains("enter picks"), "{text}");
    ctrl(&mut app, 'n');
    assert!(drawn(&app, 100, 8).contains("❯ investigating"));
}

#[test]
fn the_resume_page_aligns_id_and_age_and_an_untitled_row_shows_its_id_twice() {
    let menu = full_menu();
    let mut app = App::new(&menu, &here());
    app.page = Page::Resume;
    let text = drawn(&app, 100, 8);
    println!("{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "? archived sessions in /tmp/efference-picker/project"
    );
    assert_eq!(lines[1], "  ▏type to filter by title or id");
    assert_eq!(
        lines[2],
        "❯ tracing the flaky socket test  quiet-heron  16m ago"
    );
    assert_eq!(
        lines[3],
        "  plain-otter                    plain-otter  2d ago"
    );
    assert_eq!(
        lines[4],
        "  porting the radish parser      brisk-newt   3d ago"
    );
    type_str(&mut app, "heron");
    let text = drawn(&app, 100, 8);
    assert!(text.contains("1 of 3"), "{text}");
    assert!(!text.contains("plain-otter"), "{text}");
    type_str(&mut app, "zzz");
    assert!(drawn(&app, 100, 8).contains("nothing matches"));
}

#[test]
fn a_narrow_width_drops_the_status_then_cuts_the_title() {
    let menu = full_menu();
    let app = App::new(&menu, &here());
    // 2 gutter + title + 2 + 14-wide id + 2 + 7-wide status = 80 - title ≤ 53.
    let text = drawn(&app, 60, 8);
    println!("{text}");
    let row = text.lines().nth(3).unwrap();
    assert!(row.contains("pickled-wombat"), "{row}");
    assert!(!row.contains("running"), "{row}");
    assert!(row.contains('…'), "{row}");
    assert!(row.chars().count() <= 60, "{row}");
}

#[test]
fn the_viewport_fits_the_taller_page_within_the_terminal() {
    let menu = full_menu();
    let app = App::new(&menu, &here());
    // Top: three rows and a separator, plus the question, status and hint; the
    // resume page: three rows plus the same and the input line.
    assert_eq!(app.viewport_height(50), 7);
    // Bounded by the terminal.
    assert_eq!(app.viewport_height(6), 5);
    let many: Vec<InventoryRow> = (0..40)
        .map(|n| archived(&format!("old-{n}"), None, HOUR))
        .collect();
    let menu = archived_only();
    let app = App::new(&menu, &here());
    assert_eq!(app.viewport_height(50), 5);
    let menu = Menu::default().with_archived(many, NOW);
    assert_eq!(App::new(&menu, &here()).viewport_height(50), MAX_HEIGHT);
}
