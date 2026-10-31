//! `wait` over a host, as `efference mcp` reaches it (docs/operating.md): the statuses
//! the mount has not shown of the agents it created or named, nothing for a working
//! one, and an end once.

use super::*;
use crate::control_tools::Wait;
use crate::log::{Approval, CallId};
use std::future::Future;

/// Watch `id` as a creation over MCP does: from the status its answer showed.
fn created(executor: &ClientExecutor, host: &Host, id: &str) {
    executor
        .watch(id, host.status_view_of(id).as_ref())
        .expect("the host is reachable");
}

/// One `wait` call, owned so it can be spawned.
fn wait(
    control: &Control,
    agent: Option<&str>,
    timeout_s: u64,
) -> impl Future<Output = Result<String, String>> + Send + 'static {
    let call = Wait {
        agent: agent.map(str::to_string),
        timeout_s: Some(timeout_s),
    };
    let control = control.clone();
    async move {
        call.execute(control, RequestContext::default())
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

/// A created agent's gate is what the next `wait` returns, read as its reviewer: the
/// held call, its handle, and the `decide` call that answers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_created_agents_gate_arrives_through_wait() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, executor) = mount(&host).await;
    created(&executor, &host, "child");

    let (seq, _decision) = hold(&host, "child", &bash("toolu_1", "ls ~"));
    let said = wait(&control, None, 10).await.unwrap();
    let mut lines = said.lines();
    assert_eq!(
        lines.next(),
        Some(format!("child: waiting for your decision on bash `ls ~` (seq {seq})").as_str()),
        "{said}"
    );
    assert!(
        said.contains(&format!("decide: decide(agent: \"child\", seq: \"{seq}\"")),
        "{said}"
    );
    host.swansong().shut_down().await;
}

/// With nothing new, `wait` blocks: the decision sets the agent working, which does not
/// end the wait, and its next gate does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wait_with_nothing_new_blocks_until_the_next_decision_point() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, executor) = mount(&host).await;
    created(&executor, &host, "child");
    let (_, _first) = hold(&host, "child", &bash("toolu_1", "ls ~"));
    let said = wait(&control, None, 10).await.unwrap();
    assert!(said.contains("`ls ~`"), "{said}");

    let pending = tokio::spawn(wait(&control, None, 30));
    let session = host.session("child").unwrap();
    assert!(session.resolve_approval(
        &CallId("toolu_1".into()),
        Approval::Approve,
        Default::default()
    ));
    assert_eq!(
        session.status(),
        crate::daemon::status::SessionStatus::Running
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !pending.is_finished(),
        "a working status does not end the wait"
    );

    let (_, _second) = hold(&host, "child", &bash("toolu_2", "pwd"));
    let said = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .expect("the gate ends the wait")
        .unwrap()
        .unwrap();
    assert!(
        said.starts_with("child: waiting for your decision on bash `pwd`"),
        "{said}"
    );
    host.swansong().shut_down().await;
}

/// Each watched agent's new status arrives; those that are new together come back in
/// one answer, one status per paragraph.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_watched_agents_statuses_both_arrive() {
    let roots = Roots::new();
    let host = hosting(&roots, &["a", "b"]).await;
    let (control, executor) = mount(&host).await;
    created(&executor, &host, "a");
    created(&executor, &host, "b");
    let (_, _a) = hold(&host, "a", &bash("toolu_a", "ls a"));
    let (_, _b) = hold(&host, "b", &bash("toolu_b", "ls b"));

    let mut said = Vec::new();
    while said.len() < 2 {
        let answer = wait(&control, None, 10).await.unwrap();
        said.extend(answer.split("\n\n").map(str::to_string));
    }
    // Two answers come in whichever order the followers read the gates.
    said.sort();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        said[0].starts_with("a: waiting for your decision on bash `ls a`"),
        "{said:?}"
    );
    assert!(
        said[1].starts_with("b: waiting for your decision on bash `ls b`"),
        "{said:?}"
    );
    host.swansong().shut_down().await;
}

/// A `wait` naming an agent watches it from then on: its status now is new to this
/// mount, and later ones arrive without naming it again. An agent the host does not hold
/// is an error naming what it does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn naming_an_agent_watches_it() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, _) = mount(&host).await;

    let err = wait(&control, Some("ghost"), 10).await.unwrap_err();
    assert_eq!(err, "no agent \"ghost\" on this host; it holds child");

    let said = wait(&control, Some("child"), 10).await.unwrap();
    assert!(said.starts_with("child: idle, its turn is done"), "{said}");

    let (_, _decision) = hold(&host, "child", &bash("toolu_1", "ls ~"));
    let said = wait(&control, None, 10).await.unwrap();
    assert!(
        said.starts_with("child: waiting for your decision on bash `ls ~`"),
        "{said}"
    );
    host.swansong().shut_down().await;
}

/// A mount that created nothing and named nothing has nothing to wait for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_watched_refuses() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, _) = mount(&host).await;
    let err = wait(&control, None, 10).await.unwrap_err();
    assert_eq!(
        err,
        "you watch no agents: create one, or name one with `agent`"
    );
    host.swansong().shut_down().await;
}

/// `timeout_s` passing with nothing new answers each watched agent's line now, marked
/// as not new: one already shown, one working.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timeout_answers_the_current_statuses_marked_not_new() {
    let roots = Roots::new();
    let host = hosting(&roots, &["a", "b"]).await;
    let (control, executor) = mount(&host).await;
    created(&executor, &host, "a");
    created(&executor, &host, "b");
    host.session("b").unwrap().set_phase(LoopPhase::Requesting);

    let said = wait(&control, None, 1).await.unwrap();
    assert_eq!(
        said,
        "nothing new after 1s; where each agent is now (not new):\n\
         a: idle, its turn is done\n\
         b: working"
    );
    host.swansong().shut_down().await;
}

/// An agent that leaves the host is reported ended once, then no longer watched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_agent_is_reported_once_then_dropped() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, executor) = mount(&host).await;
    created(&executor, &host, "child");
    Archive {
        agent: "child".into(),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap();

    let said = wait(&control, None, 10).await.unwrap();
    assert_eq!(said, "child: ended; its log stays on disk");
    let err = wait(&control, None, 10).await.unwrap_err();
    assert_eq!(
        err,
        "you watch no agents: create one, or name one with `agent`"
    );
    host.swansong().shut_down().await;
}
