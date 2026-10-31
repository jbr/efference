//! Line 1: what a header carries, and the files a strict reader must refuse.

use super::*;
use crate::log::{LoggedInference, TriageModel};

/// The header is line 1, it is not an entry, and the entries behind it are
/// numbered exactly as they would be without it.
#[test]
fn the_header_is_line_one_and_takes_no_seq() {
    let path = temp_path("header-shape");

    let mut log = Log::create(&path, None).unwrap();
    sample(&mut log);
    drop(log);

    let raw = std::fs::read_to_string(&path).unwrap();
    let mut lines = raw.lines();
    let first = lines.next().unwrap();
    // A strict parse: a header with any other format or version is refused.
    let _: LogHeader = serde_json::from_str(first).expect("line 1 is a header");
    assert_eq!(
        lines.count(),
        3,
        "the header is not one of the three entries"
    );

    let loaded = load(&path).unwrap();
    let seqs: Vec<u64> = loaded.iter().map(|entry| entry.seq).collect();
    assert_eq!(
        seqs,
        vec![0, 1, 2],
        "entries start at 0: the header consumes no seq"
    );

    std::fs::remove_file(&path).unwrap();
}

/// The header round-trips the whole way: what boot passed to `create` reads
/// back equal, so a resumed session can be described from its own log.
#[test]
fn session_metadata_round_trips_through_the_header() {
    let path = temp_path("header-metadata");
    let metadata = SessionMetadata {
        upstream: vec!["https://example.test/one".into(), "other".into()],
        temperature: Some(70),
        effort: Some("high".into()),
        created_by: Some("quiet-otter".into()),
        succeeds: Some("brave-heron".into()),
        autonomous: true,
        keep_scratch: true,
        detached: true,
        ..SessionMetadata::new(
            "swift-lynx",
            "/work/project",
            LoggedInference::new("main-conn", crate::log::Provider::Anthropic, "claude"),
            LoggedInference::new("spot-conn", crate::log::Provider::OpenRouter, "flash"),
            TriageModel::separate(LoggedInference::new(
                "triage-conn",
                crate::log::Provider::Anthropic,
                "claude-fast",
            )),
        )
    };

    Log::create(&path, Some(metadata.clone())).unwrap();

    let recording = load_recording(&path).unwrap();
    assert_eq!(
        recording.header.metadata(),
        Some(&metadata),
        "every field a session records must read back"
    );
    assert!(recording.entries.is_empty());

    std::fs::remove_file(&path).unwrap();
}

/// `Separate` triage carries the reasoning settings that inference is called
/// with; `Shared` says triage ran on the main model. Both spellings survive.
#[test]
fn the_triage_choice_and_its_settings_round_trip() {
    let shared = SessionMetadata::new(
        "a",
        "/w",
        LoggedInference::new("c", crate::log::Provider::Anthropic, "m"),
        LoggedInference::new("c", crate::log::Provider::Anthropic, "m"),
        TriageModel::shared(),
    );
    let mut separate = shared.clone();
    separate.triage = TriageModel::Separate {
        selection: LoggedInference::new("triage", crate::log::Provider::Anthropic, "small"),
        thinking: true,
        auto_choice: true,
        effort: Some("low".into()),
    };
    for metadata in [shared, separate] {
        let line = serde_json::to_string(&LogHeader::new(Some(metadata.clone()))).unwrap();
        let header: LogHeader = serde_json::from_str(&line).unwrap();
        assert_eq!(header.metadata(), Some(&metadata));
    }
}

/// Metadata that is unset is skipped rather than written as nulls, so a log
/// records what was configured and nothing else — and a fixture's header with
/// no metadata reads back as having none.
#[test]
fn unset_metadata_fields_are_skipped() {
    let metadata = SessionMetadata::new(
        "swift-lynx",
        "/work",
        LoggedInference::new("c", crate::log::Provider::Anthropic, "m"),
        LoggedInference::new("c", crate::log::Provider::Anthropic, "m"),
        TriageModel::shared(),
    );
    let line = serde_json::to_string(&LogHeader::new(Some(metadata.clone()))).unwrap();
    for absent in [
        "working",
        "temperature",
        "effort",
        "upstream",
        "created_by",
        "succeeds",
        "autonomous",
        "keep_scratch",
        "detached",
    ] {
        assert!(
            !line.contains(absent),
            "{absent} is unset and should not be written: {line}"
        );
    }
    let header: LogHeader = serde_json::from_str(&line).unwrap();
    assert_eq!(header.metadata(), Some(&metadata));
    assert_eq!(
        serde_json::from_str::<LogHeader>(&bare_header())
            .unwrap()
            .metadata(),
        None
    );
}

