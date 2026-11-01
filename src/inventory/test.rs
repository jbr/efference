use super::*;
use crate::record::log::{Title, TitleAuthor};
use crate::record::persist::Log;
use std::io::Write;
use std::time::Duration;

/// A scratch working root, removed on drop.
struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("efference-inventory-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".efference")).unwrap();
        Self(root)
    }

    fn dir(&self, id: &str) -> PathBuf {
        let dir = self.0.join(".efference").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn log(&self, id: &str) -> PathBuf {
        self.dir(id).join(crate::record::session_dir::LOG_NAME)
    }

    /// A resumable session: the boot preamble, then one user entry, written
    /// through the production log.
    fn session(&self, id: &str) -> Log {
        let mut log = Log::create(self.log(id), None).unwrap();
        append_preamble(&mut log);
        tick();
        append_user(&mut log);
        log
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tick() {
    std::thread::sleep(Duration::from_millis(5));
}

fn append_preamble(log: &mut Log) {
    log.append(0, Payload::preamble("", "system", Default::default(), None))
        .unwrap();
}

fn append_user(log: &mut Log) {
    log.append(0, Payload::user("hi")).unwrap();
}

fn title(log: &mut Log, text: &str) {
    tick();
    log.append(0, Payload::Title(Title::new(text, TitleAuthor::Triage)))
        .unwrap();
}

fn row<'a>(rows: &'a [InventoryRow], id: &str) -> &'a InventoryRow {
    rows.iter().find(|row| row.id == id).expect("a row per dir")
}

#[test]
fn a_titled_session_reports_its_title_and_both_timestamps() {
    let root = Root::new("titled");
    let mut log = root.session("titled");
    title(&mut log, "First name");
    drop(log);

    let rows = Inventory::new().refresh(&root.0);
    let titled = row(&rows, "titled");
    assert_eq!(titled.title.as_deref(), Some("First name"));
    assert_eq!(titled.cwd, root.0);
    assert_eq!(titled.problem, None);
    assert!(titled.created_at.unwrap() < titled.last_active_at.unwrap());
}

#[test]
fn the_last_title_wins() {
    let root = Root::new("retitled");
    let mut log = root.session("retitled");
    title(&mut log, "Old");
    title(&mut log, "Newer");
    tick();
    append_user(&mut log);
    drop(log);

    let rows = Inventory::new().refresh(&root.0);
    assert_eq!(row(&rows, "retitled").title.as_deref(), Some("Newer"));
}

#[test]
fn a_title_is_found_across_chunk_boundaries() {
    let root = Root::new("chunks");
    let mut log = root.session("chunks");
    title(&mut log, "Early title");
    let filler = "x".repeat(10_000);
    for _ in 0..30 {
        log.append(0, Payload::user(filler.clone())).unwrap();
    }
    // A later entry that mentions the marker without being a title.
    log.append(0, Payload::user("\"Title\"")).unwrap();
    drop(log);

    let rows = Inventory::new().refresh(&root.0);
    assert_eq!(row(&rows, "chunks").title.as_deref(), Some("Early title"));
}

#[test]
fn an_untitled_session_has_no_title_and_no_problem() {
    let root = Root::new("untitled");
    drop(root.session("untitled"));

    let rows = Inventory::new().refresh(&root.0);
    let untitled = row(&rows, "untitled");
    assert_eq!(untitled.title, None);
    assert_eq!(untitled.problem, None);
    assert!(untitled.last_active_at.is_some());
}

/// Logs that predate the boot preamble parse fine and keep their title and
/// timestamps, but `boot::resume` refuses them, so the row says so.
#[test]
fn a_log_without_a_boot_preamble_is_flagged_and_keeps_its_facts() {
    let root = Root::new("no-preamble");
    let mut log = Log::create(root.log("early"), None).unwrap();
    append_user(&mut log);
    title(&mut log, "From before preambles");
    drop(log);

    let rows = Inventory::new().refresh(&root.0);
    let early = row(&rows, "early");
    assert!(
        early.problem.as_deref().unwrap().contains("boot preamble"),
        "{early:?}"
    );
    assert_eq!(early.title.as_deref(), Some("From before preambles"));
    assert!(early.created_at.unwrap() <= early.last_active_at.unwrap());
}

