//! `POST /sessions`: what a creation request establishes, and what it refuses.
//!
//! The environment cases are the ones the user-wide `env_hook` introduced — a
//! request that supplies `env` is used outright, one that omits it has a base
//! established for its own directory (docs/env.md). Every test here stops at
//! preparation rather than booting, because a booted session reaches a provider.

use super::*;

/// An omitted environment is established for the request's own directory: the
/// hook runs there, inheriting the launch baseline, and its failure fails the
/// creation before anything is minted (docs/env.md). The creation answers once its
/// id is reserved; the failure is read from the starting session's state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_omitted_environment_runs_the_hook_for_that_directory() {
    let roots = Roots::new();
    let ran = roots.0.join("hook-ran");
    // Records what it was given, then fails: a hook's stderr is discarded, so the
    // exit code is all the host has to report.
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "printf 'cwd=%s\\nhost=%s\\n' \"$PWD\" \"$FROM_BASELINE\" > {}; exit 7",
            ran.display()
        ),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    let state = created_state(&server, &stops_at_boot(&roots.0.join("one"))).await;
    assert!(failure_reason(&state).contains("exited 7"), "{state}");
    assert!(host.ids().is_empty(), "a failed launch mints no session");
    assert!(
        !roots.0.join("one/.efference").exists(),
        "a failed launch leaves no session directory"
    );

    let seen = std::fs::read_to_string(&ran).expect("the hook ran");
    assert!(
        seen.contains(&format!("cwd={}", roots.0.join("one").display())),
        "the hook ran in the session's own directory: {seen}"
    );
    assert!(
        seen.contains("host=inherited"),
        "the hook inherited the host's launch baseline: {seen}"
    );
}

/// A creation whose `cwd` is inside a worktree establishes its environment at the
/// project, where resume establishes it too, and a failed launch leaves no session
/// directory under the project or the worktree. A booted session's storage being
/// `<project>/.efference/<id>` needs a provider to reach, so it is not exercised here
/// (docs/control-channel.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worktree_cwd_runs_the_hook_at_the_project() {
    let roots = Roots::new();
    let project = roots.0.join("one");
    let worktree = project.join(".efference/worktrees/w");
    std::fs::create_dir_all(&worktree).unwrap();
    let ran = roots.0.join("hook-ran");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf 'cwd=%s\\n' \"$PWD\" > {}; exit 7", ran.display()),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    let state = created_state(&server, &stops_at_boot(&worktree)).await;
    assert!(failure_reason(&state).contains("exited 7"), "{state}");

    let seen = std::fs::read_to_string(&ran).expect("the hook ran");
    assert!(
        seen.contains(&format!("cwd={}\n", project.display())),
        "the hook ran at the project, not the worktree: {seen}"
    );
    assert!(
        !project.join(".efference/fresh").exists() && !worktree.join(".efference").exists(),
        "a failed launch leaves no session directory"
    );
}

/// A supplied environment is the base outright: the hook is never run, so a
/// request that brings its own environment is unaffected by a broken one
/// (docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supplied_environment_never_runs_the_hook() {
    let roots = Roots::new();
    let ran = roots.0.join("hook-ran");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(": > {}; exit 7", ran.display()),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    let state = created_state(
        &server,
        &serde_json::json!({
            "name": "fresh",
            "cwd": roots.0.join("one"),
            "model": "test/m",
            "config": "missing.toml",
            "env": {"HOME": "/h"},
        }),
    )
    .await;
    // Fails at boot, on the same missing config every branch here uses.
    assert!(
        failure_reason(&state).contains("could not prepare session"),
        "{state}"
    );
    assert!(
        !ran.exists(),
        "a request carrying its own environment must not run the hook"
    );
    assert!(
        host.ids().is_empty(),
        "a failed preparation mints no session"
    );
}

