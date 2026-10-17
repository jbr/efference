use super::*;
use crate::daemon::starting::{Starting, StartingParts};
use crate::persist::Log;
use trillium_testing::TestServer;

const CAP: Duration = Duration::from_millis(40);

#[test]
fn query_value_decodes_the_named_value() {
    let query = "connection=open%20router&cwd=%2FUsers%2Fjbr%2Fcode";
    assert_eq!(query_value(query, "connection").unwrap(), "open router");
    assert_eq!(query_value(query, "cwd").unwrap(), "/Users/jbr/code");
    assert_eq!(query_value(query, "config"), None);
}

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

fn starting() -> Arc<Starting> {
    Starting::new(StartingParts {
        id: "s".to_string(),
        directories: crate::policy::Directories::at(&std::env::current_dir().unwrap()),
        resume: false,
        created_by: None,
        detached: false,
    })
}

/// `GET /` against `settle_and_state_within` for `shared` under `cap`.
async fn settle_answer(shared: &Arc<Session>, cap: Duration) -> (Option<String>, Value) {
    let shared = Arc::clone(shared);
    let server = TestServer::new(move |conn: Conn| {
        let shared = Arc::clone(&shared);
        async move { settle_and_state_within(&shared, conn, cap).await }
    })
    .await;
    answer(server.get("/").await)
}

async fn starting_answer(starting: &Arc<Starting>, cap: Duration) -> (Option<String>, Value) {
    let starting = Arc::clone(starting);
    let server = TestServer::new(move |conn: Conn| {
        let starting = Arc::clone(&starting);
        async move { settle_starting_within(&starting, conn, cap).await }
    })
    .await;
    answer(server.get("/").await)
}

fn answer(response: trillium_testing::ConnTest) -> (Option<String>, Value) {
    response.assert_status(200);
    let header = response
        .response_headers()
        .get_str(SETTLED_HEADER)
        .map(String::from);
    (header, serde_json::from_str(response.body()).unwrap())
}

#[tokio::test]
async fn a_settled_session_answers_settled_true_at_once() {
    let session = session();
    let (header, state) = settle_answer(&session, Duration::from_secs(30)).await;
    assert_eq!(header.as_deref(), Some("true"));
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
}

#[tokio::test]
async fn an_unsettled_session_answers_settled_false_at_the_cap() {
    let session = session();
    session.set_phase(crate::daemon::LoopPhase::Requesting);
    let (header, state) = settle_answer(&session, CAP).await;
    assert_eq!(header.as_deref(), Some("false"));
    assert_eq!(
        state["loop_phase"],
        serde_json::json!({"phase": "requesting"})
    );
}

#[tokio::test]
async fn a_session_that_settles_inside_the_cap_answers_true() {
    let session = session();
    session.set_phase(crate::daemon::LoopPhase::Requesting);
    let waker = Arc::clone(&session);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        waker.set_phase(crate::daemon::LoopPhase::Idle);
    });
    let (header, _) = settle_answer(&session, Duration::from_secs(30)).await;
    assert_eq!(header.as_deref(), Some("true"));
}

#[tokio::test]
async fn a_starting_session_that_has_not_started_answers_false_at_the_cap() {
    let starting = starting();
    let (header, state) = starting_answer(&starting, CAP).await;
    assert_eq!(header.as_deref(), Some("false"));
    assert!(
        state.get("starting").is_some(),
        "the starting state: {state}"
    );
}

#[tokio::test]
async fn a_starting_session_that_failed_answers_settled_true() {
    let starting = starting();
    assert!(starting.failed("the hook exited 7".to_string()));
    let (header, state) = starting_answer(&starting, Duration::from_secs(30)).await;
    assert_eq!(header.as_deref(), Some("true"));
    assert!(
        state.get("starting").is_some(),
        "the starting state: {state}"
    );
}

#[tokio::test]
async fn a_starting_session_that_started_and_settled_answers_true() {
    let starting = starting();
    assert!(starting.started(session()));
    let (header, state) = starting_answer(&starting, Duration::from_secs(30)).await;
    assert_eq!(header.as_deref(), Some("true"));
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
}
