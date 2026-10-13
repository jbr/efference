use super::scaffold::self_surface_grants;
use super::*;
use crate::worktree::testing::{commit, git};
use std::path::Path;

/// The re-serve guard (docs/control-channel.md): a fresh name mints its dir, and a name whose
/// dir already exists is refused rather than opening a second seq space into the same log. The
/// error names the dir and the log inside it, so the operator can find the prior session.
#[test]
fn serve_mints_a_fresh_session_dir_and_refuses_an_existing_one() {
    let base = std::env::temp_dir().join(format!("efference-mint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("sess");

    mint_session_dir(&dir).expect("a fresh name mints its dir");
    assert!(dir.is_dir(), "the parent is created recursively with it");

    let err = mint_session_dir(&dir).expect_err("a second serve of the same name is refused");
    let msg = err.to_string();
    assert!(
        msg.contains(&dir.display().to_string()),
        "names the dir: {msg}"
    );
    assert!(msg.contains(LOG_NAME), "points at the intact log: {msg}");

    // An empty dir is refused too — occupancy is the dir, not its contents.
    let bare = base.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    assert!(mint_session_dir(&bare).is_err());

    let _ = std::fs::remove_dir_all(&base);
}

/// A temp project root, removed when the test ends.
pub(super) struct Root(PathBuf);

impl Root {
    pub(super) fn new(label: &str) -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-boot-{label}-{nonce:016x}"));
        std::fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }

    pub(super) fn session_dir(&self, id: &str) -> PathBuf {
        self.0.join(BASE_DIR).join(id)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `prepare` mints into the shared system temp dir, so the tests that count
/// scratch dirs there run one at a time.
pub(super) static PREPARING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The scratch dirs [`mint_scratch_dir`] could have made: `efference-` plus
/// exactly sixteen hex digits, which no other temp name in this suite matches.
pub(super) fn scratch_dirs() -> std::collections::BTreeSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return Default::default();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("efference-"))
                .is_some_and(|rest| rest.len() == 16 && rest.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .collect()
}

/// A session request against the Codex arm, whose capabilities come from the
/// provider's catalog: the test seeds the cache that lookup reads, so
/// preparation runs to the end without a provider. Each request writes its own
/// `auth.json` and seeds under its own account id, so tests running in parallel
/// do not share a catalog entry.
pub(super) fn request(root: &Path, id: &str, effort: Option<&str>) -> SessionRequest {
    let registry = codex_registry(root, id);
    let inference = registry.resolve(Some("oai/gpt-5.6"), None, None).unwrap();
    SessionRequest {
        directories: crate::policy::Directories::at(root),
        base_env: base_env(),
        session_dir: root.join(BASE_DIR).join(id),
        config: None,
        inference,
        temperature: None,
        effort: effort.map(str::to_string),
        upstream: Vec::new(),
        autonomous: false,
        reauth: Vec::new(),
        keep_scratch: false,
        worktree: false,
        host: None,
        lineage: crate::env::Lineage::default(),
    }
}

/// The caller environment a test session starts from.
pub(super) fn base_env() -> crate::env_table::EnvSnapshot {
    crate::env_table::EnvSnapshot::new(
        ["HOME", "PATH"]
            .into_iter()
            .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
            .collect(),
    )
    .unwrap()
}

/// The registry behind [`request`]: one `oai` connection on the seeded Codex catalog.
pub(super) fn codex_registry(root: &Path, id: &str) -> crate::inference::InferenceRegistry {
    let account = format!("acct-{id}");
    let codex_home = root.join(format!("codex-home-{id}"));
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(
        codex_home.join("auth.json"),
        serde_json::json!({
            "tokens": {"access_token": "not-a-real-credential", "account_id": account}
        })
        .to_string(),
    )
    .unwrap();
    crate::api::seed_codex_catalog(crate::api::Provider::OpenAICodex, &account);
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[connections.oai]
provider = "openai-codex"
auth = { kind = "codex-cache" }
"#,
    )
    .unwrap();
    let host_env = crate::env_table::EnvSnapshot::new(
        [(
            "CODEX_HOME".to_string(),
            codex_home.to_string_lossy().into_owned(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    crate::inference::InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        host_env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
}

/// A listing served from a loopback listener, for a connection whose base URL a
/// test names. The named providers own their routes, so a described endpoint is
/// the only one a test can point at itself. Answers every request with the same
/// body — a boot reads the listing once per role that resolves capabilities — and
/// `stop` ends the thread.
struct ListingEndpoint {
    url: String,
    stop: Box<dyn FnOnce() + Send>,
}

impl ListingEndpoint {
    fn url(&self) -> &str {
        &self.url
    }

    fn stop(self) {
        (self.stop)();
    }
}

fn listing_endpoint(body: &'static str) -> ListingEndpoint {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let url = format!("http://{address}/v1");
    // Non-blocking with a stop flag, so one client that hangs up mid-response
    // cannot end the server for the next read — a refused boot closes its
    // connection as soon as it has the answer, which is exactly the case here.
    listener.set_nonblocking(true).unwrap();
    let stopping = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopping);
    let server = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut sock, _)) => {
                    // Read the whole header block before answering: a socket
                    // closed with request bytes still unread sends the client an
                    // RST, which under load loses the response it was sent.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !buf.windows(4).any(|window| window == b"\r\n\r\n") {
                        match sock.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => buf.extend_from_slice(&chunk[..read]),
                        }
                    }
                    // `connection: close` because the socket is shut after this
                    // one response: without it the client pools the connection,
                    // and a later read that reuses it before the close is seen
                    // gets a reset instead of an answer.
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(response.as_bytes());
                    let _ = sock.flush();
                    let _ = sock.shutdown(std::net::Shutdown::Both);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    ListingEndpoint {
        url,
        stop: Box::new(move || {
            stopping.store(true, Ordering::Relaxed);
            let _ = server.join();
        }),
    }
}

/// A session request against a described endpoint at `base`, whose capabilities
/// come from the listing `listing_endpoint` serves. The spotter is named in the
/// configuration because a described endpoint publishes no catalog to pick a
/// default from, which is a resolution failure before any capability read.
fn request_for(base: &str, root: &Path, id: &str, temperature: Option<u8>) -> SessionRequest {
    let config = crate::inference::InferenceConfig::from_toml(&format!(
        r#"
[defaults]
spotter = "edge/m"

[connections.edge]
dialect = "responses"
base-url = "{base}"
auth = {{ kind = "env", key = "EFFERENCE_BOOT_TEST_KEY" }}
"#
    ))
    .unwrap();
    let host_env = crate::env_table::EnvSnapshot::new(
        [(
            "EFFERENCE_BOOT_TEST_KEY".to_string(),
            "not-a-real-credential".to_string(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let inference = crate::inference::InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        host_env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
    .resolve(Some("edge/m"), None, None)
    .unwrap();
    SessionRequest {
        directories: crate::policy::Directories::at(root),
        base_env: crate::env_table::EnvSnapshot::new(
            ["HOME", "PATH"]
                .into_iter()
                .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
                .collect(),
        )
        .unwrap(),
        session_dir: root.join(BASE_DIR).join(id),
        config: None,
        inference,
        temperature,
        effort: None,
        upstream: Vec::new(),
        autonomous: false,
        reauth: Vec::new(),
        keep_scratch: false,
        worktree: false,
        host: None,
        lineage: crate::env::Lineage::default(),
    }
}

/// The scratch dir a successful boot owns, removed here as the runtime's
/// teardown would (docs/tools.md).
pub(super) fn drop_boot(boot: SessionBoot<MountedSpotter>) {
    if let Some(scratch) = boot.toolbox.scratch_dir() {
        let _ = std::fs::remove_dir_all(scratch);
    }
}

/// The error a preparation that must fail returned, cleaning up after it if it
/// unexpectedly succeeded. `Result::expect_err` wants a `Debug` boot.
pub(super) fn failure(result: Result<SessionBoot<MountedSpotter>, Error>, what: &str) -> Error {
    match result {
        Ok(boot) => {
            drop_boot(boot);
            panic!("{what}")
        }
        Err(error) => error,
    }
}

/// A request whose model refuses a boot-time parameter is rejected before the
/// session dir is minted, so the name stays free and the corrected retry takes it
/// (docs/control-channel.md). The rejection and the retry are the sequence a
/// creating client actually runs.
///
/// The refusal is a temperature on a model whose listing names accepted sampling
/// parameters without it — the one parameter refusal a source can still make
/// offline, now that the OpenAI arms report what the provider reports and validate
/// no effort levels of their own (`api/provider/codex.rs`). The custom arm's
/// listing is served by a local listener because the named providers' routes are
/// their own.
#[tokio::test]
async fn a_rejected_parameter_reserves_no_name() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("parameter");
    let dir = root.session_dir("file-nomination");
    let endpoint = listing_endpoint(
        r#"{"data":[{"id":"m","max_input_tokens":1000,"max_tokens":100,
             "supported_parameters":["top_p"]}]}"#,
    );

    let refused = failure(
        prepare(request_for(
            endpoint.url(),
            root.path(),
            "file-nomination",
            Some(50),
        ))
        .await,
        "a refused parameter must fail preparation",
    );
    assert!(
        refused.to_string().contains("does not accept"),
        "names the refusal: {refused}"
    );
    assert!(
        !dir.exists(),
        "a refused parameter mints nothing: {}",
        dir.display()
    );

    let boot = prepare(request_for(
        endpoint.url(),
        root.path(),
        "file-nomination",
        None,
    ))
    .await
    .expect("the corrected retry mints the same name");
    assert!(dir.join(LOG_NAME).is_file(), "the retry opened its log");
    assert!(dir.join(CONFIG_NAME).is_file(), "and its config snapshot");
    drop_boot(boot);
    endpoint.stop();
}

/// A failure after the mint rolls back what the attempt minted: the session dir
/// with the config snapshot and log inside it, and the scratch dir. The injected
/// failure is a `[context]` entry naming a file that is not there, which fails
/// closed well after the log is open (docs/context.md).
#[tokio::test]
async fn a_late_failure_rolls_back_what_it_minted() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("late");
    std::fs::write(
        root.path().join("efference.toml"),
        "[context]\n\"absent.md\" = true\n",
    )
    .unwrap();
    let dir = root.session_dir("shutdown-frontend");
    let before = scratch_dirs();

    let error = failure(
        prepare(request(root.path(), "shutdown-frontend", None)).await,
        "unreadable [context] must fail preparation",
    );
    assert!(
        error.to_string().contains("[context]"),
        "fails on the injected step: {error}"
    );
    assert!(
        !dir.exists(),
        "the minted session dir is rolled back: {}",
        dir.display()
    );
    assert_eq!(
        scratch_dirs(),
        before,
        "the minted scratch dir is rolled back"
    );

    // The rolled-back name is free, and the parent the mint created recursively
    // is left in place for the retry.
    mint_session_dir(&dir).expect("the name is free for a corrected retry");
}

