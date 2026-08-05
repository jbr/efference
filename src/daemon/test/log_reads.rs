//! Tests for the bounded log reads and the two entry-fetch routes
//! (`views.rs`/`http.rs`, docs/control-api.md).

use super::*;

fn user(text: &str) -> Payload {
    Payload::User(vec![crate::log::ContentBlock::Text(text.into())])
}

/// An assistant turn carrying one tool call — the shape `/tool_calls/:call_id` addresses
/// into, since a call is embedded in an entry rather than being one.
fn assistant_calling(call_id: &str, command: &str) -> Payload {
    Payload::Assistant {
        thinking: vec![],
        text: vec![],
        calls: vec![ToolCall {
            id: CallId(call_id.into()),
            name: crate::bash::NAME.into(),
            input: serde_json::json!({ "command": command }),
        }],
    }
}

/// A log of `n` user entries, seq 0..n.
fn log_of(n: u64) -> Arc<Shared> {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    for i in 0..n {
        shared.append(1, user(&format!("entry {i}"))).unwrap();
    }
    // The turns receiver is dropped, which is fine: nothing here submits a turn.
    shared
}

fn seqs(body: &str) -> Vec<u64> {
    serde_json::from_str::<Vec<Value>>(body)
        .expect("a JSON array")
        .iter()
        .map(|e| e["seq"].as_u64().expect("a seq"))
        .collect()
}

/// The query grammar (docs/control-api.md). `since` and `tail` are mutually exclusive
/// rather than resolved by precedence: one asks to resume from a position and the other
/// to open at the end, so a request carrying both has no single reading.
#[test]
fn log_select_parses_positions_and_counts() {
    assert_eq!(
        LogSelect::parse("since=12").unwrap(),
        LogSelect {
            since: Some(12),
            ..Default::default()
        }
    );
    assert_eq!(
        LogSelect::parse("tail=30").unwrap(),
        LogSelect {
            tail: Some(30),
            ..Default::default()
        }
    );
    // One page backward: the 30 entries ending just before seq 200.
    assert_eq!(
        LogSelect::parse("before=200&tail=30").unwrap(),
        LogSelect {
            before: Some(200),
            tail: Some(30),
            ..Default::default()
        }
    );
    assert_eq!(LogSelect::parse("").unwrap(), LogSelect::default());

    assert!(LogSelect::parse("since=1&tail=1").is_err(), "no reading");
    // A malformed bound is rejected, not ignored: dropping it would silently answer with
    // the whole log, which is the failure the window exists to prevent.
    assert!(LogSelect::parse("tail=-1").is_err(), "negative count");
    assert!(LogSelect::parse("tail=abc").is_err(), "non-numeric");
    assert!(LogSelect::parse("since=1.5").is_err(), "non-integer");
}

/// `?tail=N` means exactly N, cut anywhere — no boundary-snapping, no keyframes. Cuts are
/// free because a reference dangling across one resolves by fetch (the two routes below).
#[tokio::test]
async fn tail_returns_exactly_n_from_the_end() {
    let app = test_server(&log_of(5)).await;

    assert_eq!(seqs(app.get("/log?tail=2").await.body()), vec![3, 4]);
    // Fewer entries than asked for is the whole log, not an error — the same answer a
    // client gets early in a short session.
    assert_eq!(
        seqs(app.get("/log?tail=99").await.body()),
        vec![0, 1, 2, 3, 4]
    );
    assert_eq!(seqs(app.get("/log?tail=0").await.body()), Vec::<u64>::new());
    // No bound at all is still the whole log: the default is unchanged.
    assert_eq!(seqs(app.get("/log").await.body()), vec![0, 1, 2, 3, 4]);
}

/// `?before=<seq>&tail=N` is one page backward — the escape hatch for a client scrolling
/// past the window it opened with. Paging off the front yields an empty page rather than
/// an error, since that is what the last page naturally asks for.
#[tokio::test]
async fn before_and_tail_page_backward() {
    let app = test_server(&log_of(10)).await;

    assert_eq!(
        seqs(app.get("/log?before=5&tail=3").await.body()),
        vec![2, 3, 4]
    );
    // The page before that one, addressed by the first seq the client holds.
    assert_eq!(
        seqs(app.get("/log?before=2&tail=3").await.body()),
        vec![0, 1]
    );
    assert_eq!(
        seqs(app.get("/log?before=0&tail=3").await.body()),
        Vec::<u64>::new()
    );
    // `before` alone is everything up to it; `since` alone still resumes forward.
    assert_eq!(seqs(app.get("/log?before=3").await.body()), vec![0, 1, 2]);
    assert_eq!(seqs(app.get("/log?since=8").await.body()), vec![8, 9]);
}

/// A query with no single reading is a typed 400 rather than a silently-picked winner.
#[tokio::test]
async fn contradictory_bounds_are_a_typed_error() {
    let app = test_server(&log_of(3)).await;

    let conn = app.get("/log?since=1&tail=1").await;
    conn.assert_status(400);
    conn.assert_json_body_with(|body: &Value| {
        assert_eq!(body["code"], "bad_request");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("mutually exclusive"),
            "says which pair: {body}"
        );
    });
}

/// `GET /log/:seq` is the general form of reference resolution for a windowed client, and
/// is immutably cacheable — a `LogEntry` is written once and never mutated.
#[tokio::test]
async fn one_entry_by_seq_is_immutably_cacheable() {
    let app = test_server(&log_of(3)).await;

    let conn = app.get("/log/1").await;
    conn.assert_ok();
    conn.assert_header_with("cache-control", |value| {
        let value = format!("{value:?}");
        assert!(value.contains("immutable"), "never refetched: {value}");
    });
    conn.assert_json_body_with(|entry: &Value| {
        assert_eq!(entry["seq"], 1);
        assert!(entry["payload"].to_string().contains("entry 1"));
    });

    // Past the head of the log is a 404, distinct from a window that happens to be empty.
    let conn = app.get("/log/99").await;
    conn.assert_status(404);
    conn.assert_json_body_with(|body: &Value| assert_eq!(body["code"], "not_found"));
}

/// `GET /tool_calls/:call_id` resolves the one reference that is *not* an entry: a call
/// embedded in an `Assistant` payload, addressed by the id the log, the gate, and
/// `tool_output` frames already carry. This is what makes a window cut between a call and
/// its result harmless.
#[tokio::test]
async fn a_tool_call_resolves_by_its_call_id() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.append(1, user("run the tests")).unwrap();
    shared
        .append(1, assistant_calling("toolu_9", "cargo test"))
        .unwrap();
    let app = test_server(&shared).await;

    let conn = app.get("/tool_calls/toolu_9").await;
    conn.assert_ok();
    conn.assert_header_with("cache-control", |value| {
        assert!(format!("{value:?}").contains("immutable"));
    });
    conn.assert_json_body_with(|body: &Value| {
        // The seq rides along so a client that resolved a call can reach the entry
        // carrying it without a search.
        assert_eq!(body["seq"], 1);
        assert_eq!(body["call"]["id"], "toolu_9");
        assert_eq!(body["call"]["name"], crate::bash::NAME);
        assert_eq!(body["call"]["input"]["command"], "cargo test");
    });

    let conn = app.get("/tool_calls/toolu_absent").await;
    conn.assert_status(404);
    conn.assert_json_body_with(|body: &Value| assert_eq!(body["code"], "not_found"));
}