/// A worktree session records its working directory beside the project (`cwd`); the
/// field survives a round trip, and a header without one reads as the two being the same.
#[test]
fn a_working_directory_apart_from_the_project_round_trips() {
    let mut metadata = SessionMetadata::new(
        "swift-lynx",
        "/work/project",
        LoggedInference::new("c", crate::log::Provider::Anthropic, "m"),
        LoggedInference::new("c", crate::log::Provider::Anthropic, "m"),
        TriageModel::shared(),
    );
    metadata.working = Some("/work/project/.efference/worktrees/swift-lynx".into());
    let line = serde_json::to_string(&LogHeader::new(Some(metadata.clone()))).unwrap();
    assert!(
        line.contains(r#""working":"/work/project/.efference/worktrees/swift-lynx""#),
        "{line}"
    );
    let header: LogHeader = serde_json::from_str(&line).unwrap();
    assert_eq!(header.metadata(), Some(&metadata));
    assert_eq!(header.metadata().unwrap().cwd, Path::new("/work/project"));
}

/// A file whose first line is an entry is an unversioned log — a file this
/// build cannot promise to read — so it is refused with that said plainly,
/// rather than read on the chance that an entry happens to parse.
#[test]
fn rejects_a_file_whose_first_line_is_an_entry() {
    let path = temp_path("unversioned");
    std::fs::write(&path, format!("{}\n", user_entry(0))).unwrap();

    // Not `Corrupt`: an inventory reports this one as *not a versioned log*,
    // which is a different thing from a versioned log that is damaged.
    assert!(
        matches!(load_error(&path), LoadError::Unversioned { .. }),
        "{:?}",
        load_error(&path)
    );

    std::fs::remove_file(&path).unwrap();
}

/// An empty file is not a log with an empty header; it has no header.
#[test]
fn rejects_an_empty_file() {
    let path = temp_path("empty");
    std::fs::write(&path, "").unwrap();
    assert!(matches!(
        load_error(&path),
        LoadError::Corrupt { line: 1, .. }
    ));
    std::fs::remove_file(&path).unwrap();
}

/// A file there is not enough to be a log: line 1 has to be a header object,
/// and a file that is not one is damage rather than a version to try.
#[test]
fn rejects_a_first_line_that_is_not_an_object() {
    let path = temp_path("not-a-header");
    std::fs::write(&path, "[1,2,3]\n").unwrap();

    assert!(matches!(
        load_error(&path),
        LoadError::Corrupt { line: 1, .. }
    ));
    assert!(
        load_error(&path)
            .to_string()
            .contains("the header line is not a JSON object"),
        "{}",
        load_error(&path)
    );

    std::fs::remove_file(&path).unwrap();
}

/// A version this build does not know is refused *before* its entries are read,
/// carrying the version it found so an inventory can say which file is newer
/// than the build that would read it.
#[test]
fn rejects_an_unknown_future_version() {
    let path = temp_path("future");
    let future = u64::from(LogHeader::VERSION) + 1;
    std::fs::write(
        &path,
        format!(
            "{{\"format\":\"{}\",\"version\":{future},\"metadata\":null}}\n",
            LogHeader::FORMAT
        ),
    )
    .unwrap();

    match load_error(&path) {
        LoadError::UnsupportedVersion { version, .. } => assert_eq!(version, future),
        other => panic!("expected an unsupported version, got {other:?}"),
    }
    let message = load_error(&path).to_string();
    assert!(
        message.contains(&format!("unsupported log version {future}")),
        "{message}"
    );
    assert!(
        message.contains(&format!("reads version {}", LogHeader::VERSION)),
        "{message}"
    );

    std::fs::remove_file(&path).unwrap();
}

/// The version is settled before the metadata is read into this version's
/// types, so a newer file's incompatible metadata cannot report a missing
/// field instead of the version that is the actual reason to stop.
#[test]
fn an_unsupported_version_outranks_its_metadata() {
    let path = temp_path("future-metadata");
    let future = u64::from(LogHeader::VERSION) + 2;
    std::fs::write(
        &path,
        format!(
            "{{\"format\":\"{}\",\"version\":{future},\"metadata\":{{\"id\":7,\"unknown\":true}}}}\n",
            LogHeader::FORMAT
        ),
    )
    .unwrap();

    let error = load_error(&path);
    assert!(
        matches!(error, LoadError::UnsupportedVersion { version, .. } if version == future),
        "the version is the reason to stop: {error:?}"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A different `format` is a different file type, not a version of this one,
/// and the name it was found under is what says which.
#[test]
fn rejects_a_foreign_format() {
    let path = temp_path("foreign");
    std::fs::write(&path, "{\"format\":\"usage-jsonl\",\"version\":1}\n").unwrap();

    match load_error(&path) {
        LoadError::WrongFormat { found, .. } => assert_eq!(found, "usage-jsonl"),
        other => panic!("expected a foreign format, got {other:?}"),
    }

    std::fs::remove_file(&path).unwrap();
}

/// A header naming this format but no version is not a header this build can
/// act on; it fails rather than defaulting to the current version.
#[test]
fn rejects_a_header_with_no_version() {
    let path = temp_path("no-version");
    std::fs::write(&path, format!("{{\"format\":\"{}\"}}\n", LogHeader::FORMAT)).unwrap();

    assert!(matches!(
        load_error(&path),
        LoadError::Corrupt { line: 1, .. }
    ));
    assert!(load_error(&path).to_string().contains("no `version`"));

    std::fs::remove_file(&path).unwrap();
}

/// Metadata that is not the shape this version describes is a bad header,
/// reported as damage at line 1 rather than as a file with no metadata.
#[test]
fn rejects_unreadable_metadata() {
    let path = temp_path("bad-metadata");
    std::fs::write(
        &path,
        format!(
            "{{\"format\":\"{}\",\"version\":{},\"metadata\":{{\"id\":7}}}}\n",
            LogHeader::FORMAT,
            LogHeader::VERSION
        ),
    )
    .unwrap();

    let error = load_error(&path);
    assert!(
        matches!(error, LoadError::Corrupt { line: 1, .. }),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("unreadable session metadata"),
        "{error}"
    );

    std::fs::remove_file(&path).unwrap();
}

/// The header is on disk before `create` returns, so a log that exists is one a
/// reader can open even with no entries in it yet.
#[test]
fn create_flushes_the_header_before_returning() {
    let path = temp_path("flushed");
    let mut log = Log::create(&path, None).unwrap();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{}\n", bare_header()),
        "the header line is on disk while the log is still open"
    );
    assert!(load(&path).unwrap().is_empty());

    log.append(0, Payload::User(vec![])).unwrap();
    drop(log);
    std::fs::remove_file(&path).unwrap();
}
