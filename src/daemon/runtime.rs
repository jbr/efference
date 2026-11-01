//! One session's tasks and their shutdown (docs/control-channel.md).
//!
//! A [`SessionRuntime`] owns everything that runs on a session's behalf: the
//! agent loop, the topic differ, the cache keepalive, the config and attachment
//! watchers, background-process reaping, and scratch cleanup. Listeners are not
//! its concern: a runtime is started under a parent [`Swansong`] the listener
//! owner holds, and its own child `Swansong` is the one handle that stops it.

use super::*;
use crate::api::{Client, Error, ModelCapabilities};
use crate::triage::Triager;
use tokio::sync::oneshot;
use trillium_tokio::Swansong;

/// Everything a session starts from, resolved by whoever creates it. Pure data:
/// a host's session-creation handler builds it from `boot::prepare`, and tests
/// build it by hand. Nothing here reads ambient process state.
pub struct SessionBoot<S: Spotter> {
    pub client: Client,
    pub toolbox: Toolbox,
    pub triager: Triager,
    pub spotter: S,
    pub log: Log,
    pub paths: SessionPaths,
    pub role: LoadedConfig,
    pub autonomous: bool,
    pub capabilities: ModelCapabilities,
    pub model: String,
    /// The role selections behind `model`; `None` only for a boot built by hand (tests).
    pub inference: Option<crate::inference::InferenceIdentity>,
    pub env_policy: crate::env_table::EnvPolicy,
    /// Leave the scratch dir on disk when the session ends (docs/tools.md).
    pub keep_scratch: bool,
    /// The triage model's prices, for its keepalive's affordability check
    /// (docs/transport.md). `None` on a shared triage, which has no keepalive of its own.
    pub triage_prices: Option<crate::api::TokenPrices>,
    /// When the session's main prefix was abandoned: set by a resume on another main
    /// model, so the session starts cold (docs/resumption.md).
    pub cache_lost_at: Option<crate::record::log::Timestamp>,
}

/// A started session: its state, and the handle that ends it.
pub struct SessionRuntime {
    session: Arc<Session>,
    swansong: Swansong,
    /// Resolves once the agent loop has returned and its teardown has run.
    outcome: oneshot::Receiver<Result<(), Error>>,
}

