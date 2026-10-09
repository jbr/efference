//! Tests for the title check (docs/session-title.md): the detached task appends
//! the answer whenever it lands, and the predicate stays silent while the
//! standing title answers it.

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
    mock(line, None).0
}

/// [`mock_client`] with a count of the requests it has read, and, when `hold` is
/// given, each answer held until the test sends on it — so a peek can be kept in
/// flight while the test moves the log.
fn mock(
    line: &str,
    hold: Option<std::sync::mpsc::Receiver<()>>,
) -> (Client, Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let line = line.to_string();
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    std::thread::spawn(move || {
        let mut served = 0;
        while served < 2 {
            let Ok((mut sock, _)) = listener.accept() else {
                break;
            };
            // A port the kernel hands out is reused once its test drops it, so a
            // client from another test still retrying against its own dead mock
            // can connect here. Only a title ask is answered and counted. The
            // marker is in the body, which a loaded machine may deliver after the
            // headers, so read until it arrives or the client stops sending.
            const MARKER: &str = "say what this session is for";
            let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut request = Vec::new();
            let mut buf = vec![0u8; 65536];
            while !String::from_utf8_lossy(&request).contains(MARKER) {
                match sock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buf[..read]),
                }
            }
            if !String::from_utf8_lossy(&request).contains(MARKER) {
                continue;
            }
            served += 1;
            counted.fetch_add(1, Ordering::SeqCst);
            if let Some(hold) = &hold {
                let _ = hold.recv();
            }
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
        request: Default::default(),
        base: None,
    };
    (Client::new(endpoint, "m", "sys", None), requests)
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

/// A user turn lands at a title check with an over-budget standing title: the detached
/// ask runs against the mock and the compressed answer lands as a new entry.
#[tokio::test]
async fn the_title_ask_compresses_an_over_budget_title() {
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
    assert!(
        eventually(|| titles(&shared).len() > 1).await,
        "the title ask never landed: {:?}",
        titles(&shared)
    );
    assert_eq!(titles(&shared)[1], "Fix title cadence");
}

/// A standing title with nothing after it answers the title check: no ask may run,
/// even though the mock would answer one.
#[tokio::test]
async fn the_title_ask_stays_silent_while_the_title_answers_the_check() {
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
        "a title check answered by the standing title spawned an ask"
    );
}

/// Wait for `done`, polling every 25 ms. The bound is generous on purpose: a passing
/// test returns at the first true poll, so only a real failure waits it out, while a
/// tight bound failed these tests on a loaded machine (module runtime 2–8 s).
async fn eventually(done: impl Fn() -> bool) -> bool {
    for _ in 0..1200 {
        if done() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    done()
}

fn user_turn(shared: &Session, text: &str) {
    shared
        .append_submission(
            1,
            Payload::User(vec![ContentBlock::Text(text.into())]),
            "sub".into(),
            None,
        )
        .unwrap();
}

/// Two title checks before the first ask lands — a turn's start and its trajectory's
/// end on a one-turn session — spend one peek: the second check's predicate still
/// says yes, since no `Title` exists yet, and only the in-flight guard stops a
/// second ask.
#[tokio::test]
async fn title_checks_during_an_ask_in_flight_spend_one_peek() {
    use std::sync::atomic::Ordering;
    let shared = session();
    user_turn(&shared, "start");
    let (release, hold) = std::sync::mpsc::channel();
    let (client, requests) = mock("Fix title cadence", Some(hold));
    let ask = TitleAsk::new(&client, &[], &ForkPeek::shared());
    solicit_title_detached(&shared, &ask);
    assert!(eventually(|| requests.load(Ordering::SeqCst) == 1).await);
    solicit_title_detached(&shared, &ask);
    release.send(()).unwrap();
    assert!(eventually(|| titles(&shared).len() == 1).await);
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1, "{:?}", titles(&shared));
    assert_eq!(titles(&shared), ["Fix title cadence"]);
}

/// A user turn that lands while the peek is in flight sits before that peek's
/// `Title` in the log, where the predicate counts it as answered; the ask runs
/// again so the turn the peek never saw is read.
#[tokio::test]
async fn a_user_turn_during_the_peek_is_asked_about_again() {
    use std::sync::atomic::Ordering;
    let shared = session();
    user_turn(&shared, "start");
    let (release, hold) = std::sync::mpsc::channel();
    let (client, requests) = mock("Fix title cadence", Some(hold));
    let ask = TitleAsk::new(&client, &[], &ForkPeek::shared());
    solicit_title_detached(&shared, &ask);
    assert!(eventually(|| requests.load(Ordering::SeqCst) == 1).await);
    user_turn(&shared, "new scope");
    solicit_title_detached(&shared, &ask);
    release.send(()).unwrap();
    assert!(eventually(|| requests.load(Ordering::SeqCst) == 2).await);
    release.send(()).unwrap();
    assert!(eventually(|| titles(&shared).len() == 2).await);
}
