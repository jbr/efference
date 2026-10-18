//! The `/v1/push` routes (docs/web-push.md).
//!
//! The store these routes write is machine-scoped by construction, so what is
//! asserted here is what the *router* does — validation, status codes, the error
//! envelope — and the store's own behaviour is left to `push/test.rs`, which
//! injects a path. `GET /v1/push` is asserted against a real root, since the key
//! it answers with is derived from one.

use super::*;
use base64::Engine;

/// `GET /v1/push` answers the derived VAPID public key in exactly the encoding
/// `applicationServerKey` takes: base64url of the uncompressed X9.62 point. It is
/// the same derivation the sender signs with, which is what makes a subscription
/// created against it verifiable. Without a root there is no signing key to hand
/// out, and the route says so in the typed envelope rather than inventing one.
///
/// Both branches are one test because the difference between them is the *process
/// environment*, which is shared: two tests toggling `EFFERENCE_ROOT_SECRET`
/// concurrently would each see the other's setting. Nothing else in this suite
/// reads that variable, so one test owning it for its duration is sound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_key_is_the_derived_vapid_public_key() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let server = test_server(&shared).await;

    crate::auth::Root::install(Some(crate::auth::Root::new("a test root")));
    let conn = server.get("/v1/push").await;
    conn.assert_status(200);
    conn.assert_json_body_with(|body: &Value| {
        let key = body["key"].as_str().expect("a key string");
        assert_eq!(
            key,
            crate::push::public_key(&crate::auth::Root::new("a test root").vapid_key())
        );
        let point = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(key)
            .expect("base64url");
        assert_eq!(point.len(), 65, "uncompressed X9.62");
        assert_eq!(point[0], 0x04, "the uncompressed point tag");
    });

    crate::auth::Root::install(None);
    let conn = server.get("/v1/push").await;
    conn.assert_status(500);
    conn.assert_json_body_with(|body: &Value| assert_eq!(body["code"], "internal"));
}

/// A body that is not the browser's `PushSubscription.toJSON()` shape is refused,
/// so nothing partial reaches the store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_subscription_is_refused() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let server = test_server(&shared).await;
    for body in [
        "{}",
        r#"{"endpoint":"https://push.example.net/p/a"}"#,
        r#"{"endpoint":"https://push.example.net/p/a","keys":{"p256dh":"x"}}"#,
        "not json",
    ] {
        let conn = server.post("/v1/push/subscriptions").with_body(body).await;
        conn.assert_status(400);
        conn.assert_json_body_with(|answer: &Value| {
            assert_eq!(answer["code"], "bad_request", "refusing {body}")
        });
    }
}

/// An unsubscribe without an endpoint is a bad request: the endpoint *is* the
/// subscription's identity, so there is nothing to act on without one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsubscribe_without_an_endpoint_is_refused() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let server = test_server(&shared).await;
    let conn = server
        .delete("/v1/push/subscriptions")
        .with_body("{}")
        .await;
    conn.assert_status(400);
    conn.assert_json_body_with(|answer: &Value| assert_eq!(answer["code"], "bad_request"));
}

/// What [`settle_body`] captions a settled turn with: the tail `Assistant`
/// entry's prose and only that entry's — an earlier turn's text would caption
/// this turn with another one's words.
mod settle_body {
    use crate::daemon::push::settle_body;
    use crate::log::UpstreamFailureRecord;
    use crate::log::{ContentBlock, LogEntry, Payload, Timestamp, ToolCall};

    fn entry(seq: u64, payload: Payload) -> LogEntry {
        LogEntry {
            seq,
            turn: 0,
            at: Timestamp(1000 + seq),
            submission_id: None,
            source: None,
            payload,
        }
    }

    fn assistant(text: &[&str]) -> Payload {
        Payload::Assistant {
            output: crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                text.iter()
                    .map(|t| ContentBlock::Text((*t).to_string()))
                    .collect(),
                vec![],
            ),
            excluded: None,
        }
    }

    #[test]
    fn the_tail_assistant_prose_and_only_it() {
        let entries = vec![
            entry(0, Payload::User(vec![ContentBlock::Text("hi".into())])),
            entry(1, assistant(&["an earlier turn"])),
            entry(2, Payload::User(vec![ContentBlock::Text("go on".into())])),
            entry(3, assistant(&["two blocks", "join with a space"])),
        ];
        assert_eq!(settle_body(&entries), "two blocks join with a space");
    }

    #[test]
    fn a_failed_tail_is_empty_not_an_earlier_turns_text() {
        let entries = vec![
            entry(0, assistant(&["a finished earlier turn"])),
            entry(
                1,
                Payload::Assistant {
                    output: crate::log::AssistantOutput::from_legacy_messages(
                        vec![],
                        vec![ContentBlock::Text("a partial".into())],
                        vec![],
                    ),
                    excluded: Some(crate::log::Exclusion::Upstream(UpstreamFailureRecord {
                        why: "stream died".into(),
                        stream: None,
                    })),
                },
            ),
        ];
        assert_eq!(settle_body(&entries), "");
    }

    #[test]
    fn no_prose_is_empty() {
        assert_eq!(settle_body(&[]), "");
        let calls_only = entry(
            0,
            Payload::Assistant {
                output: crate::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new("bash", "toolu_1", serde_json::json!({}))],
                ),
                excluded: None,
            },
        );
        assert_eq!(settle_body(&[calls_only]), "");
    }

    #[test]
    fn long_prose_truncates_on_a_char_boundary_with_an_ellipsis() {
        let long = "é".repeat(300);
        let body = settle_body(&[entry(0, assistant(&[&long]))]);
        assert_eq!(body.chars().count(), 201, "200 chars plus the ellipsis");
        assert!(body.ends_with('…'));
        let short = settle_body(&[entry(0, assistant(&["fits"]))]);
        assert_eq!(short, "fits");
    }
}

/// [`gate_body`] prefers the call's own `intent` — the purpose — over the
/// spotter's reason — the risk — and falls through the pair to a plain waiting
/// line.
#[test]
fn gate_body_prefers_intent_over_reason() {
    use crate::daemon::push::gate_body;
    assert_eq!(
        gate_body(
            "write",
            Some("record the decision"),
            Some("outside the tree"),
            None
        ),
        "write — record the decision"
    );
    assert_eq!(
        gate_body("write", None, Some("outside the tree"), None),
        "write — outside the tree"
    );
    assert_eq!(
        gate_body("write", None, None, None),
        "write is waiting for a decision"
    );
}