impl SessionRuntime {
    /// Start a session under `parent`: build its [`Session`] and spawn its
    /// watchers and its agent loop on the current tokio runtime. Shutting
    /// `parent` down stops this runtime; shutting this runtime down does not
    /// touch `parent`.
    ///
    /// The loop is `Send` end to end — the toolbox holds its embedded servers'
    /// state behind a `Mutex`, and the `Frontend`/`Spotter` futures are `Send` —
    /// so it runs on the host's work-stealing runtime alongside everything else.
    /// A session that ran on a runtime of its own stopped being polled whenever
    /// its loop blocked synchronously, and a pooled connection it had opened was
    /// then undriveable from anywhere (docs/control-channel.md).
    pub fn start<S: Spotter + Send + Sync + 'static>(
        boot: SessionBoot<S>,
        parent: &Swansong,
    ) -> Self {
        let SessionBoot {
            mut client,
            toolbox,
            mut triager,
            mut spotter,
            log,
            paths,
            role,
            autonomous,
            capabilities,
            model,
            inference,
            env_policy,
            keep_scratch,
            triage_prices,
            cache_lost_at,
        } = boot;
        let swansong = parent.child();
        // The keepalive interval, read off the config before it moves into `Session`
        // (docs/transport.md).
        // The configured interval applies to both lineages (docs/transport.md); read before
        // `role` moves into `Session`.
        let configured_interval = role.role.transport.keepalive_interval;
        let ping_at = configured_interval
            .unwrap_or_else(|| keepalive_interval(client.endpoint().provider.cache_ttl()));
        // The kickoff (docs/context.md), read before the config moves into `Session`.
        let first_turn = role.role.first_turn.clone();
        let scratch = toolbox.scratch_dir().map(std::path::Path::to_path_buf);
        let (session, turns) = Session::full(SessionParts {
            role,
            id: Some(paths.id().to_string()),
            scratch: scratch.clone(),
            upstream_failure: client.upstream_failure(),
            upstream_retry: client.upstream_retry(),
            plan: client.plan_state(),
            image_placement: client.image_placement(),
            env_policy,
            ..SessionParts::new(paths.directories(), log)
        });
        submit_kickoff(&session, first_turn);
        // The project's storage base, for the in-session `sessions` mount's
        // listing and target scope (docs/tools.md).
        session.set_storage_base(paths.storage());

        // The boot review level (docs/permissions.md), set before the loop spawns
        // because the gate reads it on the loop's thread.
        session.set_autonomous(autonomous);
        // Whether this model has a prompt cache to keep warm at all
        // (docs/transport.md) — read before the capabilities move into `Session`.
        let cacheable = capabilities.prompt_cache.is_supported();
        let prices = capabilities.prices;
        // The boot-fetched model capabilities (docs/transport.md) — the caller already
        // treated a failed lookup as fatal, so by here the value always exists.
        session.set_capabilities(capabilities);
        describe_session(&session, &paths, &client, model, inference, cache_lost_at);
        wire_requests(&session, &mut client, &mut triager, &mut spotter);
        // Share the client between the loop and the keepalive task. `tools` is computed
        // once here for the ping; `agent::run` recomputes the same (byte-stable,
        // docs/log-and-projection.md) definitions internally, so both send an
        // identical prefix.
        let client = Arc::new(client);
        // The exact tools[] a real turn sends — world tools + the harness tools
        // (`consolidate`, the process-monitor/inbox tools, docs/process-monitor.md, docs/inbox.md).
        // Built from the one source (`agent::tool_definitions`) so the keepalive ping
        // warms the *same* cache prefix a real turn reads; any drift here silently
        // breaks idle cache-warming (docs/log-and-projection.md).
        let tools = agent::tool_definitions(&toolbox);
        // Snapshot the mounted tool catalog for `GET /v1/tools/:name`
        // (docs/control-api.md), before `toolbox` moves into the loop below.
        session.set_tool_catalog(agent::tool_catalog(&toolbox));

        // The one state-topic differ (docs/control-api.md): recomputes the topics on
        // any change signal and publishes the ones that moved, so N connected clients
        // cost one recomputation instead of N polls.
        tokio::spawn(swansong.interrupt(watch_topics(Arc::clone(&session))));
        spawn_cache_tasks(
            &swansong,
            &session,
            &client,
            tools,
            &triager,
            CachePolicy {
                cacheable,
                ping_at,
                prices,
                configured_interval,
                triage_prices,
            },
        );
        spawn_file_watchers(&swansong, &session, &paths);

        // The agent loop, spawned on the current (host) runtime like the watchers
        // above. It shares that runtime's I/O driver with every other session and
        // with the host's own work, which is what lets the process-wide HTTP client
        // pool connections across them (docs/control-channel.md). The loop parks in
        // `read_user_turn` on the turn channel, whose senders live in the still-alive
        // `Arc<Session>`, so shutdown has to cancel it rather than wait for it: the
        // child `Swansong`'s `interrupt` drives the loop normally until shutdown is
        // initiated, then cancels it and resolves to `None`. The guard taken below
        // holds the runtime's shutdown open until teardown — background reaping and
        // scratch cleanup — has run, so `shut_down().await` on this runtime (or on its
        // parent) means the session's processes are gone.
        let (done, outcome) = oneshot::channel();
        let loop_session = Arc::clone(&session);
        let loop_swansong = swansong.clone();
        let guard = swansong.guard();
        tokio::spawn(async move {
            let _guard = guard;
            let mut frontend = RemoteFrontend {
                shared: Arc::clone(&loop_session),
                turns,
            };
            let run = agent::run(
                &client,
                &mut frontend,
                Arc::clone(&loop_session),
                triager,
                spotter,
                toolbox,
            );
            let result = match loop_swansong.interrupt(run).await {
                Some(result) => result,
                None => {
                    log::debug!("turn loop interrupted; shutting down");
                    Ok(())
                }
            };
            tear_down(&loop_session, scratch, keep_scratch).await;
            let _ = done.send(result);
        });

        Self {
            session,
            swansong,
            outcome,
        }
    }

    /// The session's state and control surface — what listeners register as
    /// shared state and handlers read.
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// The handle that ends this session and nothing else: `shut_down()` cancels
    /// the loop and the watchers, and completes once teardown has run.
    pub fn swansong(&self) -> &Swansong {
        &self.swansong
    }

    /// Wait for the agent loop to return — on its own, or because shutdown was
    /// initiated — and report how it ended. Teardown has run by then.
    pub async fn finished(self) -> Result<(), Error> {
        match self.outcome.await {
            Ok(result) => result,
            Err(_) => Err("the session loop thread ended without reporting".into()),
        }
    }
}