/// `worktree: true` outside a git work tree fails creation and leaves nothing.
#[tokio::test]
async fn a_worktree_request_outside_git_is_refused_with_nothing_minted() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("worktree-refused");
    let dir = root.session_dir("plain-fox");
    let mut req = request(root.path(), "plain-fox", None);
    req.worktree = true;

    let error = failure(prepare(req).await, "no git work tree must fail creation");
    assert!(error.to_string().contains("git work tree"), "{error}");
    assert!(!dir.exists());
}

/// A failure after the worktree exists removes the worktree and its branch.
#[tokio::test]
async fn a_late_failure_removes_the_worktree_it_made() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("worktree-late");
    git(root.path(), &["init", "-q"]);
    std::fs::write(
        root.path().join("efference.toml"),
        "[context]\n\"absent.md\" = true\n",
    )
    .unwrap();
    git(root.path(), &["add", "efference.toml"]);
    commit(root.path(), "one");
    let mut req = request(root.path(), "late-fox", None);
    req.worktree = true;

    failure(prepare(req).await, "unreadable [context] must fail");
    assert!(!crate::worktree::path(root.path(), "late-fox").exists());
    assert!(!root.session_dir("late-fox").exists());
    let branches = git(root.path(), &["branch", "--list", "efference/late-fox"]);
    assert!(branches.is_empty(), "the branch is deleted");
}

