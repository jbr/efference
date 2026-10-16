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
        output: crate::log::AssistantOutput::from_legacy_messages(
            vec![],
            vec![],
            vec![ToolCall::new(
                crate::bash::NAME,
                call_id,
                serde_json::json!({ "command": command }),
            )],
        ),
        upstream_failure: None,
    }
}

/// A log of `n` user entries, seq 0..n.
fn log_of(n: u64) -> Arc<Session> {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
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

    assert_eq!(seqs(app.get("/v1/log?tail=2").await.body()), vec![3, 4]);
    // Fewer entries than asked for is the whole log, not an error — the same answer a
    // client gets early in a short session.
    assert_eq!(
        seqs(app.get("/v1/log?tail=99").await.body()),
        vec![0, 1, 2, 3, 4]
    );
    assert_eq!(
        seqs(app.get("/v1/log?tail=0").await.body()),
        Vec::<u64>::new()
    );
    // No bound at all is still the whole log: the default is unchanged.
    assert_eq!(seqs(app.get("/v1/log").await.body()), vec![0, 1, 2, 3, 4]);
}

/// `?before=<seq>&tail=N` is one page backward — the escape hatch for a client scrolling
/// past the window it opened with. Paging off the front yields an empty page rather than
/// an error, since that is what the last page naturally asks for.
#[tokio::test]
async fn before_and_tail_page_backward() {
    let app = test_server(&log_of(10)).await;

    assert_eq!(
        seqs(app.get("/v1/log?before=5&tail=3").await.body()),
        vec![2, 3, 4]
    );
    // The page before that one, addressed by the first seq the client holds.
    assert_eq!(
        seqs(app.get("/v1/log?before=2&tail=3").await.body()),
        vec![0, 1]
    );
    assert_eq!(
        seqs(app.get("/v1/log?before=0&tail=3").await.body()),
        Vec::<u64>::new()
    );
    // `before` alone is everything up to it; `since` alone still resumes forward.
    assert_eq!(
        seqs(app.get("/v1/log?before=3").await.body()),
        vec![0, 1, 2]
    );
    assert_eq!(seqs(app.get("/v1/log?since=8").await.body()), vec![8, 9]);
}

/// A query with no single reading is a typed 400 rather than a silently-picked winner.
#[tokio::test]
async fn contradictory_bounds_are_a_typed_error() {
    let app = test_server(&log_of(3)).await;

    let conn = app.get("/v1/log?since=1&tail=1").await;
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

/// `GET /log/:seq` is the general form of reference resolution for a windowed client.
/// It validates rather than promising freshness: a content `etag`, a `304` on a match,
/// and no `cache-control` — the address is shared across sessions at a socket-root
/// binding, and the served shape changes with the build, so a stored copy must be
/// checked against this daemon every time.
#[tokio::test]
async fn one_entry_by_seq_validates_with_an_etag() {
    let app = test_server(&log_of(3)).await;

    let conn = app.get("/v1/log/1").await;
    conn.assert_ok();
    assert!(
        conn.response_headers().get_str("cache-control").is_none(),
        "no freshness lifetime on a cross-session address"
    );
    let etag = conn
        .response_headers()
        .get_str("etag")
        .expect("etag")
        .to_owned();
    conn.assert_json_body_with(|entry: &Value| {
        assert_eq!(entry["seq"], 1);
        assert!(entry["payload"].to_string().contains("entry 1"));
    });

    let conn = app
        .get("/v1/log/1")
        .with_request_header("if-none-match", etag)
        .await;
    conn.assert_status(304);
    assert!(conn.body_bytes().is_empty(), "a 304 carries no body");

    // Past the head of the log is a 404, distinct from a window that happens to be empty.
    let conn = app.get("/v1/log/99").await;
    conn.assert_status(404);
    conn.assert_json_body_with(|body: &Value| assert_eq!(body["code"], "not_found"));
}

/// `GET /tool_calls/:call_id` resolves the one reference that is *not* an entry: a call
/// embedded in an `Assistant` payload, addressed by the id the log, the gate, and
/// `tool_output` frames already carry. This is what makes a window cut between a call and
/// its result harmless.
#[tokio::test]
async fn a_tool_call_resolves_by_its_call_id() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("run the tests")).unwrap();
    shared
        .append(1, assistant_calling("toolu_9", "cargo test"))
        .unwrap();
    let app = test_server(&shared).await;

    let conn = app.get("/v1/tool_calls/toolu_9").await;
    conn.assert_ok();
    assert!(conn.response_headers().get_str("etag").is_some());
    conn.assert_json_body_with(|body: &Value| {
        // The seq rides along so a client that resolved a call can reach the entry
        // carrying it without a search.
        assert_eq!(body["seq"], 1);
        assert_eq!(body["call"]["id"], "toolu_9");
        assert_eq!(body["call"]["name"], crate::bash::NAME);
        assert_eq!(body["call"]["input"]["command"], "cargo test");
    });

    let conn = app.get("/v1/tool_calls/toolu_absent").await;
    conn.assert_status(404);
    conn.assert_json_body_with(|body: &Value| assert_eq!(body["code"], "not_found"));
}

