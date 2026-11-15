//! Composer discovery over the two listener stacks (docs/control-api.md,
//! docs/web-bind.md): the routes are mounted under each session's prefix, the socket
//! answers without a credential, and a protected TCP bind refuses an anonymous read
//! before the route runs.

use super::*;

/// A host serving `one` and `two`, each in its own directory under `roots`.
async fn hosting_two(roots: &Roots) -> Arc<Host> {
    let host = Host::new(one_connection_registry(), baseline_env());
    for id in ["one", "two"] {
        let started = runtime::started(runtime::boot(&roots.0.join(id)), host.swansong()).await;
        host.adopt(id.into(), started)
            .expect("the id is free in this test");
    }
    host
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn path_candidates_read_the_sessions_working_directory_over_the_socket() {
    let roots = Roots::new();
    let host = hosting_two(&roots).await;
    std::fs::write(roots.0.join("one").join("a.txt"), "a").expect("the entry is written");
    let server = host_server(&host).await;

    let answer = server
        .get("/sessions/one/v1/discovery/paths?prefix=a")
        .await;
    answer.assert_status(200);
    let body = json(answer.body());
    let working = roots.0.join("one");
    assert_eq!(body["directory"], working.display().to_string());
    assert_eq!(body["entries"][0]["name"], "a.txt");
    assert_eq!(body["entries"][0]["original"], "a.txt");
    assert_eq!(
        body["entries"][0]["path"],
        working.join("a.txt").display().to_string()
    );
    assert_eq!(body["entries"][0]["target"], "file");
    assert_eq!(body["entries"][0]["symlink"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_candidates_are_the_hosts_listing_rows() {
    let roots = Roots::new();
    let host = hosting_two(&roots).await;
    let server = host_server(&host).await;

    let answer = server
        .get("/sessions/one/v1/discovery/agents?prefix=@on")
        .await;
    answer.assert_status(200);
    let body = json(answer.body());
    assert_eq!(body["prefix"], "@on");
    assert_eq!(body["agents"].as_array().expect("an array").len(), 1);
    // The row is the listing's own, not a narrower shape: the submission gate checks
    // the same `Host::listing()` universe.
    assert_eq!(
        body["agents"][0],
        serde_json::to_value(host.listed("one").expect("one is listed")).unwrap()
    );

    let all = server
        .get("/sessions/one/v1/discovery/agents?prefix=")
        .await;
    all.assert_status(200);
    let all = json(all.body());
    let ids: Vec<String> = all["agents"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|agent| agent["id"].as_str().expect("an id").to_string())
        .collect();
    assert_eq!(ids, ["one", "two"]);

    let capped = server
        .get("/sessions/one/v1/discovery/agents?prefix=&limit=1")
        .await;
    capped.assert_status(200);
    let capped = json(capped.body());
    assert_eq!(capped["agents"].as_array().expect("an array").len(), 1);
    assert_eq!(capped["truncated"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_starting_session_is_an_agent_candidate_the_gate_accepts() {
    let roots = Roots::new();
    let host = hosting_two(&roots).await;
    // A third id that is only starting: listed and addressable, with no session yet.
    host.reserve_starting("three", &roots.0.join("three"));
    let server = host_server(&host).await;

    let answer = server
        .get("/sessions/one/v1/discovery/agents?prefix=")
        .await;
    answer.assert_status(200);
    let body = json(answer.body());
    let ids: Vec<String> = body["agents"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|agent| agent["id"].as_str().expect("an id").to_string())
        .collect();
    assert_eq!(ids, ["one", "three", "two"]);

    // The submission gate accepts an agent target exactly when `Host::listed` returns a
    // row (docs/control-api.md), so discovery's ids and the gate's universe are the same
    // set, starting sessions included.
    let gate: Vec<String> = host
        .ids()
        .into_iter()
        .filter(|id| host.listed(id).is_some())
        .collect();
    assert_eq!(ids, gate);

    // The starting row is the listing's own: status `starting`, no title yet.
    assert_eq!(body["agents"][1]["status"], "starting");
    assert_eq!(
        body["agents"][1],
        serde_json::to_value(host.listed("three").expect("three is listed")).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_outside_the_working_directory_is_listed_and_nothing_is_written() {
    use crate::config::policy::{Verdict, classify};
    let roots = Roots::new();
    let working = roots.0.join("one");
    let outside = roots.0.join("two");

    // The session's own config, loaded through the production path: `read` is mounted
    // and trusted, the working directory is granted, and `../two` is an explicit
    // no-access entry, so the outside directory falls outside every grant. `[paths]`
    // has no deny verdict — a no-access entry only keeps the fast path from clearing —
    // so a call there is not cleared: it goes to `Gate`, the approval rung, not a
    // refusal.
    let config_path = working.join(crate::config::DEFAULT_PATH);
    std::fs::write(
        &config_path,
        "[tools]\n\
         \"efference.read\" = { enabled = true, granted = true }\n\
         \n\
         [paths]\n\
         \".\" = true\n\
         \"../two\" = false\n",
    )
    .expect("the session config is written");
    std::fs::write(outside.join("b.txt"), "b").expect("the outside entry is written");

    let host = Host::new(one_connection_registry(), baseline_env());
    let mut boot = runtime::boot(&working);
    boot.role = crate::config::load_in(&working, None).expect("the session config loads");
    let one = runtime::started(boot, host.swansong()).await;
    host.adopt("one".into(), one)
        .expect("the id is free in this test");
    let two = runtime::started(runtime::boot(&outside), host.swansong()).await;
    host.adopt("two".into(), two)
        .expect("the id is free in this test");

    let session = host.session("one").expect("one is hosted");
    assert_eq!(
        session.config_path(),
        Some(config_path.as_path()),
        "the session runs the config written above"
    );
    let directories = session.directories();
    let read = |path| ToolCall::new("read", "toolu_d", serde_json::json!({"paths": [path]}));

    // The session's actual policy: a read inside the working directory clears, a read
    // of the outside entry does not.
    let before = session.effective_config();
    let paths_before = before.paths.clone();
    let tools_before = before.tools.clone();
    assert_eq!(
        classify(&directories, &before, &read("b.txt")),
        Verdict::Clear
    );
    assert_eq!(
        classify(&directories, &before, &read("../two/b.txt")),
        Verdict::Gate,
        "the session's grants do not reach the outside directory"
    );

    let entries_before = session.log_for_test().lock().unwrap().entries().len();
    let config_before = std::fs::read(&config_path).expect("the config is readable");
    let server = host_server(&host).await;

    // Discovery lists the outside entry regardless of the policy above: it reads names
    // and consults no grant.
    let answer = server
        .get("/sessions/one/v1/discovery/paths?prefix=../two/b")
        .await;
    answer.assert_status(200);
    let body = json(answer.body());
    assert_eq!(body["directory"], outside.display().to_string());
    assert_eq!(body["entries"][0]["name"], "b.txt");
    assert_eq!(body["entries"][0]["original"], "../two/b.txt");
    assert_eq!(
        body["entries"][0]["path"],
        outside.join("b.txt").display().to_string()
    );
    assert_eq!(body["entries"][0]["target"], "file");

    // Both discovery routes answer, and neither appends a log entry (the fixture's log
    // is in memory, so its entries are the whole log), touches the session's config
    // file, nor changes the effective grants.
    server
        .get("/sessions/one/v1/discovery/agents?prefix=")
        .await
        .assert_status(200);
    assert_eq!(
        session.log_for_test().lock().unwrap().entries().len(),
        entries_before,
        "discovery appends no log entry"
    );
    assert_eq!(
        std::fs::read(&config_path).expect("the config is still readable"),
        config_before,
        "discovery leaves the session's config file unchanged"
    );
    let after = session.effective_config();
    assert_eq!(
        after.paths, paths_before,
        "discovery leaves the effective path grants unchanged"
    );
    assert_eq!(after.tools, tools_before, "discovery grants no tool");
    assert_eq!(
        classify(&directories, &after, &read("../two/b.txt")),
        Verdict::Gate,
        "the outside read is still outside the effective grants"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_protected_tcp_bind_asks_for_a_token_before_discovery() {
    let roots = Roots::new();
    let host = hosting_two(&roots).await;
    let (server, bearer, _state) = network_host_server(&host).await;
    let path = "/sessions/one/v1/discovery/paths?prefix=a";

    server.get(path).await.assert_status(401);
    server
        .get(path)
        .with_request_header("authorization", bearer)
        .await
        .assert_status(200);
}
