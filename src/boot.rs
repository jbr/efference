//! Preparing a session from its creator's inputs (docs/control-channel.md).
//!
//! `prepare` turns a [`SessionRequest`] — what whoever creates a session has
//! resolved: its root, its base environment, its storage dir, its flags — into a
//! [`SessionBoot`] the runtime starts from. Everything in between is session
//! preparation: the origin config, `[env]` references, the provider and its
//! capability lookup, the session dir and config snapshot, scratch, the `[env]`
//! policy, the log, the toolbox and its MCP mounts, the frozen preamble, the
//! provider client, the spotter, and the triage. Listeners and the control
//! socket are not prepared here; they belong to whoever runs the runtime.
//!
//! A host's session-creation handler builds the request from a control-channel
//! body after resolving its role selections against the host's connections.
//! Boot progress is reported on stderr, the host's surface; a hosted creation
//! will need that report returned instead (backlog/inbox/host-process.md).

use std::path::PathBuf;

use crate::api::Error;
use crate::daemon::SessionBoot;
use crate::persist::Log;
use crate::spotter::MountedSpotter;

mod env;
pub mod mcp_mount;
mod metadata;
mod mint;
mod model;
mod parts;
mod resume;
mod scaffold;

use env::resolve_env_values;
use mint::Minted;
pub use mint::{mint_scratch_dir, mint_session_dir};
pub use resume::{ResumeRequest, recorded_selections, resume};
pub use scaffold::CONFIG_SCAFFOLD;

/// The base directory for all session dirs (gitignored). Each session is
/// `.efference/<id>/`, holding its standard-named log and config
/// (docs/permissions.md).
pub const BASE_DIR: &str = crate::daemon::SESSION_BASE;

pub const LOG_NAME: &str = "log.jsonl";

pub const CONFIG_NAME: &str = "config.toml";

/// What a session's creator supplies. Paths are absolute; nothing here is read
/// from the process environment or working directory by `prepare`.
pub struct SessionRequest {
    /// The session's project (config, grants, the session base) and working directory
    /// (tools, MCP mounts, children) (docs/control-channel.md).
    pub directories: crate::policy::Directories,
    /// The session's base environment (docs/env.md).
    pub base_env: crate::env_table::EnvSnapshot,
    /// The session dir to mint: `<project>/.efference/<id>`.
    pub session_dir: PathBuf,
    /// `--config`: the origin config to snapshot, or `None` for the default probe.
    pub config: Option<PathBuf>,
    /// The resolved role selections: each role's endpoint and native model, and
    /// the credential needles the session masker absorbs (docs/transport.md).
    pub inference: crate::inference::SessionInference,
    pub temperature: Option<u8>,
    pub effort: Option<String>,
    pub upstream: Vec<String>,
    pub autonomous: bool,
    pub reauth: Vec<String>,
    /// What an MCP server asking for authorization meets (docs/mcp-auth.md).
    pub authorization: mcp_mount::AuthorizationHandling,
    pub keep_scratch: bool,
    /// Whether the session outlives its creator (docs/tools.md); recorded in the
    /// header so a resume reads it back.
    pub detached: bool,
    /// Create the session's own git worktree and start it there (docs/worktrees.md).
    pub worktree: bool,
    /// The host this session is created on, when it is: the in-session
    /// `sessions` mount reaches it directly (docs/tools.md). `None` for a
    /// standalone session, which has no host to address.
    pub host: Option<std::sync::Arc<crate::daemon::Host>>,
    /// Where this session came from — its creator and its predecessor, when a
    /// session made it (docs/tools.md). Both reach the prompt's identity line
    /// (docs/context.md); the host records the same values when it adopts the
    /// started runtime. Default for a human root.
    pub lineage: crate::env::Lineage,
}

/// Prepare a session: everything from the origin config to the provider client,
/// in the order the boot report logs it (per-session setup detail, at `debug`).
/// Fails before minting anything when the
/// config, a reference, the provider, or the effort level is unusable; a failure
/// after that point rolls back what this attempt minted, so the name is free for a
/// corrected retry (docs/control-channel.md).
///
/// Spawned resources need no entry in [`Minted`]: an stdio MCP server's child is
/// killed when the toolbox holding it drops, which the error return does.
pub async fn prepare(request: SessionRequest) -> Result<SessionBoot<MountedSpotter>, Error> {
    let mut minted = Minted::default();
    minted.keep_scratch = request.keep_scratch;
    // On an error, and when this future is dropped mid-await, `minted` rolls back as
    // it drops.
    let boot = prepare_minting(request, &mut minted).await?;
    minted.keep();
    Ok(boot)
}

