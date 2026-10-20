//! Reopening: the writer lock, the tail repair, and what an open refuses to
//! write (docs/log-and-projection.md).

use super::*;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use crate::env_table::Masker;

/// Where a dropped fragment is kept, computed the way an open does — so a test
/// can clear a leftover from an earlier run, or leave a decoy there.
fn fragment_path(log: &std::path::Path, byte_offset: u64) -> PathBuf {
    let mut name = log.as_os_str().to_os_string();
    name.push(format!(".torn-{byte_offset}"));
    PathBuf::from(name)
}

/// A log with a live writer is a session in use, not one waiting to be
/// continued: the second writer is refused by name, and reopening it once that
/// writer is gone is what continuing one means.
#[test]
fn open_refuses_a_log_another_writer_holds() {
    let path = temp_path("held");
    let first = Log::create(&path, None).unwrap();

    let error = match Log::open(&path) {
        Ok(_) => panic!("a log with a live writer must not open"),
        Err(error) => error,
    };
    assert!(matches!(error, OpenError::Held { .. }), "{error:?}");
    assert!(
        error.to_string().contains(&path.display().to_string()),
        "the refusal names the log: {error}"
    );

    drop(first);
    let reopened = Log::open(&path).expect("the lock is released with the writer");
    assert!(reopened.repaired.is_none());

    drop(reopened);
    std::fs::remove_file(&path).unwrap();
}

/// Two opens of one log at the same moment: the lock is taken atomically per
/// handle, so exactly one of them owns the file and the other is told rather
/// than queued.
#[test]
fn two_opens_race_and_exactly_one_wins() {
    let path = temp_path("race");
    drop(Log::create(&path, None).unwrap());

    let rendezvous = Arc::new(Barrier::new(2));
    let attempts: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let rendezvous = Arc::clone(&rendezvous);
            std::thread::spawn(move || {
                rendezvous.wait();
                Log::open(&path)
            })
        })
        .collect();
    // Held until both threads are done, so the loser cannot be the winner that
    // already let go.
    let outcomes: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect();

    let (opened, refused): (Vec<_>, Vec<_>) = outcomes.iter().partition(|o| o.is_ok());
    assert_eq!(opened.len(), 1, "exactly one open owns the log");
    for outcome in refused {
        let Err(error) = outcome else {
            panic!("both opens succeeded: two writers on one log");
        };
        assert!(matches!(error, OpenError::Held { .. }), "{error}");
    }

    drop(outcomes);
    std::fs::remove_file(&path).unwrap();
}

/// Opening is for a log that is there. A path with nothing at it is a refusal,
/// not an empty log to write: `create` mints a session, `open` continues one.
#[test]
fn open_refuses_a_missing_file_without_creating_it() {
    let path = temp_path("absent-open");

    let error = match Log::open(&path) {
        Ok(_) => panic!("there is no log at that path to continue"),
        Err(error) => error,
    };
    match error {
        OpenError::Io { error, .. } => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected the missing file itself, got {other:?}"),
    }
    assert!(!path.exists(), "a refused open leaves no file behind");
}

