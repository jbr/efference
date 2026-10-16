//! Building a [`Session`] and its state cells (docs/control-channel.md).

use super::*;

/// What a [`Session`] is built from: the working directory and log it owns, the
/// role it starts under, and the shared cells its client already holds.
///
/// Named fields rather than a parameter list, because four of these are opaque
/// shared-state handles and a positional mistake among them would compile.
/// [`Self::new`] fills every field a caller without a provider client has no
/// value for, so adding a dependency to a session touches the sites that supply
/// it rather than every site that constructs one.
pub(crate) struct SessionParts {
    /// The project and working directories (docs/control-channel.md).
    pub directories: crate::policy::Directories,
    pub log: Log,
    pub role: LoadedConfig,
    pub scratch: Option<PathBuf>,
    pub upstream_failure: UpstreamFailureState,
    pub upstream_retry: UpstreamRetryState,
    pub plan: crate::api::PlanState,
    /// Chosen from the provider at boot and fixed thereafter, so the canonical
    /// wire and the fork's cached prefix project byte-identically
    /// (docs/log-and-projection.md).
    pub image_placement: ImagePlacement,
    pub env_policy: crate::env_table::EnvPolicy,
    /// The session's id — the storage directory's name, known at boot
    /// (docs/control-channel.md). None in tests and offline sessions, whose
    /// log lines print unattributed.
    pub id: Option<String>,
}

impl SessionParts {
    /// The session's own two pieces, with every shared cell freshly allocated —
    /// the shape a caller with no provider client behind it wants. A caller that
    /// has one overrides the cells it holds.
    pub(crate) fn new(directories: crate::policy::Directories, log: Log) -> Self {
        Self {
            directories,
            id: None,
            log,
            role: LoadedConfig::default(),
            scratch: None,
            upstream_failure: upstream_failure_state(),
            upstream_retry: upstream_retry_state(),
            plan: crate::api::plan_state(),
            image_placement: ImagePlacement::default(),
            env_policy: crate::env_table::EnvPolicy::default(),
        }
    }
}