/// One name, one session. A create checks the name is free *before* it awaits the
/// hook and preparation, so two creates naming the same session in different
/// directories both pass that check and arrive at the insert together; first in
/// wins, and the loser is refused and its runtime shut down rather than replacing
/// the winner's entry and leaving a running session unreachable.
///
/// Tested at the insert rather than through two requests, because two creates that
/// both *reach* the insert need a provider — every path short of it stops earlier
/// (docs/control-api.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_already_hosted_is_refused_rather_than_replaced() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let runtime_for = async |dir: &str| {
        let cwd = roots.0.join(dir);
        runtime::started(
            runtime::boot_in(&cwd, &cwd.join(".efference").join("fresh")),
            host.swansong(),
        )
        .await
    };

    let winner = runtime_for("one").await;
    let winning_cwd = Arc::clone(winner.session()).cwd().to_path_buf();
    host.adopt_with("fresh".to_string(), winner, None, false)
        .expect("the first adopt takes a free name");

    let loser = runtime_for("two").await;
    let error = host
        .adopt_with("fresh".to_string(), loser, None, false)
        .expect_err("the second adopt finds the name taken");
    assert!(error.contains("already hosted"), "{error}");

    assert_eq!(host.ids(), ["fresh"], "one session under that name");
    assert_eq!(
        host.session("fresh").expect("still hosted").cwd(),
        winning_cwd,
        "and it is the first one, not the session that lost",
    );
    host.swansong().shut_down().await;
}

/// `QUERY /sessions` stays a preview: it resolves options and nothing else, so it
/// establishes no environment and runs no hook. Minting nothing is not by itself
/// evidence of that — the hook writes a marker, and no marker means it never ran
/// (docs/control-api.md, docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preview_never_runs_the_hook() {
    let roots = Roots::new();
    let ran = roots.0.join("hook-ran");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(": > {}", ran.display()),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    // No `env` at all, which is exactly the request a preview would establish an
    // environment for if it did.
    server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!({
            "cwd": roots.0.join("one"),
            "connection": "test",
            "model": "m",
        }))
        .await
        .assert_status(200);

    assert!(host.ids().is_empty(), "a preview mints nothing");
    assert!(
        !ran.exists(),
        "a preview must not establish an environment, so it must not run the hook"
    );
    host.swansong().shut_down().await;
}

/// Host shutdown cancels an establishment in progress, and the creation ends
/// without minting. The hook here is killed mid-run — nothing about that rolls
/// back whatever the hook had already done outside the process (docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_shutdown_cancels_an_establishment_in_progress() {
    let roots = Roots::new();
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "sleep 30".to_string(),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    // Shut down while the hook is still running: the hook outlives a test's
    // patience, so the cancellation is the only way this request ends.
    let shutting_down = Arc::clone(&host);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        shutting_down.swansong().shut_down().await;
    });

    let created = server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&roots.0.join("one")))
        .await;
    created.assert_status(201);
    let starting = host
        .starting("fresh")
        .expect("starting while the hook runs");
    let ended = starting.finished().await;
    assert!(
        ended.is_err_and(|reason| reason.contains("the host shut down")),
        "the shutdown cancelled the preparation"
    );
    assert!(
        host.ids().is_empty(),
        "a cancelled establishment mints no session"
    );
    assert!(
        !roots.0.join("one/.efference").exists(),
        "and leaves no session directory"
    );
}

/// With no hook there is nothing to run, so establishment returns the host's own
/// environment and creation proceeds to the same preparation failure every other
/// branch reaches (docs/env.md).
///
/// What this shows is the *absence* of an environment error, not that the base is
/// the baseline: that would need a booted session, and every branch here stops at
/// preparation to avoid a provider. `env_table`'s own tests cover the no-hook
/// passthrough, and the hook branch above covers what the baseline is used for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_hook_creation_reaches_preparation() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), marked_baseline());
    let server = host_server(&host).await;

    let state = created_state(&server, &stops_at_boot(&roots.0.join("one"))).await;
    let body = failure_reason(&state);
    assert!(
        body.contains("could not prepare session"),
        "an omitted environment with no hook reaches boot rather than failing at \
         environment establishment: {body}"
    );
    assert!(
        !body.contains("env:"),
        "and reports nothing from environment establishment: {body}"
    );
    assert!(
        host.ids().is_empty(),
        "a failed preparation mints no session"
    );
}

