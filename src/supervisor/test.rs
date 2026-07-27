
use super::*;

#[test]
fn register_mints_handles_and_tails_recent_output() {
    let mut s = Supervisor::new();
    let h = s.register("cargo watch".into(), Some(123));
    assert_eq!(h, "bg#1");
    s.push_chunk(&h, b"line one\nline two\n");

    let views = s.views();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].command, "cargo watch");
    assert!(views[0].tail.contains("line two"), "{:?}", views[0].tail);
}

#[test]
fn mute_drops_from_the_peek_but_keeps_it_running() {
    let mut s = Supervisor::new();
    let h = s.register("x".into(), Some(7));
    s.mute(&h);
    assert!(s.views().is_empty(), "muted → off the ambient show");
    assert_eq!(
        s.running_pgids(),
        vec![7],
        "muted ≠ killed: still in the teardown set"
    );
}

#[test]
fn finish_marks_exited_summarizes_and_leaves_the_show() {
    let mut s = Supervisor::new();
    let h = s.register("x".into(), Some(7));
    s.push_chunk(&h, b"done\n");
    let (source, body) = s.finish(&h, None).unwrap();
    assert_eq!(source, "bg#1");
    assert!(body.contains("done"), "exit carries the final tail: {body}");
    assert!(s.views().is_empty(), "exited → off the ambient show");
    assert!(
        s.running_pgids().is_empty(),
        "exited → out of the teardown set"
    );
}

#[test]
fn pin_snapshot_carries_command_and_current_tail() {
    let mut s = Supervisor::new();
    let h = s.register("npm test".into(), None);
    s.push_chunk(&h, b"PASS\n");
    let (source, body) = s.pin_snapshot(&h).unwrap();
    assert_eq!(source, "bg#1");
    assert!(body.contains("npm test") && body.contains("PASS"), "{body}");
}

#[test]
fn adopt_seeds_the_capture_and_reads_span_the_whole_run() {
    let mut s = Supervisor::new();
    let h = s.adopt("cargo test".into(), Some(9), b"pre-detach output\n");
    s.push_chunk(&h, b"post-detach output\n");
    let read = s.read_since(&h, None).unwrap();
    assert!(
        read.contains("pre-detach output") && read.contains("post-detach output"),
        "seed and live bytes are one stream: {read}"
    );
    assert_eq!(
        s.pgid_of(&h),
        Some(9),
        "adopted child is killable/teardown-able"
    );
}

#[test]
fn front_drop_is_counted_not_silent() {
    let mut s = Supervisor::new();
    let h = s.register("noisy".into(), None);
    // Overflow the cap so the front is dropped.
    let chunk = vec![b'.'; BUFFER_CAP + 10];
    s.push_chunk(&h, &chunk);
    s.push_chunk(&h, b"\nlatest\n");
    let tail = &s.views()[0].tail;
    assert!(tail.contains("earlier bytes dropped"), "{tail}");
    assert!(tail.contains("latest"), "{tail}");
}

#[test]
fn empty_capture_reads_as_no_output_yet() {
    let mut s = Supervisor::new();
    s.register("slow".into(), None);
    assert!(s.views()[0].tail.contains("no output yet"));
}

#[test]
fn list_text_shows_running_and_marks_muted() {
    let mut s = Supervisor::new();
    s.register("cargo watch".into(), Some(1));
    let b = s.register("sleep 9".into(), Some(2));
    s.mute(&b);
    let text = s.list_text();
    assert!(text.contains("[bg#1] cargo watch"), "{text}");
    assert!(
        text.contains("[bg#2] sleep 9") && text.contains("(muted)"),
        "{text}"
    );
}

#[test]
fn read_since_advances_a_cursor_and_reports_no_new() {
    let mut s = Supervisor::new();
    let h = s.register("x".into(), None);
    s.push_chunk(&h, b"hello world\n");
    let first = s.read_since(&h, None).unwrap();
    assert!(first.contains("hello world"), "{first}");
    // Pull the cursor out of the footer and read again — nothing new.
    let cursor: u64 = first
        .rsplit("cursor: ")
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        s.read_since(&h, Some(cursor))
            .unwrap()
            .contains("no new output")
    );
    // More output → only the delta comes back from that cursor.
    s.push_chunk(&h, b"more\n");
    let third = s.read_since(&h, Some(cursor)).unwrap();
    assert!(
        third.contains("more") && !third.contains("hello"),
        "{third}"
    );
}

#[test]
fn read_since_unknown_handle_is_none() {
    assert!(Supervisor::new().read_since("bg#99", None).is_none());
}

#[test]
fn pgid_of_only_running() {
    let mut s = Supervisor::new();
    let h = s.register("x".into(), Some(42));
    assert_eq!(s.pgid_of(&h), Some(42));
    s.finish(&h, None);
    assert_eq!(s.pgid_of(&h), None, "exited → no kill target");
}