/// A tool result carrying one image whose decoded bytes are `IMAGE_BYTES`.
fn result_with_image() -> Payload {
    use base64::Engine;
    Payload::tool_result(
        "toolu_img",
        crate::log::RawResult::text("2×2 image/webp, 1 visual tokens").with_images(vec![
            crate::log::ResultImage {
                media_type: "image/webp".into(),
                data: base64::engine::general_purpose::STANDARD.encode(IMAGE_BYTES),
                width: 2,
                height: 2,
            },
        ]),
        crate::log::WireState::Raw,
    )
}

const IMAGE_BYTES: &[u8] = &[0x52, 0x49, 0x46, 0x46, 0x00, 0x01, 0x02, 0x03];

/// The log reads serve entries as views — a result image's base64 `data` never rides
/// `GET /log` or `GET /log/:seq`; the layout metadata does, and the bytes resolve
/// through `GET /log/:seq/images/:idx` (entry_view.rs, docs/control-api.md).
#[tokio::test]
async fn log_reads_reference_images_instead_of_inlining_them() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("look at this")).unwrap();
    shared.append(1, result_with_image()).unwrap();
    let app = test_server(&shared).await;

    for path in ["/v1/log", "/v1/log/1"] {
        let conn = app.get(path).await;
        let body = conn.body();
        assert!(
            !body.contains("UklGR"),
            "base64 data must not ride {path}: {body}"
        );
        assert!(body.contains(r#""media_type":"image/webp""#), "{path}");
        assert!(body.contains(r#""width":2"#), "{path}");
    }
}

/// `GET /log/:seq/images/:idx` answers with the decoded bytes under the stored media
/// type, `ETag`-validated like any single-entry read; every miss shape is a 404 and
/// malformed addresses are 400s.
#[tokio::test]
async fn an_image_resolves_by_seq_and_index() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("look at this")).unwrap();
    shared.append(1, result_with_image()).unwrap();
    let app = test_server(&shared).await;

    let conn = app.get("/v1/log/1/images/0").await;
    conn.assert_ok();
    conn.assert_header("content-type", "image/webp");
    assert!(conn.response_headers().get_str("etag").is_some());
    assert_eq!(conn.body_bytes(), IMAGE_BYTES, "decoded, not base64");

    // Misses: an index past the result's images, an entry that is not a tool result,
    // and a seq past the head — all the same 404.
    for path in [
        "/v1/log/1/images/1",
        "/v1/log/0/images/0",
        "/v1/log/9/images/0",
    ] {
        let conn = app.get(path).await;
        conn.assert_status(404);
        conn.assert_json_body_with(|body: &Value| assert_eq!(body["code"], "not_found"));
    }
    app.get("/v1/log/x/images/0").await.assert_status(400);
    app.get("/v1/log/1/images/-1").await.assert_status(400);
}

/// The two sides of the result-size threshold, over the real routes
/// (docs/control-api.md). The window elides an oversized result's `content` and tags
/// what it cut; `GET /log/:seq` — the dereference address — serves it whole. A result
/// under the threshold is byte-identical through both, which is the property the
/// frame ≡ window pin now states (`frames.rs`).
/// The line counts ride whichever entry decided the result (docs/triage.md): a
/// result captured before its triage carries none of its own, and its verdict — which
/// names the result but holds none of its bytes — carries them, so a client updating
/// its slot from that one frame states the same numbers.
#[tokio::test]
async fn a_captured_result_serves_its_line_counts_on_the_verdict() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let content = (1..=40)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let result_seq = shared
        .append(
            1,
            Payload::tool_result(
                "toolu_1",
                crate::log::RawResult::text(&content),
                crate::log::WireState::Pending,
            ),
        )
        .unwrap();
    let verdict_seq = shared
        .append(
            1,
            Payload::ResultVerdict {
                for_result: result_seq,
                wire: crate::log::WireState::from(crate::log::Triaged::new(
                    crate::log::Keep::Ranges(vec![(1, 6)]),
                )),
            },
        )
        .unwrap();
    let app = test_server(&shared).await;

    let window = app.get("/v1/log").await;
    window.assert_ok();
    let entries: Vec<Value> = serde_json::from_str(window.body()).unwrap();

    let captured = &entries[result_seq as usize]["payload"]["ToolResult"];
    assert_eq!(captured["wire"], "Pending");
    assert!(
        captured.get("selection").is_none(),
        "an undecided result states no count: {captured}"
    );

    let verdict = &entries[verdict_seq as usize]["payload"]["ResultVerdict"];
    assert_eq!(verdict["for_result"], result_seq);
    assert_eq!(verdict["selection"]["total_lines"], 40);
    assert_eq!(verdict["selection"]["kept_lines"], 6);

    // The same entry by address carries the same counts — the two routes are one view.
    let by_seq = app.get(&format!("/v1/log/{verdict_seq}")).await;
    by_seq.assert_ok();
    by_seq.assert_json_body_with(|entry: &Value| {
        assert_eq!(entry["payload"], entries[verdict_seq as usize]["payload"]);
    });
}

