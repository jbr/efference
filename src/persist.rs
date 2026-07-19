//! Durable persistence of the log (docs/README.md).
//!
//! Append-only **JSONL**: one [`LogEntry`] per line. Chosen deliberately over a
//! binary format because the log is an append-only *event journal* we want to
//! read/`tail`/`jq` while iterating on projection — and because a Phase-1
//! redaction is itself just another appended event line (flip `wire`, never
//! touch `raw`), where a snapshot format would re-serialize the world. Perf is a
//! non-issue: projection reads the in-memory `Vec` each turn, never the disk;
//! the disk is touched once per append and once per session load.
//!
//! This is the durable substrate *mechanism*; `log.rs` holds the substrate
//! *types*. Phase 0 always constructs `wire: Raw`, so persisting the entry whole
//! is faithful; when Phase 1 introduces wire flips, those become their own
//! appended events that replay folds in (not a rewrite of the line).

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::log::{LogEntry, Payload, Seq, Timestamp, Turn};

/// The live append-only log: the in-memory entries plus an optional JSONL sink.
/// Construction is the only place that touches the clock and `seq` allocation,
/// so those non-deterministic inputs never leak toward the wire (docs/substrate.md).
pub struct Log {
    entries: Vec<LogEntry>,
    sink: Option<BufWriter<File>>,
}

impl Log {
    /// A live log appending to `path` (created if absent; opened for append so a
    /// re-run never truncates a prior session).
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            entries: Vec::new(),
            sink: Some(BufWriter::new(file)),
        })
    }

    /// An in-memory-only log — ephemeral runs and tests; no disk.
    pub fn in_memory() -> Self {
        Self {
            entries: Vec::new(),
            sink: None,
        }
    }

    /// The entries so far — what [`crate::project::project`] folds into the wire.
    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    /// Append a payload: assign the next `seq` (gap-free — append-only, so
    /// `seq == index`), stamp the wall clock, **durably write the JSONL line**,
    /// then keep it in memory. The line is flushed before returning so a crash
    /// can't lose an acknowledged entry (we stop short of `fsync` per line in
    /// Phase 0 — a deliberate durability/throughput tradeoff to revisit).
    pub fn append(&mut self, turn: Turn, payload: Payload) -> io::Result<Seq> {
        let seq = self.entries.len() as Seq;
        let entry = LogEntry {
            seq,
            turn,
            at: now(),
            payload,
        };
        if let Some(sink) = &mut self.sink {
            serde_json::to_writer(&mut *sink, &entry).map_err(io::Error::other)?;
            sink.write_all(b"\n")?;
            sink.flush()?;
        }
        self.entries.push(entry);
        Ok(seq)
    }
}

/// Replay a persisted log back into memory (the basis for daemon attach — the backlog).
/// Blank lines are tolerated; a malformed line is a hard error — a corrupt
/// journal should be loud, not silently truncated.
pub fn load(path: impl AsRef<Path>) -> io::Result<Vec<LogEntry>> {
    let file = File::open(path)?;
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(&line).map_err(io::Error::other)?);
    }
    Ok(entries)
}

fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as u64;
    Timestamp(millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{CallId, ContentBlock, RawResult, ToolCall, WireState};
    use crate::project::project;
    use serde_json::json;

    fn sample(log: &mut Log) {
        log.append(0, Payload::User(vec![ContentBlock::Text("hello".into())]))
            .unwrap();
        log.append(
            1,
            Payload::Assistant {
                thinking: vec![],
                text: vec![ContentBlock::Text("let me check".into())],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    input: json!({ "command": "ls", "timeout": 5 }),
                }],
            },
        )
        .unwrap();
        log.append(
            1,
            Payload::ToolResult {
                call_id: CallId("toolu_1".into()),
                raw: RawResult {
                    content: "file.txt".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
        )
        .unwrap();
    }

    /// The headline property: a log written to disk and read back projects to the
    /// exact same wire bytes — persistence is faithful, and reuses the existing
    /// byte-stability guarantee as its oracle.
    #[tokio::test]
    async fn disk_roundtrip_projects_identically() {
        let path =
            std::env::temp_dir().join(format!("efference-roundtrip-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut log = Log::create(&path).unwrap();
        sample(&mut log);
        let expected = serde_json::to_string(&project(log.entries())).unwrap();
        drop(log); // flush + close.

        let loaded = load(&path).unwrap();
        let actual = serde_json::to_string(&project(&loaded)).unwrap();
        assert_eq!(actual, expected, "disk round-trip must project identically");
        assert_eq!(loaded.len(), 3);

        std::fs::remove_file(&path).unwrap();
    }

    /// The boot preamble snapshots the **structured** `[context]` listing beside the flat
    /// `system` string (backlog/context-surfacing.md), so a listed item's name / path /
    /// description survive the log's serde and `project` can later fold them back into a
    /// surfacing reminder. The flat `system` stays the byte-exact wire truth; this pins the
    /// additive structured half.
    #[test]
    fn preamble_snapshots_the_structured_context_listing() {
        use crate::context::{Item, Resolved, Section};

        let listing = Resolved {
            sections: vec![Section {
                heading: Some("Skills".into()),
                items: vec![Item::Listed {
                    name: "ferritin".into(),
                    description: "look up Rust docs".into(),
                    path: "skills/ferritin.md".into(),
                }],
            }],
        };
        let line = serde_json::to_string(&Payload::Preamble {
            system: "sys".into(),
            context: listing.clone(),
        })
        .unwrap();

        match serde_json::from_str::<Payload>(&line).unwrap() {
            Payload::Preamble { system, context } => {
                assert_eq!(system, "sys");
                assert_eq!(
                    context, listing,
                    "structured listing must survive the round-trip"
                );
            }
            other => panic!("expected a Preamble, got {other:?}"),
        }
    }

    /// A log line predating the structured listing (bare `{"system": …}`) still loads — the
    /// `#[serde(default)]` on `context` reads it back as an empty `Resolved`, never a parse
    /// error. Old logs must remain replayable.
    #[test]
    fn legacy_preamble_without_context_reads_back_empty() {
        let legacy = r#"{"Preamble":{"system":"you are an agent"}}"#;
        match serde_json::from_str::<Payload>(legacy).unwrap() {
            Payload::Preamble { system, context } => {
                assert_eq!(system, "you are an agent");
                assert_eq!(context, crate::context::Resolved::default());
            }
            other => panic!("expected a Preamble, got {other:?}"),
        }
    }

    /// Blank lines tolerated; entry count survives.
    #[test]
    fn load_tolerates_blank_lines() {
        let path =
            std::env::temp_dir().join(format!("efference-blank-{}.jsonl", std::process::id()));
        std::fs::write(
            &path,
            "{\"seq\":0,\"turn\":0,\"at\":1,\"payload\":{\"User\":[{\"Text\":\"hi\"}]}}\n\n",
        )
        .unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        std::fs::remove_file(&path).unwrap();
    }
}