#[test]
fn a_torn_final_line_is_skipped_back_over() {
    let root = Root::new("torn");
    let mut log = root.session("torn");
    title(&mut log, "Survives");
    drop(log);
    let path = root.log("torn");
    let before = Inventory::new().refresh(&root.0);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(br#"{"seq":3,"turn":0,"at":99999999999999,"payl"#)
        .unwrap();

    let rows = Inventory::new().refresh(&root.0);
    let torn = row(&rows, "torn");
    assert_eq!(torn.problem, None);
    assert_eq!(torn.title.as_deref(), Some("Survives"));
    assert_eq!(torn.last_active_at, row(&before, "torn").last_active_at);
}

#[test]
fn a_complete_last_line_that_does_not_parse_is_a_problem() {
    let root = Root::new("garbled");
    drop(root.session("garbled"));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(root.log("garbled"))
        .unwrap();
    file.write_all(b"not json\n").unwrap();

    let rows = Inventory::new().refresh(&root.0);
    let garbled = row(&rows, "garbled");
    assert!(
        garbled
            .problem
            .as_deref()
            .unwrap()
            .starts_with("last entry does not parse"),
        "{garbled:?}"
    );
    assert!(garbled.created_at.is_some(), "what was learned is kept");
}

#[test]
fn problem_directories_are_rows() {
    let root = Root::new("problems");
    root.dir("no-log");
    std::fs::write(root.log("empty"), "").unwrap();
    std::fs::write(root.log("bad-header"), "this is not json\n").unwrap();
    std::fs::write(
        root.log("newer"),
        "{\"format\":\"efference-session-log\",\"version\":99,\"metadata\":null}\n",
    )
    .unwrap();
    std::fs::write(root.log("unversioned"), "{\"seq\":0,\"turn\":0,\"at\":1}\n").unwrap();
    drop(Log::create(root.log("header-only"), None).unwrap());
    // A plain file beside the directories is not a session.
    std::fs::write(root.0.join(".efference/stray.txt"), "x").unwrap();

    let rows = Inventory::new().refresh(&root.0);
    assert_eq!(rows.len(), 6, "{rows:?}");
    let problem = |id| row(&rows, id).problem.clone().unwrap();
    assert_eq!(problem("no-log"), "no log.jsonl");
    assert_eq!(problem("empty"), "empty log: no header");
    assert!(problem("bad-header").starts_with("header: "), "{rows:?}");
    assert!(problem("newer").contains("unsupported log version 99"));
    assert!(problem("unversioned").contains("no `format`"));
    assert_eq!(problem("header-only"), "no entries after the header");
    assert!(rows.iter().all(|row| row.last_active_at.is_none()));
}

#[test]
fn rows_are_ordered_by_last_activity_with_problems_last() {
    let root = Root::new("order");
    let mut oldest = root.session("a-oldest");
    tick();
    drop(root.session("b-middle"));
    root.dir("c-broken");
    tick();
    // Appending to the oldest log makes it the most recent, whatever its
    // creation or file order.
    append_user(&mut oldest);
    drop(oldest);

    let rows = Inventory::new().refresh(&root.0);
    let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["a-oldest", "b-middle", "c-broken"]);
}

#[test]
fn a_root_without_a_session_base_has_no_rows() {
    let root = Root::new("bare");
    std::fs::remove_dir_all(root.0.join(".efference")).unwrap();
    assert!(Inventory::new().refresh(&root.0).is_empty());
}

#[test]
fn a_refresh_rescans_only_the_log_that_grew() {
    let root = Root::new("cache");
    drop(root.session("one"));
    let mut two = root.session("two");
    drop(root.session("three"));
    let inventory = Inventory::new();

    inventory.refresh(&root.0);
    assert_eq!(inventory.scans(), 3);
    inventory.refresh(&root.0);
    assert_eq!(inventory.scans(), 3, "nothing changed");

    title(&mut two, "Appended");
    let rows = inventory.refresh(&root.0);
    assert_eq!(inventory.scans(), 4, "only the grown log was read again");
    assert_eq!(row(&rows, "two").title.as_deref(), Some("Appended"));

    drop(two);
    std::fs::remove_dir_all(root.0.join(".efference/one")).unwrap();
    let rows = inventory.refresh(&root.0);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        inventory.cache.lock().unwrap().len(),
        2,
        "the gone dir is dropped"
    );
    assert_eq!(inventory.scans(), 4);
}

fn resumed(log: &mut Log) {
    tick();
    log.append(0, Payload::Resumed(crate::record::log::Resumed::new(1)))
        .unwrap();
}

#[test]
fn the_last_lifecycle_entry_says_whether_a_session_was_interrupted() {
    let root = Root::new("lifecycle");
    let mut never = root.session("never");
    title(&mut never, "never");
    let mut archived = root.session("archived");
    tick();
    archived.append(0, Payload::archived(None)).unwrap();
    let mut reopened = root.session("reopened");
    tick();
    reopened
        .append(0, Payload::archived(Some("web".into())))
        .unwrap();
    resumed(&mut reopened);
    let mut again = root.session("again");
    resumed(&mut again);
    tick();
    again
        .append(0, Payload::archived(Some("mcp[claude-code]".into())))
        .unwrap();
    // A title after the archive does not hide it.
    title(&mut again, "late title");
    drop((never, archived, reopened, again));

    let rows = Inventory::new().refresh(&root.0);
    assert!(row(&rows, "never").interrupted);
    assert!(!row(&rows, "archived").interrupted);
    assert!(row(&rows, "reopened").interrupted);
    assert_eq!(row(&rows, "archived").archived_by, None);
    assert_eq!(
        row(&rows, "reopened").archived_by,
        None,
        "a resume after the archive leaves nobody archiving it"
    );
    let again = row(&rows, "again");
    assert!(!again.interrupted);
    assert_eq!(again.archived_by.as_deref(), Some("mcp[claude-code]"));
    assert_eq!(again.title.as_deref(), Some("late title"));
}

#[test]
fn a_lifecycle_word_inside_text_is_not_a_lifecycle_entry() {
    let root = Root::new("lifecycle-text");
    let mut log = root.session("quoted");
    log.append(0, Payload::user("\"Archived\" {\"Resumed\""))
        .unwrap();
    drop(log);

    let rows = Inventory::new().refresh(&root.0);
    assert!(row(&rows, "quoted").interrupted);
}
