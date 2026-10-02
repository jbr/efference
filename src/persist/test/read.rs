//! Reading: what a strict, read-only loader refuses, what it reports, and the
//! promise that it changes nothing.

use super::*;

/// Creation is exclusive, so a second run cannot append to a file that already
/// numbers its entries — and the refusal leaves the file as it found it.
#[test]
fn create_refuses_an_existing_file_without_changing_it() {
    let path = temp_path("exclusive");
    let existing = "a prior log\n";
    std::fs::write(&path, existing).unwrap();

    let error = match Log::create(&path, None) {
        Ok(_) => panic!("create must refuse a file that already exists"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        error.to_string().contains(&path.display().to_string()),
        "the refusal names the file it refused: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        existing,
        "the refused file is byte-for-byte untouched"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A load is a read. Neither a clean load nor a refused one may move a byte,
/// since the same log is read again by inventory, by `view`, and by the TUI.
#[test]
fn loading_never_changes_the_bytes() {
    let path = temp_path("readonly");
    let mut log = Log::create(&path, None).unwrap();
    sample(&mut log);
    drop(log);
    let before = std::fs::read(&path).unwrap();

    load(&path).unwrap();
    load_recording(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // And a file this reader refuses is refused without being repaired.
    std::fs::write(&path, format!("{HEADER}\n{}\nnonsense\n", user_entry(0))).unwrap();
    let corrupt = std::fs::read(&path).unwrap();
    assert!(load(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);

    std::fs::remove_file(&path).unwrap();
}

/// Every line after the header is one entry, so a blank one is a hole in a
/// journal rather than whitespace to step over.
#[test]
fn a_blank_line_is_refused() {
    let path = temp_path("blank");
    std::fs::write(&path, format!("{HEADER}\n{}\n\n", user_entry(0))).unwrap();

    let error = load_error(&path);
    assert!(error.contains("blank line"), "{error}");
    assert!(error.contains(":3:"), "the line is named: {error}");

    std::fs::remove_file(&path).unwrap();
}

/// An interior line that is not an entry is damage, and is loud about it. A
/// blank first line is separately caught as a header that is not there.
#[test]
fn a_malformed_interior_line_is_refused() {
    let path = temp_path("malformed");
    std::fs::write(
        &path,
        format!("{HEADER}\n{}\n{{\"seq\":1,\"turn\":0\n", user_entry(0)),
    )
    .unwrap();

    let error = load_error(&path);
    assert!(error.contains("malformed entry"), "{error}");
    assert!(error.contains(":3:"), "the line is named: {error}");

    std::fs::remove_file(&path).unwrap();
}

/// `seq` is the address space `project` reads over, so an entry sitting at an
/// index other than its `seq` means the file is not the sequence it claims.
#[test]
fn a_duplicated_seq_is_refused() {
    let path = temp_path("duplicate-seq");
    std::fs::write(
        &path,
        format!("{HEADER}\n{}\n{}\n", user_entry(0), user_entry(0)),
    )
    .unwrap();

    let error = load_error(&path);
    assert!(error.contains("seq 0 but sits at index 1"), "{error}");
    assert!(error.contains(":3:"), "the line is named: {error}");

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_gapped_seq_is_refused() {
    let path = temp_path("gapped-seq");
    std::fs::write(
        &path,
        format!("{HEADER}\n{}\n{}\n", user_entry(0), user_entry(2)),
    )
    .unwrap();

    let error = load_error(&path);
    assert!(error.contains("seq 2 but sits at index 1"), "{error}");

    std::fs::remove_file(&path).unwrap();
}

/// A crash between writing an entry and writing its newline leaves an object
/// that is complete and in order: it loads as the entry it is, and the missing
/// terminator is reported so a writer knows to finish the line.
#[test]
fn a_complete_final_entry_without_its_newline_is_an_entry() {
    let path = temp_path("missing-newline");
    std::fs::write(&path, format!("{HEADER}\n{}", user_entry(0))).unwrap();

    let recording = load_recording(&path).unwrap();
    assert_eq!(recording.entries.len(), 1);
    assert_eq!(recording.entries[0].seq, 0);
    assert_eq!(
        recording.trailing,
        Some(Trailing::MissingNewline {
            byte_offset: HEADER.len() as u64 + 1,
        }),
        "the entry is usable and the absent newline is reported"
    );
    assert!(recording.is_torn());

    std::fs::remove_file(&path).unwrap();
}

/// An append interrupted mid-object leaves bytes that are not an entry. They
/// are reported where they start — for the entries behind them to stand on —
/// and the file is left exactly as the writer left it.
#[test]
fn an_unfinished_final_entry_is_reported_as_trailing_bytes() {
    let path = temp_path("partial");
    let torn = r#"{"seq":1,"turn":0,"at":1,"pay"#;
    std::fs::write(&path, format!("{HEADER}\n{}\n{torn}", user_entry(0))).unwrap();
    let before = std::fs::read(&path).unwrap();

    let recording = load_recording(&path).unwrap();
    assert_eq!(recording.entries.len(), 1);
    assert_eq!(
        recording.trailing,
        Some(Trailing::PartialEntry {
            byte_offset: (HEADER.len() + 1 + user_entry(0).len() + 1) as u64,
            bytes: torn.as_bytes().to_vec(),
        })
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "reporting the tail must not touch it"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A complete object that is *not* an entry is damage, not a torn write: the
/// parse ran out of nothing, it ran out of input only in the case above. An
/// unknown payload shape with no closing newline is still an error.
#[test]
fn a_complete_object_that_is_not_an_entry_is_damage_even_unterminated() {
    let path = temp_path("not-an-entry");
    let not_an_entry = r#"{"seq":0,"turn":0,"at":1,"payload":{"NoSuchPayload":[]}}"#;
    std::fs::write(&path, format!("{HEADER}\n{not_an_entry}")).unwrap();

    let error = load_error(&path);
    assert!(error.contains("malformed entry"), "{error}");

    std::fs::remove_file(&path).unwrap();
    let path = temp_path("missing-field");
    std::fs::write(
        &path,
        format!(
            r#"{HEADER}
{{"seq":0,"turn":0,"payload":{{"User":[{{"Text":"hi"}}]}}}}"#
        ),
    )
    .unwrap();
    assert!(
        load_error(&path).contains("malformed entry"),
        "{}",
        load_error(&path)
    );

    std::fs::remove_file(&path).unwrap();
}

/// The tear can land inside a multi-byte codepoint, which is not text at all.
/// Those bytes are reported so the entries in front of them still load.
#[test]
fn a_tail_cut_through_a_codepoint_is_reported_not_fatal() {
    let path = temp_path("codepoint");
    let mut bytes = format!("{HEADER}\n{}\n", user_entry(0)).into_bytes();
    let start = bytes.len();
    bytes.extend_from_slice(&[0xe2, 0x98]); // the first two bytes of "☃"
    std::fs::write(&path, &bytes).unwrap();

    let recording = load_recording(&path).unwrap();
    assert_eq!(recording.entries.len(), 1);
    assert_eq!(
        recording.trailing,
        Some(Trailing::PartialEntry {
            byte_offset: start as u64,
            bytes: vec![0xe2, 0x98],
        })
    );

    std::fs::remove_file(&path).unwrap();
}

/// `load` is the entries-only view, so a reader that wants the record (to say a
/// log is torn, or to keep the bytes) asks for the whole recording.
#[test]
fn load_drops_the_tail_that_load_recording_reports() {
    let path = temp_path("dropped-tail");
    std::fs::write(
        &path,
        format!("{HEADER}\n{}\n{{\"seq\":1,\"turn\":", user_entry(0)),
    )
    .unwrap();

    assert_eq!(load(&path).unwrap().len(), 1);
    assert!(load_recording(&path).unwrap().is_torn());

    std::fs::remove_file(&path).unwrap();
}

/// A log nobody was interrupted on ends on its last newline, and reports
/// nothing.
#[test]
fn a_log_written_to_the_end_is_not_torn() {
    let path = temp_path("whole");
    let mut log = Log::create(&path, None).unwrap();
    sample(&mut log);
    drop(log);

    let recording = load_recording(&path).unwrap();
    assert_eq!(recording.entries.len(), 3);
    assert_eq!(recording.trailing, None);
    assert!(!recording.is_torn());

    std::fs::remove_file(&path).unwrap();
}
