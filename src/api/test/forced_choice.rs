//! Tests for the forced-`tool_choice` fallback (docs/transport.md): a model that
//! refuses forced choice with a 400 is asked again as `auto` with an instruction,
//! and the client remembers.

use super::*;
use crate::project::WireTurn;
use std::io::{Read, Write};

/// The refusal Sonnet 5.5 answers a forced `tool_choice` with.
const REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"tool_choice: type \"tool\" and \"any\" are not supported for this model."}}"#;

/// An empty stream that assembles into a completion.
pub(super) const STREAM: &str = "event: message_start\ndata: {\"message\":{\"id\":\"msg-1\",\
     \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
     event: data\ndata: [DONE]\n\n";

/// Serve `replies` in order, one connection each, as `(status, body)`; the
/// returned handle yields the request bodies received.
pub(super) fn scripted(
    replies: Vec<(u16, &'static str)>,
) -> (Client, std::thread::JoinHandle<Vec<Value>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for (status, reply) in replies {
            let (mut sock, _) = listener.accept().unwrap();
            let mut request = String::new();
            let mut buf = [0u8; 16384];
            let body = loop {
                let read = sock.read(&mut buf).unwrap();
                request.push_str(&String::from_utf8_lossy(&buf[..read]));
                let complete = request.split_once("\r\n\r\n").and_then(|(head, body)| {
                    let length = head
                        .to_ascii_lowercase()
                        .split("\r\n")
                        .find_map(|line| line.strip_prefix("content-length: ").map(str::to_string))
                        .and_then(|n| n.trim().parse::<usize>().ok())?;
                    (body.len() >= length).then(|| body.to_string())
                });
                if let Some(body) = complete {
                    break body;
                }
                assert!(read > 0, "the request ended early: {request}");
            };
            bodies.push(serde_json::from_str(&body).unwrap());
            let content_type = if status == 200 {
                "text/event-stream"
            } else {
                "application/json"
            };
            sock.write_all(
                format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\n\
                     connection: close\r\ncontent-length: {}\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
        }
        bodies
    });
    let endpoint = Endpoint::at(Provider::Anthropic, format!("http://{addr}/v1/messages"));
    let client =
        Client::new(endpoint, "claude-sonnet-5-5", "sys", None).with_thinking(ThinkingSupport {
            adaptive: true,
            off: ThinkingOff::BetweenTools,
        });
    (client, server)
}

fn last_user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .to_string()
}

/// A forced call refused with a 400 is retried as `auto`, ending with a line
/// naming the tool and with thinking still off; the next forced call goes
/// straight to that form, one request.
#[tokio::test]
async fn a_refused_forced_choice_falls_back_to_auto_and_is_remembered() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM), (200, STREAM)]);
    let turns = [WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let bodies = server.join().unwrap();

    assert_eq!(bodies[0]["tool_choice"]["type"], "tool");
    assert_eq!(bodies[0]["thinking"]["type"], "between_tools");
    for body in &bodies[1..] {
        assert_eq!(body["tool_choice"]["type"], "auto");
        assert_eq!(body["thinking"]["type"], "between_tools", "{body}");
        assert!(
            last_user_text(body).contains("calling the `spot` tool"),
            "{body}"
        );
    }
}

/// The fallback carries a turn's origin but not the turn's prompt — it appends an
/// instruction turn and turns thinking off — so it is off the cache chain
/// (docs/transport.md): the turn's own request opts in, and the retry that follows it
/// neither sends the object nor leaves an id for the next turn to be compared against.
/// Naming the fallback's response as the next comparison base would have the provider
/// report a divergence in `messages` that the loop never made.
#[tokio::test]
async fn the_forced_choice_fallback_stays_off_the_cache_chain() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM), (200, STREAM)]);
    let client = client.with_origin(crate::log::RequestOrigin::Turn);
    let turns = [WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let bodies = server.join().unwrap();

    assert_eq!(
        bodies[0]["diagnostics"]["previous_message_id"],
        Value::Null,
        "the turn's own request opts in: {}",
        bodies[0]
    );
    for body in &bodies[1..] {
        assert!(
            body.get("diagnostics").is_none(),
            "the fallback is off the chain: {body}"
        );
    }
}

