//! Tests for the title check (docs/session-title.md): the detached task appends
//! the answer whenever it lands, and the predicate stays silent while the
//! standing title answers it.

use super::*;
use crate::log::{Provider, Title, TitleAuthor};
use crate::protocol::state::SessionIdentity;
use crate::testing::upstream::{Canned, ScriptedUpstream, Turnstile};

fn session() -> Arc<Session> {
    let (shared, _rx) = Session::in_memory();
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

/// A client against an upstream answering every request, title asks included, with a
/// one-line text answer (a Responses message output item): `line`.
fn mock_client(line: &str) -> Client {
    mock(line, None).0
}

/// [`mock_client`], and its upstream, which counts the asks; with `hold`, each answer
/// waits at that turnstile until the test admits it, so a triage can be kept in flight
/// while the test moves the log.
fn mock(line: &str, hold: Option<&Turnstile>) -> (Client, ScriptedUpstream) {
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
    let hold = hold.cloned();
    let upstream = ScriptedUpstream::scripting_title_asks(move |_| {
        let answer = Canned::stream(body.clone());
        match &hold {
            Some(turnstile) => answer.held(turnstile),
            None => answer,
        }
    });
    let endpoint = upstream.endpoint(Provider::OpenAICodex, "/backend-api/codex/responses");
    (Client::new(endpoint, "m", "sys", None), upstream)
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
        .set_title(Title::new(
            "Implementing result-before-triage capture with later verdicts and a long tail",
            TitleAuthor::Triage,
        ))
        .unwrap();
    shared
        .append_submission(
            1,
            Payload::user("new scope"),
            "sub".into(),
            Some("web".into()),
        )
        .unwrap();
    let client = mock_client("Fix title cadence");
    let ask = TitleAsk::new(&client, &[], &Triager::shared());
    solicit_title_detached(&shared, &ask);
    shared
        .wait_until("the title ask landed", || titles(&shared).len() > 1)
        .await;
    assert_eq!(titles(&shared)[1], "Fix title cadence");
}

/// A standing title with nothing after it answers the title check: no ask may run,
/// even though the mock would answer one.
#[tokio::test(start_paused = true)]
async fn the_title_ask_stays_silent_while_the_title_answers_the_check() {
    let shared = session();
    shared
        .set_title(Title::new("Fix title cadence", TitleAuthor::Triage))
        .unwrap();
    let client = mock_client("A different line");
    let ask = TitleAsk::new(&client, &[], &Triager::shared());
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

fn user_turn(shared: &Session, text: &str) {
    shared
        .append_submission(1, Payload::user(text), "sub".into(), None)
        .unwrap();
}

/// Two title checks before the first ask lands — a turn's start and its trajectory's
/// end on a one-turn session — spend one triage: the second check's predicate still
/// says yes, since no `Title` exists yet, and only the in-flight guard stops a
/// second ask.
#[tokio::test(start_paused = true)]
async fn title_checks_during_an_ask_in_flight_spend_one_peek() {
    let shared = session();
    user_turn(&shared, "start");
    let release = Turnstile::new();
    let (client, upstream) = mock("Fix title cadence", Some(&release));
    let ask = TitleAsk::new(&client, &[], &Triager::shared());
    solicit_title_detached(&shared, &ask);
    shared
        .wait_until_with(&upstream, "the ask reached the mock", || {
            upstream.count() == 1
        })
        .await;
    solicit_title_detached(&shared, &ask);
    release.admit();
    shared
        .wait_until("the title landed", || titles(&shared).len() == 1)
        .await;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(upstream.count(), 1, "{:?}", titles(&shared));
    assert_eq!(titles(&shared), ["Fix title cadence"]);
}

/// A user turn that lands while the triage is in flight sits before that triage's
/// `Title` in the log, where the predicate counts it as answered; the ask runs
/// again so the turn the triage never saw is read.
#[tokio::test]
async fn a_user_turn_during_the_peek_is_asked_about_again() {
    let shared = session();
    user_turn(&shared, "start");
    let release = Turnstile::new();
    let (client, upstream) = mock("Fix title cadence", Some(&release));
    let ask = TitleAsk::new(&client, &[], &Triager::shared());
    solicit_title_detached(&shared, &ask);
    shared
        .wait_until_with(&upstream, "the ask reached the mock", || {
            upstream.count() == 1
        })
        .await;
    user_turn(&shared, "new scope");
    solicit_title_detached(&shared, &ask);
    release.admit();
    shared
        .wait_until_with(&upstream, "the second ask reached the mock", || {
            upstream.count() == 2
        })
        .await;
    release.admit();
    shared
        .wait_until("the second title landed", || titles(&shared).len() == 2)
        .await;
}