/// The reopened log continues the sequence it read: the next append takes the
/// `seq` after the last entry on disk, and the header it came from is left
/// exactly as it was rather than rewritten by this build.
#[test]
fn appends_after_open_continue_the_sequence() {
    let path = temp_path("continue");
    let mut log = Log::create(&path, None).unwrap();
    sample(&mut log);
    drop(log);
    let before = std::fs::read(&path).unwrap();
    let header = header_line(&path);

    let mut opened = Log::open(&path).unwrap();
    assert_eq!(
        opened.log.entries().len(),
        3,
        "the entries read are in memory"
    );
    let seq = opened
        .log
        .append(1, Payload::User(vec![ContentBlock::Text("after".into())]))
        .unwrap();
    assert_eq!(seq, 3, "the next append is the next seq, not a restart");
    drop(opened);

    let recording = load_recording(&path).unwrap();
    let seqs: Vec<u64> = recording.entries.iter().map(|entry| entry.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3]);
    assert!(recording.trailing.is_none());
    assert_eq!(header_line(&path), header, "the header is not rewritten");
    assert_eq!(
        std::fs::read(&path).unwrap()[..before.len()],
        before[..],
        "what the first writer wrote is byte-for-byte what is still there"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A complete entry whose newline is missing is completed with that one byte:
/// the log becomes byte-exact what a writer that had got that far would have
/// left, and every entry it read is kept.
#[test]
fn a_missing_newline_is_terminated() {
    let path = temp_path("repair-newline");
    let clean = format!("{HEADER}\n{}\n", user_entry(0));
    std::fs::write(&path, format!("{HEADER}\n{}", user_entry(0))).unwrap();

    let opened = Log::open(&path).unwrap();
    assert_eq!(
        opened.repaired,
        Some(Repair::MissingNewline {
            byte_offset: (HEADER.len() + 1) as u64,
        })
    );
    drop(opened);

    assert_eq!(
        std::fs::read(&path).unwrap(),
        clean.as_bytes(),
        "one byte makes it the whole log"
    );
    assert!(!load_recording(&path).unwrap().is_torn());

    std::fs::remove_file(&path).unwrap();
}

/// An append that never became an entry is kept beside the log before the log
/// is cut, so the bytes that were being written survive the repair and the
/// entries behind them stand.
#[test]
fn an_unfinished_append_is_truncated_with_its_bytes_kept_beside() {
    let path = temp_path("repair-torn");
    let torn = r#"{"seq":1,"turn":0,"at":1,"pay"#;
    let clean = format!("{HEADER}\n{}\n", user_entry(0));
    let offset = (HEADER.len() + 1 + user_entry(0).len() + 1) as u64;
    let fragment = fragment_path(&path, offset);
    let _ = std::fs::remove_file(&fragment);
    std::fs::write(&path, format!("{HEADER}\n{}\n{torn}", user_entry(0))).unwrap();

    let opened = Log::open(&path).unwrap();
    let saved_to = match &opened.repaired {
        Some(Repair::PartialEntry {
            byte_offset,
            saved_to,
            bytes,
        }) => {
            assert_eq!(
                *byte_offset, offset,
                "the fragment starts where an entry would have"
            );
            assert_eq!(*bytes, torn.len());
            assert_eq!(
                saved_to.parent(),
                path.parent(),
                "the fragment is kept beside the log"
            );
            assert!(
                saved_to.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with(
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                    )
                }),
                "named for the log it came from: {}",
                saved_to.display()
            );
            assert!(
                saved_to
                    .to_string_lossy()
                    .ends_with(&format!(".torn-{offset}")),
                "with the offset it began at: {}",
                saved_to.display()
            );
            saved_to.clone()
        }
        other => panic!("expected a dropped fragment, got {other:?}"),
    };
    assert_eq!(
        std::fs::read(&saved_to).unwrap(),
        torn.as_bytes(),
        "the dropped bytes are kept exactly as they were written"
    );
    drop(opened);

    assert_eq!(std::fs::read(&path).unwrap(), clean.as_bytes());
    let recording = load_recording(&path).unwrap();
    assert_eq!(recording.entries.len(), 1);
    assert!(recording.trailing.is_none());

    std::fs::remove_file(&path).unwrap();
    std::fs::remove_file(&saved_to).unwrap();
}

/// A fragment already at that name is another repair's, and this one is about
/// to leave the only file that holds its bytes — so the open refuses rather than
/// overwriting, and touches neither file.
#[test]
fn a_fragment_file_from_earlier_is_not_overwritten() {
    let path = temp_path("refuse-fragment");
    let torn = r#"{"seq":1,"turn":0,"at":1,"pay"#;
    let bytes = format!("{HEADER}\n{}\n{torn}", user_entry(0));
    let offset = (HEADER.len() + 1 + user_entry(0).len() + 1) as u64;
    let fragment = fragment_path(&path, offset);
    std::fs::write(&path, &bytes).unwrap();
    let earlier = b"a fragment an earlier repair kept";
    std::fs::write(&fragment, earlier).unwrap();

    let error = match Log::open(&path) {
        Ok(_) => panic!("the log must not be cut while its bytes have somewhere else to go"),
        Err(error) => error,
    };
    assert!(matches!(error, OpenError::Io { .. }), "{error:?}");
    assert_eq!(
        std::fs::read(&fragment).unwrap(),
        earlier,
        "the earlier fragment is not overwritten"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes.as_bytes(),
        "and the log is left as its writer left it"
    );

    std::fs::remove_file(&path).unwrap();
    std::fs::remove_file(&fragment).unwrap();
}