/// A result logged before capture existed carries the counts on its own entry, which
/// is what every log and fixture written the old way still serves.
#[tokio::test]
async fn a_result_logged_with_its_verdict_serves_the_counts_on_itself() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let content = (1..=40)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    shared
        .append(
            1,
            Payload::tool_result(
                "toolu_1",
                crate::log::RawResult::text(&content),
                crate::log::WireState::from(crate::log::Triaged::new(crate::log::Keep::Ranges(
                    vec![(1, 6)],
                ))),
            ),
        )
        .unwrap();
    let app = test_server(&shared).await;

    let window = app.get("/v1/log").await;
    window.assert_ok();
    let entries: Vec<Value> = serde_json::from_str(window.body()).unwrap();
    let result = &entries[0]["payload"]["ToolResult"];
    assert_eq!(result["selection"]["total_lines"], 40);
    assert_eq!(result["selection"]["kept_lines"], 6);
}

#[tokio::test]
async fn an_oversized_result_is_elided_in_the_window_and_whole_by_seq() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let big = "z".repeat(crate::daemon::entry_view::MAX_INLINE_RESULT_BYTES * 2);
    let result = |call_id: &str, content: &str| {
        Payload::tool_result(
            call_id,
            crate::log::RawResult::text(content),
            crate::log::WireState::Raw,
        )
    };
    shared.append(1, result("toolu_small", "small")).unwrap();
    shared.append(1, result("toolu_big", &big)).unwrap();
    let app = test_server(&shared).await;

    let window = app.get("/v1/log").await;
    window.assert_ok();
    let entries: Vec<Value> = serde_json::from_str(window.body()).unwrap();

    let small = &entries[0]["payload"]["ToolResult"]["raw"];
    assert_eq!(small["content"], "small");
    assert!(
        small.get("content_elided").is_none(),
        "no tag on a result that fits: {small}"
    );

    let elided = &entries[1]["payload"]["ToolResult"]["raw"];
    assert_eq!(
        elided["content"].as_str().unwrap().len(),
        crate::daemon::entry_view::MAX_INLINE_RESULT_BYTES,
        "the window carries the prefix"
    );
    assert_eq!(
        elided["content_elided"]["kept_bytes"],
        crate::daemon::entry_view::MAX_INLINE_RESULT_BYTES
    );
    assert_eq!(elided["content_elided"]["total_bytes"], big.len());

    // The same two entries by address: the small one identical, the big one whole.
    let by_seq = app.get("/v1/log/0").await;
    by_seq.assert_ok();
    by_seq.assert_json_body_with(|entry: &Value| {
        assert_eq!(*entry, entries[0], "below the threshold the routes agree");
    });

    let by_seq = app.get("/v1/log/1").await;
    by_seq.assert_ok();
    by_seq.assert_json_body_with(|entry: &Value| {
        let raw = &entry["payload"]["ToolResult"]["raw"];
        assert_eq!(raw["content"].as_str().unwrap().len(), big.len());
        assert!(
            raw.get("content_elided").is_none(),
            "the whole result carries no elision tag"
        );
    });
}