/// What decides the cache tasks a session runs (docs/transport.md).
#[derive(Clone, Copy)]
struct CachePolicy {
    /// Whether this model has a prompt cache to keep warm at all.
    cacheable: bool,
    /// The main lineage's ping interval.
    ping_at: Duration,
    prices: Option<crate::api::TokenPrices>,
    /// `[transport] keepalive-interval`, which applies to both lineages.
    configured_interval: Option<Duration>,
    /// The triage model's prices, for its own lineage's affordability check.
    triage_prices: Option<crate::api::TokenPrices>,
}

/// The kickoff: with a `first-turn` and nothing yet said, the harness submits
/// the opening turn so the agent speaks first. A resumed log already has its
/// opening, so the check is on the log rather than on how the session began.
fn submit_kickoff(session: &Session, first_turn: Option<String>) {
    if let Some(text) = first_turn {
        let fresh = !session
            .snapshot()
            .iter()
            .any(|entry| matches!(entry.payload, Payload::User(_)));
        if fresh {
            session.submit_turn(SubmittedTurn::new(text, mint_submission_id()).sourced(Some(
                crate::record::names::sources::KICKOFF_SOURCE.to_string(),
            )));
        }
    }
}

/// What the session reports about itself and where it runs: its identity, its cost tracking
/// where the provider has one, a cache lost at boot, and its usage record.
fn describe_session(
    session: &Session,
    paths: &SessionPaths,
    client: &Client,
    model: String,
    inference: Option<crate::inference::InferenceIdentity>,
    cache_lost_at: Option<crate::record::log::Timestamp>,
) {
    // What this session is, for a client that has to name it on screen
    // (docs/control-api.md): the resolved model and the working directory, both
    // fixed by the time the caller gets here.
    session.set_identity(SessionIdentity {
        id: paths.id().to_string(),
        title: crate::record::log::latest_title(&session.snapshot()).map(str::to_string),
        model,
        cwd: paths.working().to_string_lossy().into_owned(),
        inference,
        successor: None,
    });
    // Session cost telemetry (docs/transport.md): armed only where the provider
    // has a per-request accounting surface (OpenRouter's generation endpoint).
    // Direct Anthropic has none, so the readout stays absent rather than modeled.
    if client.endpoint().provider == crate::record::log::Provider::OpenRouter {
        session.enable_cost_tracking(client.endpoint().clone());
    }
    // The per-request usage record (docs/log-and-projection.md) lives beside the
    // canonical log in the explicitly supplied storage directory.
    if let Some(at) = cache_lost_at {
        session.mark_cache_lost(at);
    }
    session.attach_usage_log(&paths.usage_log());
}

/// Wire the session's clients to it. The operator's interrupt ends a generation in flight
/// (docs/control-channel.md): the agent client and the own-triage client both read against
/// it. The spotter does not — an interrupt during a gate is the tool's kill. Every request
/// any of the session's clients makes reports its stages to the log
/// (docs/log-and-projection.md): the agent client (turns, the shared peek, titles,
/// keepalive, compactions), the own-triage client, and the spotter.
fn wire_requests<S: Spotter>(
    session: &Arc<Session>,
    client: &mut Client,
    triager: &mut Triager,
    spotter: &mut S,
) {
    client.set_abort(session.interrupt_signal());
    triager.set_abort(session.interrupt_signal());
    if let Some(cells) = triager.client_state() {
        session.set_triage_client(cells);
    }
    client.set_stage_sink(session.stage_sink());
    triager.set_stage_sink(session.stage_sink());
    spotter.set_stage_sink(session.stage_sink());
}