/// The one file at that name which is not another fragment is *this* fragment,
/// left there by a repair interrupted between keeping the bytes and cutting the
/// log. Finishing that repair is the point of opening at all, so it is finished
/// rather than refused.
#[test]
fn a_repair_interrupted_after_keeping_its_bytes_is_finished_not_refused() {
    let path = temp_path("finish-fragment");
    let torn = r#"{"seq":1,"turn":0,"at":1,"pay"#;
    let clean = format!("{HEADER}\n{}\n", user_entry(0));
    let offset = (HEADER.len() + 1 + user_entry(0).len() + 1) as u64;
    let fragment = fragment_path(&path, offset);
    std::fs::write(&path, format!("{HEADER}\n{}\n{torn}", user_entry(0))).unwrap();
    std::fs::write(&fragment, torn).unwrap();

    let opened =
        Log::open(&path).expect("the same bytes are already kept, so this one completes it");
    assert!(matches!(opened.repaired, Some(Repair::PartialEntry { .. })));
    drop(opened);

    assert_eq!(std::fs::read(&path).unwrap(), clean.as_bytes());
    assert_eq!(std::fs::read(&fragment).unwrap(), torn.as_bytes());

    std::fs::remove_file(&path).unwrap();
    std::fs::remove_file(&fragment).unwrap();
}

/// Every class of file this build will not read is refused with the reason and
/// without a byte written: appending to a log whose contents are unknown would
/// be guessing, and guessing at a log is how a sequence gets two of an entry.
#[test]
fn an_unreadable_log_is_refused_with_its_bytes_untouched() {
    let cases: Vec<(&str, String)> = vec![
        ("unversioned", format!("{}\n", user_entry(0))),
        (
            "foreign",
            "{\"format\":\"usage-jsonl\",\"version\":1,\"metadata\":null}\n".to_string(),
        ),
        (
            "future",
            format!(
                "{{\"format\":\"{}\",\"version\":8,\"metadata\":null}}\n",
                LogHeader::FORMAT
            ),
        ),
        (
            "corrupt",
            format!("{HEADER}\n{}\nnonsense\n", user_entry(0)),
        ),
        (
            "sequence",
            format!("{HEADER}\n{}\n{}\n", user_entry(0), user_entry(0)),
        ),
        ("blank-header", format!("\n{}\n", user_entry(0))),
    ];

    for (name, bytes) in cases {
        let path = temp_path(&format!("refused-{name}"));
        std::fs::write(&path, &bytes).unwrap();

        let error = match Log::open(&path) {
            Ok(_) => panic!("{name}: a log this build will not read must not be continued"),
            Err(error) => error,
        };
        assert!(
            matches!(error, OpenError::Load(_)),
            "{name}: the refusal carries the load error: {error:?}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes.as_bytes(),
            "{name}: the refused file is byte-for-byte untouched"
        );

        std::fs::remove_file(&path).unwrap();
    }
}

/// The reopened log is a log like any other: the mask the session installs
/// applies to its appends exactly as it did to the ones before them.
#[test]
fn appends_after_open_are_masked_like_any_other() {
    let path = temp_path("masked");
    let mut log = Log::create(&path, None).unwrap();
    log.append(0, Payload::User(vec![ContentBlock::Text("first".into())]))
        .unwrap();
    drop(log);

    let mut opened = Log::open(&path).unwrap();
    let masker = Arc::new(Masker::default());
    masker.absorb_secret(&[("EFFERENCE_TEST_TOKEN".into(), "t0ken-value-here".into())]);
    opened.log.set_masker(masker);
    opened
        .log
        .append(
            0,
            Payload::User(vec![ContentBlock::Text("t0ken-value-here".into())]),
        )
        .unwrap();
    drop(opened);

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains("t0ken-value-here"),
        "the value is elided on the way in: {raw}"
    );

    std::fs::remove_file(&path).unwrap();
}
