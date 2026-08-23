use super::*;

#[test]
fn register_mints_ids_and_tails_recent_output() {
    let mut s = Supervisor::new();
    let id = s.register_background("cargo watch".into(), Some(123));
    assert_eq!(id, 1);
    assert_eq!(handle(id), "p#1");
    s.push_chunk(id, b"line one\nline two\n");

    let views = s.views();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].command, "cargo watch");
    assert_eq!(views[0].handle, "p#1");
    assert!(views[0].tail.contains("line two"), "{:?}", views[0].tail);
}

#[test]
fn handle_round_trips_and_rejects_anything_else() {
    assert_eq!(parse_handle(&handle(3)), Some(3));
    assert_eq!(parse_handle("p#12"), Some(12));
    for bad in ["bg#3", "p#", "p#x", "3", "p#3x", ""] {
        assert_eq!(parse_handle(bad), None, "{bad:?}");
    }
}

#[test]
fn max_handle_mention_over_matches_safely() {
    assert_eq!(max_handle_mention("backgrounded as p#3. p#12 exited"), 12);
    assert_eq!(max_handle_mention("no handles here, bg#9 is legacy"), 0);
    assert_eq!(max_handle_mention("p# p#x p#"), 0);
}

#[test]
fn with_minted_starts_past_the_seed() {
    let mut s = Supervisor::with_minted(4);
    assert_eq!(
        s.register_background("x".into(), None),
        5,
        "never re-mints a logged id"
    );
}

#[test]
fn mute_drops_from_the_peek_but_keeps_it_running() {
    let mut s = Supervisor::new();
    let id = s.register_background("x".into(), Some(7));
    s.set_muted(id, true);
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
    let id = s.register_background("x".into(), Some(7));
    s.push_chunk(id, b"done\n");
    let (source, body) = s.finish(id, None).unwrap();
    assert_eq!(source, "p#1");
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
    let id = s.register_background("npm test".into(), None);
    s.push_chunk(id, b"PASS\n");
    let (source, body) = s.pin_snapshot(id).unwrap();
    assert_eq!(source, "p#1");
    assert!(body.contains("npm test") && body.contains("PASS"), "{body}");
}

#[test]
fn handoff_flips_the_property_and_reads_span_the_whole_run() {
    let mut s = Supervisor::new();
    let id = s.register_foreground("cargo test".into(), Some(9));
    s.push_chunk(id, b"pre-detach output\n");
    assert!(s.make_background(id), "the entry exists to flip");
    s.push_chunk(id, b"post-detach output\n");
    let read = s.read_since(id, None).unwrap();
    assert!(
        read.contains("pre-detach output") && read.contains("post-detach output"),
        "the flip keeps the capture — one stream across the handoff: {read}"
    );
    assert_eq!(
        s.pgid_of(id),
        Some(9),
        "adopted child is killable/teardown-able"
    );
}

#[test]
fn foreground_is_listed_but_kept_out_of_model_views() {
    let mut s = Supervisor::new();
    let id = s.register_foreground("cargo build".into(), Some(11));
    let states = s.process_states();
    assert_eq!(states.len(), 1, "addressable and listed from spawn");
    assert!(!states[0].background);
    assert!(
        s.views().is_empty(),
        "the model is not told about its own in-flight command"
    );
    assert!(s.list_text().contains("No background processes"));
    s.remove(id);
    assert!(
        s.process_states().is_empty(),
        "a completed foreground command leaves the registry"
    );
}

#[test]
fn front_drop_is_counted_not_silent() {
    let mut s = Supervisor::new();
    let id = s.register_background("noisy".into(), None);
    // Overflow the cap so the front is dropped.
    let chunk = vec![b'.'; BUFFER_CAP + 10];
    s.push_chunk(id, &chunk);
    s.push_chunk(id, b"\nlatest\n");
    let tail = &s.views()[0].tail;
    assert!(tail.contains("earlier bytes dropped"), "{tail}");
    assert!(tail.contains("latest"), "{tail}");
}

#[test]
fn empty_capture_reads_as_no_output_yet() {
    let mut s = Supervisor::new();
    s.register_background("slow".into(), None);
    assert!(s.views()[0].tail.contains("no output yet"));
}

#[test]
fn list_text_shows_running_and_marks_muted() {
    let mut s = Supervisor::new();
    s.register_background("cargo watch".into(), Some(1));
    let b = s.register_background("sleep 9".into(), Some(2));
    s.set_muted(b, true);
    let text = s.list_text();
    assert!(text.contains("[p#1] cargo watch"), "{text}");
    assert!(
        text.contains("[p#2] sleep 9") && text.contains("(muted)"),
        "{text}"
    );
}

#[test]
fn process_states_carry_id_and_pid() {
    let mut s = Supervisor::new();
    s.register_background("cargo watch".into(), Some(4242));
    let states = s.process_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].id, 1);
    assert_eq!(states[0].pid, Some(4242), "the pid is an attribute");
}

#[test]
fn read_since_advances_a_cursor_and_reports_no_new() {
    let mut s = Supervisor::new();
    let id = s.register_background("x".into(), None);
    s.push_chunk(id, b"hello world\n");
    let first = s.read_since(id, None).unwrap();
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
        s.read_since(id, Some(cursor))
            .unwrap()
            .contains("no new output")
    );
    // More output → only the delta comes back from that cursor.
    s.push_chunk(id, b"more\n");
    let third = s.read_since(id, Some(cursor)).unwrap();
    assert!(
        third.contains("more") && !third.contains("hello"),
        "{third}"
    );
}

#[test]
fn read_since_unknown_id_is_none() {
    assert!(Supervisor::new().read_since(99, None).is_none());
}

#[test]
fn pgid_of_only_running() {
    let mut s = Supervisor::new();
    let id = s.register_background("x".into(), Some(42));
    assert_eq!(s.pgid_of(id), Some(42));
    s.finish(id, None);
    assert_eq!(s.pgid_of(id), None, "exited → no kill target");
}

/// The family's briefs (docs/permissions.md): every supervisor/inbox tool the loop
/// dispatches structurally answers `brief` — the spotter once judged these as bare
/// names because only the providers were consulted — and the annotations carry the
/// harness's own claims: reads are reads, `kill_process` is the destructive member.
#[test]
fn every_supervisor_tool_has_a_brief_with_honest_annotations() {
    for name in [LIST, READ, KILL, CHECK_INBOX] {
        let b = brief(name).unwrap_or_else(|| panic!("no brief for {name}"));
        assert!(b.server.is_none(), "{name} is the harness's own");
        let hints = b.definition.annotations.as_ref().unwrap();
        assert_eq!(
            hints.open_world_hint,
            Some(false),
            "{name} is session-local"
        );
        assert_eq!(
            hints.read_only_hint,
            Some(name != KILL),
            "{name}: only kill_process mutates"
        );
    }
    assert_eq!(
        brief(KILL)
            .unwrap()
            .definition
            .annotations
            .as_ref()
            .unwrap()
            .destructive_hint,
        Some(true)
    );
    assert!(brief("bash").is_none());
}

/// The wire entries stay in the provider shape — name/description/input_schema only,
/// sorted keys; annotations have no wire slot and must not leak into `tools[]`
/// (docs/log-and-projection.md byte stability).
#[test]
fn wire_definitions_carry_no_annotations() {
    let defs = tool_definitions();
    let names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec![LIST, READ, KILL, CHECK_INBOX]);
    for def in &defs {
        let keys: Vec<&str> = def
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["description", "input_schema", "name"], "{def}");
    }
}