impl Session {
    /// Build state under the default role at an absolute working root
    /// (docs/control-channel.md). Relative roots are a caller error and panic.
    pub fn new(cwd: PathBuf, log: Log) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role(cwd, log, LoadedConfig::default())
    }

    /// Build the shared state under a resolved role and hand back the turn-inbox
    /// receiver for the loop.
    pub fn with_role(
        cwd: PathBuf,
        log: Log,
        role: LoadedConfig,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role_and_scratch(cwd, log, role, None)
    }

    /// The full constructor: [`with_role`](Self::with_role) plus the session
    /// scratch dir (docs/tools.md), whose value seeds the structured-tool path
    /// expander and the background spawn env.
    pub fn with_role_and_scratch(
        cwd: PathBuf,
        log: Log,
        role: LoadedConfig,
        scratch: Option<PathBuf>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role_scratch_and_upstream_failure(
            cwd,
            log,
            role,
            scratch,
            upstream_failure_state(),
        )
    }

    /// The daemon constructor with the upstream client's shared transport
    /// diagnostic (docs/transport.md). Tests normally use the public convenience
    /// constructors above, which allocate an empty cell.
    pub(crate) fn with_role_scratch_and_upstream_failure(
        cwd: PathBuf,
        log: Log,
        role: LoadedConfig,
        scratch: Option<PathBuf>,
        upstream_failure: UpstreamFailureState,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        let directories = crate::policy::Directories::at(&cwd);
        // Tests and the convenience constructors resolve the `[env]` policy from
        // the role they were handed (docs/env.md); boot builds one earlier —
        // it needs the strip set for the toolbox and the masker for the boot
        // preamble — and passes it to [`Session::full`] so the session has exactly
        // one, whose needle set is append-only for its lifetime.
        let base = crate::env_table::EnvSnapshot::capture().expect("valid local environment");
        let resolver = Arc::new(crate::env_table::Resolver::with_environment(
            directories.project().to_path_buf(),
            base.clone(),
        ));
        let env_policy =
            crate::env_table::EnvPolicy::build_snapshot(&role.role.env, base, &[], resolver).policy;
        Self::full(SessionParts {
            role,
            scratch,
            upstream_failure,
            env_policy,
            ..SessionParts::new(directories, log)
        })
    }

    /// The full constructor.
    pub(crate) fn full(parts: SessionParts) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        let SessionParts {
            directories,
            id,
            mut log,
            mut role,
            scratch,
            upstream_failure,
            upstream_retry,
            plan,
            image_placement,
            env_policy,
        } = parts;
        assert!(
            directories.working().is_absolute() && directories.project().is_absolute(),
            "session directories must be absolute"
        );
        if let ConfigSource::Path(path) = &mut role.source {
            *path = directories.project().join(&*path);
        }
        // Install the mask before the log takes another append (docs/env.md).
        // Masking is irreversible filtering upstream of the log, so it has to be
        // in force at the choke point rather than applied to a stored entry.
        log.set_masker(env_policy.masker());
        let (turns_tx, turns_rx) = mpsc::unbounded_channel();

        // Bounded ring: a consumer that falls this far behind drops the oldest
        // deltas (a `Lagged` on `recv`) rather than stalling the loop — the log is
        // the durable record, so a dropped delta is only a momentary gap in the
        // live view.
        let (stream_tx, _) = broadcast::channel(1024);
        // State updates are whole-topic replacements, so a lagging connection loses
        // nothing it cannot recover: the newest value for each topic is the only one that
        // matters, and a `Lagged` is resolved by re-reading the topics rather than by
        // replay. Sized well above the topic count so that is rare rather than routine.
        let (state_tx, _) = broadcast::channel(256);
        // Fold any grants already in the log (a resumed session) into the initial effective
        // role; from here it's maintained incrementally on append. Computed before `log` is
        // moved into the struct.
        let effective = Arc::new(crate::policy::effective_config(&role.role, log.entries()));
        // Fold the probe ledger from a resumed log (no-op for a fresh one), then maintain it
        // incrementally as the loop assesses moves (docs/permissions.md).
        let probes = crate::policy::probe_ledger(&directories, &effective, log.entries());
        // Seed the process-id mint past every `p#<n>` a resumed log mentions, so a restarted
        // daemon never re-mints an ordinal the durable record already refers to
        // (docs/process-monitor.md). Scanned over the serialized entry because
        // handles land in prose (tool results, notification sources), not in a typed field.
        let minted = log
            .entries()
            .iter()
            .map(|e| {
                serde_json::to_string(e)
                    .map_or(0, |s| crate::process_monitor::max_handle_mention(&s))
            })
            .max()
            .unwrap_or(0);
        // Open the file the role resolved from, if any. Boot has already loaded and validated it,
        // so this parses; a failure here is reported and leaves the session file-less (edits stay
        // in the log fold) rather than aborting a session over a race with an external writer.
        let config_file = match &role.source {
            ConfigSource::Path(path) => match crate::config::ConfigFile::load(path) {
                Ok(file) => Some(file),
                Err(e) => {
                    log::warn!(
                        "{}config: could not open {} for editing: {e}",
                        session_id_prefix(id.as_deref()),
                        path.display()
                    );
                    None
                }
            },
            ConfigSource::Default => None,
        };
        let shared = Arc::new_cyclic(|me| Self {
            pushes: push::PushState::new(me.clone()),
            directories,
            id,
            storage_base: std::sync::OnceLock::new(),
            attachment_store: std::sync::OnceLock::new(),
            web_path: std::sync::OnceLock::new(),
            cache_ttl: std::sync::OnceLock::new(),
            person_acted: AtomicBool::new(false),
            cold_parked: AtomicBool::new(false),
            cold_signal: Notify::new(),
            fold_state: Mutex::new(None),
            fold_signal: Notify::new(),
            operations: Mutex::new(operations::Operations::fold(log.entries())),
            log: Mutex::new(log),
            image_placement,
            turns_tx,
            approvals: Mutex::new(HashMap::new()),
            pending: Mutex::new(None),
            upstream: Mutex::new(None),
            interrupt: Arc::new(Notify::new()),
            interrupt_latch: AtomicBool::new(false),
            detach: Notify::new(),
            detach_note: Mutex::new(None),
            appended: Notify::new(),
            attention: std::sync::OnceLock::new(),
            escalation: Arc::default(),
            running: AtomicBool::new(false),
            queue: Mutex::new(std::collections::VecDeque::new()),
            ask: Mutex::new(None),
            settle_changed: Notify::new(),
            continue_pending: AtomicBool::new(false),
            continue_signal: Notify::new(),
            subscriptions: std::sync::atomic::AtomicUsize::new(0),
            notified: Notify::new(),
            stream_tx,
            state_changed: Notify::new(),
            state_tx,
            metrics: Mutex::new(Metrics::default()),
            usage_log: Mutex::new(None),
            upstream_failure,
            upstream_retry,
            plan,
            retention: Mutex::new((0, Retention::default())),
            capabilities: Mutex::new(None),
            cost: Arc::new(Mutex::new(CostCell::default())),
            identity: Mutex::new(None),
            stall: Mutex::new(None),
            config_repair: Mutex::new(None),
            config_repaired: Notify::new(),
            inbox: Mutex::new(Inbox::default()),

            proposals: Mutex::new(Vec::new()),
            process_monitor: Mutex::new(ProcessMonitor::with_minted(minted)),
            reaped: tokio::sync::Notify::new(),
            role,
            effective: Mutex::new(effective),
            config_file: Mutex::new(config_file),
            probes: Mutex::new(probes),
            env_policy,
            review_level: AtomicU8::new(crate::policy::ReviewLevel::SpotterHuman.as_u8()),
            pace: Mutex::new(Pace::default()),
            pace_interrupt: Notify::new(),
            expander: crate::expand::Expander::new(
                scratch
                    .iter()
                    .map(|p| ("TMPDIR".to_string(), p.to_string_lossy().into_owned()))
                    .collect(),
            ),
            scratch,
            tools: Mutex::new(HashMap::new()),
        });
        (shared, turns_rx)
    }
}