/// A name whose dir exists is refused, and refusing it touches nothing: the
/// rollback only removes a dir this attempt's own exclusive create returned.
#[tokio::test]
async fn a_collision_leaves_the_existing_session_untouched() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("collide");
    let dir = root.session_dir("occupied");
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join(LOG_NAME);
    let config = dir.join(CONFIG_NAME);
    std::fs::write(&log, b"{\"seq\":0}\n").unwrap();
    std::fs::write(&config, b"scope = \"the prior session\"\n").unwrap();
    let before = scratch_dirs();

    let error = failure(
        prepare(request(root.path(), "occupied", None)).await,
        "an occupied name must be refused",
    );
    assert!(
        error.to_string().contains("already exists"),
        "names the collision: {error}"
    );
    assert_eq!(std::fs::read(&log).unwrap(), b"{\"seq\":0}\n");
    assert_eq!(
        std::fs::read(&config).unwrap(),
        b"scope = \"the prior session\"\n"
    );
    assert_eq!(scratch_dirs(), before, "and mints no scratch of its own");
}

/// Two creations of one name race the same exclusive `create_dir`: one gets the
/// dir and prepares into it, the other is refused and removes nothing — the
/// winner's log and config snapshot are whole when both have returned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_creations_of_one_name_yield_one_session() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("race");
    let dir = root.session_dir("contested");

    let (first, second) = tokio::join!(
        prepare(request(root.path(), "contested", None)),
        prepare(request(root.path(), "contested", None)),
    );
    let (winner, loser) = match (first, second) {
        (Ok(boot), Err(error)) | (Err(error), Ok(boot)) => (boot, error),
        (Ok(_), Ok(_)) => panic!("both creations of one name succeeded"),
        (Err(a), Err(b)) => panic!("neither creation succeeded: {a} / {b}"),
    };
    assert!(
        loser.to_string().contains("already exists"),
        "the loser is refused at the mint: {loser}"
    );
    assert!(dir.join(LOG_NAME).is_file(), "the winner's log survives");
    let recording = crate::persist::load_recording(dir.join(LOG_NAME)).unwrap();
    let metadata = recording.header.metadata().expect("boot records metadata");
    assert_eq!(metadata.id, "contested");
    assert_eq!(metadata.cwd, root.path());
    assert_eq!(metadata.main.model, winner.model);
    assert_eq!(
        metadata.main,
        *crate::log::recorded_inference(&recording.entries).unwrap()
    );
    assert_eq!(
        recording.entries[0].seq, 0,
        "the header consumes no sequence"
    );
    assert!(
        dir.join(CONFIG_NAME).is_file(),
        "the winner's config snapshot survives"
    );
    assert!(
        winner
            .toolbox
            .scratch_dir()
            .is_some_and(|scratch| scratch.is_dir()),
        "and its scratch dir survives the loser's rollback"
    );
    drop_boot(winner);
}