/// The tasks that watch the session's files: the config file and the attachment store.
fn spawn_file_watchers(swansong: &Swansong, session: &Arc<Session>, paths: &SessionPaths) {
    // The fs-watch initiator (docs/permissions.md): if the config resolved
    // from a file, watch it so external edits take effect live as committed
    // `ConfigEdit`s. The built-in default has no file to watch.
    if let Some(config_path) = session.config_path().map(std::path::Path::to_path_buf) {
        tokio::spawn(swansong.interrupt(config_watch_loop(Arc::clone(session), config_path)));
    }

    // The attachment store (docs/inbox.md): the same fs-watch affordance, over
    // `.efference/<id>/attachments/`. A file appearing there — pasted, dragged,
    // moved in — becomes a notification naming what arrived; the bytes stay on
    // disk until the model pulls them, so nothing here enters the log.
    session.set_attachment_store(crate::attachments::Store::new(
        paths.attachments(),
        paths.attachment_display(),
    ));
    tokio::spawn(swansong.interrupt(attachments_watch_loop(
        Arc::clone(session),
        paths.attachments(),
        paths.attachment_display(),
    )));
}

/// The tasks that keep the session's prompt caches: the cold-state watcher, the
/// compaction loop, and a keepalive per armed lineage.
fn spawn_cache_tasks(
    swansong: &Swansong,
    session: &Arc<Session>,
    client: &Arc<Client>,
    tools: Vec<Value>,
    triager: &Triager,
    policy: CachePolicy,
) {
    let CachePolicy {
        cacheable,
        ping_at,
        prices,
        configured_interval,
        triage_prices,
    } = policy;
    // The cache keepalive runs alongside the loop (docs/log-and-projection.md) — a
    // transport-side timer that never touches the log, so it composes with the
    // loop without sharing anything but the read-only client and the telemetry
    // clocks.
    //
    // Two gates, either of which keeps it from spawning. **No prompt cache**: the
    // boot capability lookup found no upstream behind this model that caches
    // prompts, so a ping would refresh nothing and re-bill the
    // whole prefix at the input rate every interval — the ping is only free where
    // there is an entry to read. **Interval zero**: the operator turned it off.
    // The affordability check inside the loop stays as well; it catches the endpoint
    // that claims a cache and reasons on the ping anyway (docs/transport.md).
    // The cold state (backlog: cold-session.md) exists only where there is a cache
    // to lose; its watcher publishes the moment the belief turns.
    if cacheable {
        session.set_cache_ttl(client.endpoint().provider.cache_ttl());
        tokio::spawn(swansong.interrupt(cold::cold_watch(Arc::clone(session))));
    }
    // The compaction writes on the session's own client (docs/summary.md). It starts
    // whatever the capabilities say about a prompt cache: a warm `POST /loop/compact`
    // is accepted without a cold belief, and unknown cache support is not evidence
    // that none exists.
    tokio::spawn(swansong.interrupt(compact::compact_loop(
        Arc::clone(client),
        Arc::clone(session),
        tools.clone(),
    )));
    if cacheable && !ping_at.is_zero() {
        tokio::spawn(swansong.interrupt(keepalive_loop(
            KeepaliveSpec {
                lineage: CacheLineage::Main,
                client: Arc::clone(client),
                tools,
                ping_at,
                prices,
            },
            Arc::clone(session),
        )));
    } else if !cacheable {
        log::debug!(
            "keepalive off: {} reports no prompt cache behind this model",
            client.endpoint().provider.name()
        );
    }
    // The triage lineage's keepalive (docs/transport.md): armed only where the triage
    // model is the session's own and reports a prompt cache. Interval zero turns it
    // off, as it does the main loop.
    if let Some(triage) = KeepaliveSpec::triage(triager, configured_interval, triage_prices)
        && !triage.ping_at.is_zero()
    {
        tokio::spawn(swansong.interrupt(keepalive_loop(triage, Arc::clone(session))));
    }
}

/// The session's teardown once its loop has returned. Reaps any background children
/// before the session is gone so a stopped session doesn't orphan a process group
/// (docs/process-monitor.md). Awaited, not merely signalled: the scratch removal below
/// would otherwise race a child still writing under it, and `shut_down()` would resolve
/// while the processes were still in the table.
async fn tear_down(session: &Session, scratch: Option<PathBuf>, keep_scratch: bool) {
    session.reap_all_bg().await;
    // Scratch lifecycle (docs/tools.md): cleaned on session end unless
    // retained for post-mortem inspection.
    if let Some(scratch) = scratch {
        if keep_scratch {
            log::debug!("scratch kept: {}", scratch.display());
        } else if let Err(e) = std::fs::remove_dir_all(&scratch)
            && e.kind() != io::ErrorKind::NotFound
        {
            log::warn!("scratch: could not remove {}: {e}", scratch.display());
        }
    }
}
