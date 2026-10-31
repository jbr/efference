use super::*;

/// The names and modes are what sessions already on disk were written with, and what
/// the web client's base derivation mirrors; a change here orphans existing logs.
#[test]
fn the_names_and_modes_on_disk_are_fixed() {
    assert_eq!(SESSION_BASE, ".efference");
    assert_eq!(LOG_NAME, "log.jsonl");
    assert_eq!(DIR_MODE, 0o700);
    assert_eq!(FILE_MODE, 0o600);
}

/// Ids are validated before they are joined onto a path: only a plain segment names
/// a session.
#[test]
fn only_plain_ids_name_a_session() {
    assert!(plain_id("work-1.2"));
    for bad in ["", ".", "..", "a/b", "/etc", "a b", "wörk"] {
        assert!(!plain_id(bad), "{bad:?} passed as a session id");
    }
}