/// The self-surface stack (docs/permissions.md, docs/inbox.md): under an operator's `.` grant the
/// session's own log stays unreadable and unwritable, while its attachment store is readable —
/// the notice tells the model to pull from there — and still not writable.
#[test]
fn the_attachment_store_is_readable_through_the_self_surface_deny() {
    let mut role = crate::config::Config::from_toml(
        "tools = [\"read\", \"read_image\", \"write\"]\n[paths]\n\".\" = { read = true, write = true }\n",
    )
    .unwrap();
    let session_dir = Path::new(BASE_DIR).join("s1");
    self_surface_grants(&mut role.paths, &session_dir);
    let cwd = PathBuf::from("/work/proj");
    let call = |tool: &str, path: PathBuf| {
        let input = serde_json::json!({ "path": path, "paths": [path], "content": "x" });
        crate::policy::classify(
            &crate::policy::Directories::at(&cwd),
            &role,
            &crate::log::ToolCall::new(tool, "c", input),
        )
    };
    let attached = session_dir.join("attachments/composer/shot.png");
    use crate::policy::Verdict::{Clear, Gate};
    assert_eq!(call("read_image", attached.clone()), Clear);
    assert_eq!(call("read", attached.clone()), Clear);
    assert_eq!(call("write", attached), Gate);
    assert_eq!(call("read", session_dir.join(LOG_NAME)), Gate);
    assert_eq!(call("read", session_dir.join(CONFIG_NAME)), Clear);
}
