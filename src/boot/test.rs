use super::*;

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
struct Root(PathBuf);

impl Root {
    fn new(label: &str) -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-boot-{label}-{nonce:016x}"));
        std::fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn session_dir(&self, id: &str) -> PathBuf {
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
static PREPARING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The scratch dirs [`mint_scratch_dir`] could have made: `efference-` plus
/// exactly sixteen hex digits, which no other temp name in this suite matches.
fn scratch_dirs() -> std::collections::BTreeSet<PathBuf> {
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

/// A session request against the compiled OpenAI catalog: `fetch_capabilities`
/// reads it in-process, so preparation runs to the end without a provider.
fn request(root: &Path, id: &str, effort: Option<&str>) -> SessionRequest {
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[defaults]
model = "oai/gpt-5.6"

[connections.oai]
provider = "openai"
auth = { kind = "env", key = "EFFERENCE_BOOT_TEST_KEY" }
"#,
    )
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
    .resolve(None, None, None)
    .unwrap();
    let base_env = crate::env_table::EnvSnapshot::new(
        ["HOME", "PATH"]
            .into_iter()
            .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
            .collect(),
    )
    .unwrap();
    SessionRequest {
        cwd: root.to_path_buf(),
        base_env,
        session_dir: root.join(BASE_DIR).join(id),
        config: None,
        inference,
        temperature: None,
        effort: effort.map(str::to_string),
        upstream: Vec::new(),
        autonomous: false,
        reauth: Vec::new(),
        keep_scratch: false,
        host: None,
        created_by: None,
    }
}

/// The scratch dir a successful boot owns, removed here as the runtime's
/// teardown would (docs/tools.md).
fn drop_boot(boot: SessionBoot<ForkSpot>) {
    if let Some(scratch) = boot.toolbox.scratch_dir() {
        let _ = std::fs::remove_dir_all(scratch);
    }
}

/// The error a preparation that must fail returned, cleaning up after it if it
/// unexpectedly succeeded. `Result::expect_err` wants a `Debug` boot.
fn failure(result: Result<SessionBoot<ForkSpot>, Error>, what: &str) -> Error {
    match result {
        Ok(boot) => {
            drop_boot(boot);
            panic!("{what}")
        }
        Err(error) => error,
    }
}

/// An effort level the model's metadata does not declare is rejected before the
/// session dir is minted, so the name stays free and the corrected retry takes it
/// (docs/control-channel.md). The rejection and the retry are the sequence a
/// creating client actually runs.
#[tokio::test]
async fn a_rejected_effort_reserves_no_name() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("effort");
    let dir = root.session_dir("file-nomination");

    let error = failure(
        prepare(request(root.path(), "file-nomination", Some("bogus"))).await,
        "an undeclared effort level must fail preparation",
    );
    assert!(
        error.to_string().contains("bogus"),
        "names the level: {error}"
    );
    assert!(
        !dir.exists(),
        "a rejected effort mints nothing: {}",
        dir.display()
    );

    let boot = prepare(request(root.path(), "file-nomination", Some("high")))
        .await
        .expect("the corrected retry mints the same name");
    assert!(dir.join(LOG_NAME).is_file(), "the retry opened its log");
    assert!(dir.join(CONFIG_NAME).is_file(), "and its config snapshot");
    drop_boot(boot);
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
