//! Tests for the title ask at the loop's gates (docs/session-title.md): the
//! detached task appends the answer whenever it lands, and the gate's predicate
//! stays silent while the standing title answers it.

use super::*;
use crate::api::{Endpoint, Provider};
use crate::daemon::SessionIdentity;
use crate::log::{Title, TitleAuthor};

fn session() -> Arc<Session> {
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    // The identity carries the title: without it `set_title` records nothing
    // (a session read with no daemon behind it). `None` inference is the
    // not-booted shape tests build.
    shared.set_identity(SessionIdentity {
        id: "title-test".into(),
        title: None,
        model: "m".into(),
        cwd: std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        inference: None,
        successor: None,
    });
    shared
}

/// A throwaway socket serving up to two completions, each a one-line text
/// answer (the codex tests' mock, pointed at a message output item). Two
/// accepts so a test asserting *no* peek fired fails loudly if one does.
fn mock_client(line: &str) -> Client {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let line = line.to_string();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut sock, _)) = listener.accept() else {
                break;
            };
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).unwrap();
            let item = format!(
                "{{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\
                 \"status\":\"completed\",\"content\":[{{\"type\":\"output_text\",\
                 \"text\":\"{line}\",\"annotations\":[]}}]}}"
            );
            let body = format!(
                "event: response.completed\ndata: {{\"type\":\"response.completed\",\
                 \"response\":{{\"id\":\"r\",\"status\":\"completed\",\"output\":[{item}],\
                 \"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\n"
            );
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
        }
    });
    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some(format!("http://{addr}/backend-api/codex/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        base: None,
    };
    Client::new(endpoint, "m", "sys", None)
}

fn titles(shared: &Session) -> Vec<String> {
    shared
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Title(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

/// A user turn lands at a gate with an over-budget standing title: the detached
/// ask runs against the mock and the compressed answer lands as a new entry.
#[tokio::test]
async fn the_title_ask_compresses_an_over_budget_title_at_a_gate() {
    let shared = session();
    shared
        .set_title(Title {
            text: "Implementing result-before-triage capture with later verdicts and a long tail"
                .into(),
            author: TitleAuthor::Triage,
        })
        .unwrap();
    shared
        .append_submission(
            1,
            Payload::User(vec![ContentBlock::Text("new scope".into())]),
            "sub".into(),
            Some("web".into()),
        )
        .unwrap();
    let client = mock_client("Fix title cadence");
    let ask = TitleAsk::new(&client, &[], &ForkPeek::shared());
    solicit_title_detached(&shared, &ask);
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let titles = titles(&shared);
        if titles.len() > 1 {
            assert_eq!(titles[1], "Fix title cadence");
            return;
        }
    }
    panic!("the title ask never landed: {:?}", titles(&shared));
}

/// A standing title with nothing after it answers the gate: no ask may run,
/// even though the mock would answer one.
#[tokio::test]
async fn the_title_ask_stays_silent_while_the_title_answers_the_gate() {
    let shared = session();
    shared
        .set_title(Title {
            text: "Fix title cadence".into(),
            author: TitleAuthor::Triage,
        })
        .unwrap();
    let client = mock_client("A different line");
    let ask = TitleAsk::new(&client, &[], &ForkPeek::shared());
    solicit_title_detached(&shared, &ask);
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        titles(&shared).len(),
        1,
        "a gate answered by the standing title spawned an ask"
    );
}