/// [`prepare`], recording what it mints as it goes.
///
/// The steps a resume shares with a fresh create are the functions called from
/// here (`model::resolve_capabilities`, `scaffold::load_session_config`,
/// `scaffold::mint_scratch`, `env::build_env_policy`, and the `parts` builders);
/// what is written inline is specific to creating: the origin config, the
/// minted dir and its config snapshot, the log's creation, and the frozen
/// preamble.
async fn prepare_minting(
    request: SessionRequest,
    minted: &mut Minted,
) -> Result<SessionBoot<MountedSpotter>, Error> {
    let mut metadata = metadata::metadata(&request);
    let SessionRequest {
        mut directories,
        base_env,
        session_dir,
        config,
        inference,
        temperature,
        effort,
        upstream,
        autonomous,
        reauth,
        authorization,
        keep_scratch,
        // Already in `metadata`, which carries it to the header.
        detached: _,
        worktree,
        host,
        lineage,
    } = request;
    assert!(
        directories.project().is_absolute() && directories.working().is_absolute(),
        "session directories must be absolute"
    );
    assert!(session_dir.is_absolute(), "session dir must be absolute");
    let (main_selection, mut endpoint) = inference.main();
    // The main role is fixed for the session's life. Keep its structured identity
    // for the boot record; offline readers must not recover it from prose or usage
    // telemetry (docs/log-and-projection.md).
    let recorded_inference = crate::log::LoggedInference::new(
        main_selection.connection.clone(),
        main_selection.provider,
        main_selection.model.clone(),
    );
    let model = main_selection.model.as_str();

    // The origin config is read here — before the session dir is minted — because
    // its `[env]` table may supply the harness's own credential as a reference
    // (docs/env.md), and because a malformed template or an unresolvable
    // reference should fail before anything is minted or bound.
    let origin = match crate::config::load_in(directories.project(), config.as_deref()) {
        Ok(l) => l,
        Err(e) => {
            e.eprint();
            return Err("could not load role config".into());
        }
    };
    // `[env]` reference resolution (docs/env.md), eagerly and once: a vault that
    // prompts (1Password's Touch ID) does it here, at session open, never inside
    // a turn. The resolver's cache is shared with the session policy below and
    // with every later config edit, so an unchanged reference never re-prompts.
    let resolver = std::sync::Arc::new(crate::env_table::Resolver::with_environment(
        directories.project().to_path_buf(),
        base_env.clone(),
    ));
    // Every value is resolved here so a failing reference fails creation, before
    // anything is minted; the policy below re-reads them from the shared cache.
    resolve_env_values(&resolver, &origin.role.env)?;

    // The sticky-routing key (docs/transport.md): derived once here, from the session
    // dir, which is fixed for the session's life. It pins the session to one upstream
    // so consolidation — which rewrites the opening messages, and with them the key
    // OpenRouter would otherwise derive by hashing them — can't re-route mid-session.
    // Whether it rides a request is the provider's call; a session that never talks to
    // a provider with sticky routing simply derives a key nothing reads.
    endpoint.session_id = Some(crate::api::sticky_session_id(&session_dir));

    // The session's effort level (docs/transport.md): the flag if given, else the
    // config's `[transport] effort`, else nothing on the wire at all. Boot-time and
    // frozen — no runtime toggle, because a mid-session change would move the cached
    // prefix. Read from the origin, which the session's snapshot below copies
    // verbatim, so the level is settled before anything is minted.
    let effort = effort.or_else(|| origin.role.transport.effort.clone());
    let capabilities =
        model::resolve_capabilities(&endpoint, model, &upstream, temperature, effort.as_deref())
            .await?;

    // Everything for this session is colocated in its own dir (gitignored), standard-named since
    // the dir is the unique unit (docs/permissions.md). Minting it is also the guard against
    // re-serving a name: an existing dir is refused here, before the log is opened and before
    // the socket is unlinked, so neither the sequential nor the concurrent corruption path is
    // reachable (docs/control-channel.md).
    mint_session_dir(&session_dir)?;
    // A successful exclusive create is the proof of ownership rollback needs: this
    // path is this attempt's alone, so a later failure may remove it (docs/control-channel.md).
    minted.session_dir = Some(session_dir.clone());

    let session_config = session_dir.join(CONFIG_NAME);
    scaffold::snapshot_config(&origin, &session_config)?;
    let mut loaded = scaffold::load_session_config(&session_config, &session_dir)?;

    let scratch = scaffold::mint_scratch(minted, &mut loaded)?;

    let env_policy = env::build_env_policy(&resolver, &loaded.role.env, &inference, base_env)?;

    // The session's id is its dir's name (docs/control-api.md): the address the
    // control channel, the `sessions` mount's caller field, and the identity line
    // in the system prompt all use, so it is read once here.
    let session_id = session_dir
        .file_name()
        .map_or_default(|name| name.to_string_lossy().into_owned());

    // The session's own worktree, made before the log exists so a refusal (not a git
    // work tree) fails creation with only the session dir to roll back. Git runs the
    // project's `post-checkout` hook inside this call, which can take seconds.
    let mut worktree_added = None;
    if worktree {
        let project = directories.project().to_path_buf();
        let (from, id, child_env) = (
            directories.working().to_path_buf(),
            session_id.clone(),
            env_policy.child_env(),
        );
        let add_project = project.clone();
        let added = tokio::task::spawn_blocking(move || {
            crate::worktree::add(&add_project, &id, &from, &child_env)
        })
        .await
        .map_err(|e| format!("worktree creation task failed: {e}"))??;
        minted.worktree = Some(mint::MintedWorktree {
            project: project.clone(),
            path: added.path.clone(),
            branch: added.branch.clone(),
            env: env_policy.child_env(),
        });
        directories = crate::policy::Directories::new(project, added.path.clone());
        metadata.working = Some(added.path.clone());
        worktree_added = Some(added);
    }

    let log_path = session_dir.join(LOG_NAME);
    metadata.effort = effort.clone();
    let mut log = Log::create(&log_path, Some(metadata))?;
    // In force before the first append — the boot `Preamble` below is the first
    // entry, and its `[context]` exec snapshot is exactly the kind of env capture
    // masking exists for.
    inference.absorb_secrets(&env_policy.masker());
    log.set_masker(env_policy.masker());
    log::debug!("session dir: {}", session_dir.display());
    log::debug!("logging to {}", log_path.display());

    // Build the system prompt once, here at startup, and freeze it into the cached
    // prefix — it must stay byte-stable for the whole session or it would invalidate
    // the cache every turn (docs/log-and-projection.md). Everything it folds in is resolved
    // once at boot: the [context] block (files read + exec sections run now, frozen —
    // docs/context.md; orientation rides here as a config exec entry), the role's
    // brief, and the config note.
    let tools = parts::mount_toolbox(parts::ToolboxInputs {
        cwd: directories.working(),
        scratch: &scratch,
        loaded: &loaded,
        env_policy: &env_policy,
        capabilities: &capabilities,
        endpoint: &endpoint,
        host,
        secrets: inference.secrets(),
        session_id: &session_id,
        reauth: &reauth,
        authorization: &authorization,
    })
    .await?;
    let parts::MountedTools {
        toolbox,
        unmounted_section,
    } = tools;

    let preamble = parts::freeze_preamble(
        &loaded,
        crate::env::Whereabouts {
            project: directories.project(),
            working: directories.working(),
            created_branch: worktree_added.as_ref().map(|added| added.branch.as_str()),
        },
        &env_policy,
        model,
        &session_id,
        &lineage,
        unmounted_section.as_deref(),
    )?;
    let parts::FrozenPreamble {
        identity,
        body,
        resolved,
        context_block,
    } = preamble;
    // Snapshot the rendered boot preamble into the log (docs/context.md): the
    // [context] block folds in file contents that drift on disk, so the preamble is no longer
    // reconstructable from config + current files — this one entry keeps the log the complete
    // record of the wire. `efference dev wire` reads it back instead of rebuilding.
    log.append(
        0,
        crate::log::Payload::preamble(
            identity.clone(),
            body.clone(),
            resolved,
            Some(recorded_inference),
        ),
    )?;

    // The tools[] this session advertises, kept so a resume can say what changed
    // (docs/resumption.md) and offline readers need not rebuild it.
    log.append(
        0,
        crate::log::Payload::Telemetry(crate::log::Telemetry::Tools {
            tools: crate::agent::tool_definitions(&toolbox),
        }),
    )?;

    if let Some(added) = worktree_added {
        log.append(0, crate::log::Payload::Worktree(added.into_event(false)))?;
    }

    let client = parts::build_client(
        &endpoint,
        model,
        format!("{identity}{body}"),
        parts::ClientSettings {
            temperature,
            upstream,
            effort,
        },
        &loaded,
        &capabilities,
    );

    let spotter = parts::mount_spotter(&inference, &endpoint, &scratch, &directories).await?;

    if autonomous {
        log::debug!(
            "autonomous: no synchronous gate — spotter Watch/Gate redirects the model to its \
             config file; config writes still gate for the orchestrator-above"
        );
    }
    let (triager, triage_prices) = parts::mount_triager(
        &inference,
        &endpoint,
        model,
        context_block.as_deref(),
        &env_policy,
        &loaded,
    )
    .await?;
    Ok(SessionBoot {
        client,
        toolbox,
        triager,
        spotter,
        log,
        paths: crate::daemon::SessionPaths::new(directories, session_dir.canonicalize()?)?,
        role: loaded,
        autonomous,
        capabilities,
        model: model.to_string(),
        inference: Some(inference.identity()),
        env_policy,
        keep_scratch,
        triage_prices,
        cache_lost_at: None,
    })
}

/// The context window for a boot line: the token count, or a statement that the
/// capability source reported none (docs/transport.md). Said rather than omitted,
/// because an unreported window turns off the occupancy meter and the pre-emptive
/// output fit, and the boot line is where that becomes visible instead of
/// surprising.
pub fn describe_window(window: Option<u64>) -> String {
    match window {
        Some(tokens) => format!("{tokens} tokens"),
        None => "window not reported".to_string(),
    }
}

#[cfg(test)]
pub(crate) mod test;
