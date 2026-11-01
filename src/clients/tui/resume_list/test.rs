use super::*;
use crossterm::event::KeyModifiers;

const NOW: u64 = 1_800_000_000_000;
const MINUTE: u64 = 60_000;
const HOUR: u64 = 3_600_000;

fn inventory(id: &str, title: Option<&str>, ago_ms: u64, problem: Option<&str>) -> InventoryRow {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "cwd": "/work/project",
        "title": title,
        "created_at": NOW - ago_ms - HOUR,
        "last_active_at": NOW - ago_ms,
        "problem": problem,
    }))
    .unwrap()
}

fn row(id: &str, title: Option<&str>, ago_ms: u64) -> ResumeRow {
    ResumeRow::from_inventory(vec![inventory(id, title, ago_ms, None)], NOW).remove(0)
}

/// Three rows: titled, untitled, titled.
fn page() -> ResumePage {
    ResumePage::new(vec![
        row(
            "quiet-heron",
            Some("tracing the flaky socket test"),
            16 * MINUTE,
        ),
        row("plain-otter", None, 50 * HOUR),
        row(
            "brisk-newt",
            Some("porting the radish parser"),
            3 * 24 * HOUR,
        ),
    ])
}

fn press(page: &mut ResumePage, code: KeyCode) -> ResumeKey {
    page.handle(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(page: &mut ResumePage, c: char) -> ResumeKey {
    page.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn type_str(page: &mut ResumePage, text: &str) {
    for c in text.chars() {
        press(page, KeyCode::Char(c));
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

fn drawn(page: &ResumePage, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    page.render(&mut buf, area, styles(), "archived sessions here");
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
fn ages_read_in_the_largest_whole_unit() {
    assert_eq!(age(NOW, NOW - 30_000), "just now");
    assert_eq!(age(NOW, NOW - 5 * 60_000), "5m ago");
    assert_eq!(age(NOW, NOW - 3 * HOUR), "3h ago");
    assert_eq!(age(NOW, NOW - 2 * 24 * HOUR - 1), "2d ago");
    assert_eq!(age(NOW, NOW + HOUR), "just now");
}

#[test]
fn rows_with_a_problem_are_not_offered() {
    let rows = ResumeRow::from_inventory(
        vec![
            inventory("fine", Some("ok"), HOUR, None),
            inventory("broken", None, HOUR, Some("no log.jsonl")),
        ],
        NOW,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "fine");
    assert_eq!(rows[0].age.as_deref(), Some("1h ago"));
}

#[test]
fn typing_filters_on_title_and_id() {
    let mut page = page();
    assert_eq!(page.matches().len(), 3);
    type_str(&mut page, "radish");
    assert_eq!(page.matches().len(), 1);
    assert_eq!(
        press(&mut page, KeyCode::Enter),
        ResumeKey::Pick("brisk-newt".into())
    );
    // The id matches too, and an untitled row is found by it.
    for _ in 0..6 {
        press(&mut page, KeyCode::Backspace);
    }
    type_str(&mut page, "otter");
    assert_eq!(
        press(&mut page, KeyCode::Enter),
        ResumeKey::Pick("plain-otter".into())
    );
    // Every word must match, in any order; no match, no pick.
    for _ in 0..5 {
        press(&mut page, KeyCode::Backspace);
    }
    type_str(&mut page, "parser porting");
    assert_eq!(page.matches().len(), 1);
    type_str(&mut page, " zzz");
    assert_eq!(press(&mut page, KeyCode::Enter), ResumeKey::Continue);
}

#[test]
fn escape_clears_a_filter_before_it_asks_to_go_back() {
    let mut page = page();
    type_str(&mut page, "radish");
    assert_eq!(press(&mut page, KeyCode::Esc), ResumeKey::Continue);
    assert!(page.query().is_empty());
    assert_eq!(page.matches().len(), 3);
    assert_eq!(press(&mut page, KeyCode::Esc), ResumeKey::Back);
}

#[test]
fn a_new_query_puts_the_cursor_back_on_the_first_match() {
    let mut page = page();
    ctrl(&mut page, 'n');
    ctrl(&mut page, 'n');
    assert_eq!(
        press(&mut page, KeyCode::Enter),
        ResumeKey::Pick("brisk-newt".into())
    );
    type_str(&mut page, "o");
    assert_eq!(
        press(&mut page, KeyCode::Enter),
        ResumeKey::Pick("quiet-heron".into())
    );
}

#[test]
fn a_pasted_query_filters() {
    let mut page = page();
    page.paste("heron");
    assert_eq!(page.matches().len(), 1);
}

#[test]
fn enter_waits_while_a_resume_is_out_and_a_failure_frees_it() {
    let mut page = page();
    page.begin_resume("quiet-heron");
    assert_eq!(press(&mut page, KeyCode::Enter), ResumeKey::Continue);
    assert!(drawn(&page, 80, 8).contains("resuming quiet-heron…"));
    page.resume_failed("resuming quiet-heron: no such directory".into());
    let text = drawn(&page, 80, 8);
    assert!(text.contains("no such directory"), "{text}");
    assert_eq!(
        press(&mut page, KeyCode::Enter),
        ResumeKey::Pick("quiet-heron".into())
    );
}

#[test]
fn a_loading_page_says_so_until_the_rows_land_or_the_read_fails() {
    let mut page = ResumePage::loading();
    assert!(drawn(&page, 80, 8).contains("reading the archived sessions…"));
    assert_eq!(press(&mut page, KeyCode::Enter), ResumeKey::Continue);
    page.read_failed("listing archived sessions: refused".into());
    let text = drawn(&page, 80, 8);
    assert!(text.contains("refused"), "{text}");
    assert!(!text.contains("reading"), "{text}");
    page.set_rows(vec![row("quiet-heron", None, HOUR)]);
    let text = drawn(&page, 80, 8);
    assert!(text.contains("❯ quiet-heron"), "{text}");
    assert!(!text.contains("refused"), "{text}");
    let empty = ResumePage::new(Vec::new());
    assert!(drawn(&empty, 80, 8).contains("no archived sessions here"));
}

#[test]
fn the_page_aligns_id_and_age_and_an_untitled_row_shows_its_id_twice() {
    let mut page = page();
    let text = drawn(&page, 100, 8);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "? archived sessions here");
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
    type_str(&mut page, "heron");
    let text = drawn(&page, 100, 8);
    assert!(text.contains("1 of 3"), "{text}");
    assert!(!text.contains("plain-otter"), "{text}");
    type_str(&mut page, "zzz");
    assert!(drawn(&page, 100, 8).contains("nothing matches"));
}

#[test]
fn a_narrow_width_cuts_titles_but_keeps_the_id() {
    let text = drawn(&page(), 34, 8);
    let row = text.lines().nth(2).unwrap();
    assert!(row.contains('…') && row.contains("quiet-heron"), "{row}");
}

#[test]
fn a_wide_terminal_shows_a_full_length_title_whole() {
    let title = "a".repeat(80);
    let page = ResumePage::new(vec![row("quiet-heron", Some(&title), HOUR)]);
    let text = drawn(&page, 140, 8);
    let row = text.lines().nth(2).unwrap();
    assert!(row.contains(&title) && !row.contains('…'), "{row}");
}

#[test]
fn the_window_scrolls_within_a_short_viewport() {
    let rows = (0..30)
        .map(|n| row(&format!("old-{n:02}"), None, (n + 1) * HOUR))
        .collect();
    let mut page = ResumePage::new(rows);
    for _ in 0..29 {
        press(&mut page, KeyCode::Down);
    }
    let text = drawn(&page, 100, 8);
    assert!(text.contains("❯ old-29"), "{text}");
    assert!(text.contains("30/30"), "{text}");
    assert!(!text.contains("old-00"), "{text}");
}

#[test]
fn the_height_covers_every_row_plus_the_chrome() {
    assert_eq!(page().height(), 7);
    assert_eq!(ResumePage::new(Vec::new()).height(), 0);
}