/// Creation is validated before anything is reserved: a malformed body, an id that
/// is not a plain segment, a relative cwd, and a taken id each answer `400` with the
/// reason and mint nothing. Validation is a property of the request, so a network
/// listener answers the same way. A config that does not exist is preparation's to
/// find, and fails the starting session instead (`a_supplied_environment_…`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_validates_its_body_on_every_listener() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt(
        "one".into(),
        runtime::started(runtime::boot(&roots.0.join("one")), host.swansong()).await,
    )
    .expect("the id is free in this test");
    let server = host_server(&host).await;
    let base = |name: &str, cwd: &Path| {
        serde_json::json!({
            "name": name, "cwd": cwd, "model": "test/m",
            "env": {"HOME": "/h"},
        })
    };
    let cases: Vec<(Value, &str)> = vec![
        // Not an object at all. A body that merely omits `env` is well-formed —
        // that omission is what asks the host to establish one (docs/env.md).
        (serde_json::json!(["not", "an", "object"]), "invalid body"),
        (base("../up", &roots.0), "not a single plain path segment"),
        (base("fresh", Path::new("relative")), "cwd must be absolute"),
        (base("one", &roots.0), "already hosted"),
    ];
    for (body, reason) in &cases {
        server
            .post("/sessions")
            .with_json_body(body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    assert!(
        !roots.0.join("two/.efference").exists(),
        "a failed creation mints nothing"
    );
    assert_eq!(host.ids(), ["one"]);

    // The same rejections on a network listener: same status, same reasons.
    let (network, bearer, _state) = network_host_server(&host).await;
    for (body, reason) in &cases {
        network
            .post("/sessions")
            .with_request_header(KnownHeaderName::Authorization, bearer.clone())
            .with_json_body(body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    assert_eq!(host.ids(), ["one"]);

    host.swansong().shut_down().await;
}

/// The accepted temperature is the resolver's rule, so it holds at creation's
/// HTTP boundary too: a value `u8` accepts but the range does not is refused
/// there, not left for the first request to the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_enforces_the_temperature_range() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    for (temperature, status) in [(200u16, 400), (260, 400)] {
        server
            .post("/sessions")
            .with_json_body(&serde_json::json!({
                "cwd": roots.0.join("one"), "env": {}, "temperature": temperature,
            }))
            .await
            .assert_status(status);
    }
    assert!(host.ids().is_empty());
    host.swansong().shut_down().await;
}

/// A host whose state database, and so its session-id register, is a file under
/// this test's root.
fn host_with_register(roots: &Roots) -> Arc<Host> {
    Host::new(
        one_connection_registry().with_history(crate::config::history::History::at(
            roots.0.join("state.db"),
        )),
        baseline_env(),
    )
}

/// An explicit name is reserved host-wide: another root holding it refuses the
/// creation before anything is prepared, and a creation that fails after
/// reserving gives the name back (docs/session-title.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_name_held_under_another_root_is_refused() {
    let roots = Roots::new();
    let host = host_with_register(&roots);
    let server = host_server(&host).await;
    let register = crate::session_ids::SessionIds::at(roots.0.join("state.db"));
    register
        .reserve("fresh", &roots.0.join("two"))
        .expect("the name is free");

    server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&roots.0.join("one")))
        .await
        .assert_status(400)
        .assert_body_contains("is taken by a session under");

    register.release("fresh", &roots.0.join("two"));
    let state = created_state(&server, &stops_at_boot(&roots.0.join("one"))).await;
    assert!(
        failure_reason(&state).contains("could not prepare session"),
        "{state}"
    );
    assert_eq!(
        register.holder("fresh"),
        None,
        "a creation that failed released the name it reserved"
    );
}