/// When the `auto` retry fails too, the 400 had another cause: the first error
/// comes back and nothing is learned, so the next forced call is sent forced.
#[tokio::test]
async fn a_400_the_fallback_does_not_cure_is_returned_and_not_learned() {
    let other =
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"something else"}}"#;
    let (client, server) = scripted(vec![(400, REFUSAL), (400, other), (200, STREAM)]);
    let turns = [WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    let error = client
        .complete(&turns, &[], forced, |_| {})
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not supported"), "{error}");
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let bodies = server.join().unwrap();
    assert_eq!(bodies[2]["tool_choice"]["type"], "tool");
}

/// An `auto` request that gets a 400 is not retried: there is no forced choice
/// to give up.
#[tokio::test]
async fn an_unforced_400_is_not_retried() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM)]);
    let turns = [WireTurn::text("hi")];
    assert!(
        client
            .complete(&turns, &[], ToolChoice::auto(), |_| {})
            .await
            .is_err()
    );
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.join().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(!last_user_text(&bodies[1]).contains("calling the"));
}

/// What a forced call went out as, read after it returned (docs/transport.md): as given
/// until the model refuses forced choice, then the fallback's form — `auto`, the
/// instruction appended, thinking off — which is what a keepalive of it must re-send.
#[tokio::test]
async fn the_sent_form_follows_the_learned_fallback() {
    let (client, server) = scripted(vec![(200, STREAM), (400, REFUSAL), (200, STREAM)]);
    let turns = vec![WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    assert_eq!(
        client.sent_form(turns.clone(), &[], forced),
        SentRequest::new(turns.clone(), Vec::new(), forced)
    );
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let sent = client.sent_form(turns.clone(), &[], forced);
    assert!(sent.unforced);
    assert_eq!(sent.choice, ToolChoice::auto());
    assert_eq!(sent.turns.len(), 2, "the instruction rides after the turns");
    server.join().unwrap();
}

/// A ping of a request the fallback sent goes out as the fallback did: `auto`, thinking
/// as the fallback spelled it, and over the turns it is given.
#[tokio::test]
async fn a_keepalive_of_an_unforced_request_sends_the_fallbacks_envelope() {
    let ping = r#"{"id":"p","model":"m","usage":{"input_tokens":1,"output_tokens":0}}"#;
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM), (200, ping)]);
    let turns = vec![WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let mut sent = client.sent_form(turns.clone(), &[], forced);
    sent.turns.truncate(1);
    client
        .keepalive(crate::log::RequestOrigin::TriageKeepalive, &sent)
        .await
        .unwrap();
    let bodies = server.join().unwrap();
    let (fallback, ping) = (&bodies[1], &bodies[2]);
    assert_eq!(ping["tool_choice"], fallback["tool_choice"]);
    assert_eq!(ping["thinking"], fallback["thinking"]);
    assert_eq!(ping["messages"][0], fallback["messages"][0]);
    assert_eq!(ping["messages"].as_array().unwrap().len(), 1);
    assert_eq!(ping["max_tokens"], 0);
}

/// A request ends at its last breakpoint: later turns go, and so do a user turn's parts
/// after a breakpoint inside it. With no breakpoint there is nothing to end at.
#[test]
fn a_request_ends_at_its_last_breakpoint() {
    use crate::project::UserPart;
    let history = WireTurn::User(vec![
        UserPart::Text("digest".into()),
        UserPart::Breakpoint,
        UserPart::Text("tail".into()),
        UserPart::Breakpoint,
    ]);
    let current = WireTurn::User(vec![UserPart::Text("current step".into())]);
    let request = SentRequest::new(
        vec![history.clone(), current.clone()],
        Vec::new(),
        ToolChoice::force("retain"),
    );
    assert_eq!(
        request.through_last_breakpoint().unwrap().turns,
        vec![history.clone()]
    );

    let mid = WireTurn::User(vec![
        UserPart::Text("head".into()),
        UserPart::Breakpoint,
        UserPart::Text("after".into()),
    ]);
    let request = SentRequest::new(vec![mid, current.clone()], Vec::new(), ToolChoice::auto());
    assert_eq!(
        request.through_last_breakpoint().unwrap().turns,
        vec![WireTurn::User(vec![
            UserPart::Text("head".into()),
            UserPart::Breakpoint
        ])]
    );

    let request = SentRequest::new(
        vec![current.clone(), WireTurn::Breakpoint, current.clone()],
        Vec::new(),
        ToolChoice::auto(),
    );
    assert_eq!(
        request.through_last_breakpoint().unwrap().turns,
        vec![current.clone(), WireTurn::Breakpoint]
    );

    let request = SentRequest::new(vec![current], Vec::new(), ToolChoice::auto());
    assert_eq!(request.through_last_breakpoint(), None);
}
