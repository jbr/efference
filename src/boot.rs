//! Preparing a session from its creator's inputs (docs/control-channel.md).
//!
//! `prepare` turns a [`SessionRequest`] — what whoever creates a session has
//! resolved: its root, its base environment, its storage dir, its flags — into a
//! [`SessionBoot`] the runtime starts from. Everything in between is session
//! preparation: the origin config, `[env]` references, the provider and its
//! capability lookup, the session dir and config snapshot, scratch, the `[env]`
//! policy, the log, the toolbox and its MCP mounts, the frozen preamble, the
//! provider client, the spotter, and the fork peek. Listeners and the control
//! socket are not prepared here; they belong to whoever runs the runtime.
//!
//! A host's session-creation handler builds the request from a control-channel
//! body after resolving its role selections against the host's connections.
//! Boot progress is reported on stderr, the host's surface; a hosted creation
//! will need that report returned instead (backlog/host-process.md).

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
mod scaffold;

use env::resolve_env_values;
use mint::Minted;
pub use mint::{mint_scratch_dir, mint_session_dir};
pub use scaffold::CONFIG_SCAFFOLD;

/// The base directory for all session dirs (gitignored). Each session is
/// `.efference/<id>/`, holding its standard-named socket, log, and config
/// (docs/permissions.md).
pub const BASE_DIR: &str = crate::daemon::SESSION_BASE;

pub const LOG_NAME: &str = "log.jsonl";

pub const CONFIG_NAME: &str = "config.toml";

/// What a session's creator supplies. Paths are absolute; nothing here is read
/// from the process environment or working directory by `prepare`.
pub struct SessionRequest {
    /// The session's working root (docs/control-channel.md).
    pub cwd: PathBuf,
    /// The session's base environment (docs/env.md).
    pub base_env: crate::env_table::EnvSnapshot,
    /// The session dir to mint: `<cwd>/.efference/<id>` for the CLI.
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
    pub keep_scratch: bool,
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
/// in the order the boot report prints it. Fails before minting anything when the
/// config, a reference, the provider, or the effort level is unusable; a failure
/// after that point rolls back what this attempt minted, so the name is free for a
/// corrected retry (docs/control-channel.md).
///
/// Spawned resources need no entry in [`Minted`]: an stdio MCP server's child is
/// killed when the toolbox holding it drops, which the error return does.
pub async fn prepare(request: SessionRequest) -> Result<SessionBoot<MountedSpotter>, Error> {
    let mut minted = Minted {
        keep_scratch: request.keep_scratch,
        ..Minted::default()
    };
    match prepare_minting(request, &mut minted).await {
        Ok(boot) => Ok(boot),
        Err(error) => {
            minted.roll_back();
            Err(error)
        }
    }
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
        cwd,
        base_env,
        session_dir,
        config,
        inference,
        temperature,
        effort,
        upstream,
        autonomous,
        reauth,
        keep_scratch,
        host,
        lineage,
    } = request;
    assert!(cwd.is_absolute(), "session cwd must be absolute");
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
    let origin = match crate::config::load_in(&cwd, config.as_deref()) {
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
        cwd.clone(),
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

    let log_path = session_dir.join(LOG_NAME);
    metadata.effort = effort.clone();
    let mut log = Log::create(&log_path, Some(metadata))?;
    // In force before the first append — the boot `Preamble` below is the first
    // entry, and its `[context]` exec snapshot is exactly the kind of env capture
    // masking exists for.
    inference.absorb_secrets(&env_policy.masker());
    log.set_masker(env_policy.masker());
    eprintln!("session dir: {}", session_dir.display());
    eprintln!("logging to {}", log_path.display());

    // The session's id is its dir's name (docs/control-api.md): the address the
    // control channel, the `sessions` mount's caller field, and the identity line
    // in the system prompt all use, so it is read once here.
    let session_id = session_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Build the system prompt once, here at startup, and freeze it into the cached
    // prefix — it must stay byte-stable for the whole session or it would invalidate
    // the cache every turn (docs/log-and-projection.md). Everything it folds in is resolved
    // once at boot: the [context] block (files read + exec sections run now, frozen —
    // docs/context.md; orientation rides here as a config exec entry), the role's
    // brief, and the config note.
    let tools = parts::mount_toolbox(parts::ToolboxInputs {
        cwd: &cwd,
        scratch: &scratch,
        loaded: &loaded,
        env_policy: &env_policy,
        capabilities: &capabilities,
        endpoint: &endpoint,
        host,
        session_id: &session_id,
        reauth: &reauth,
    })
    .await?;
    let parts::MountedTools {
        toolbox,
        unmounted_section,
    } = tools;

    let preamble = parts::freeze_preamble(
        &loaded,
        &cwd,
        &env_policy,
        model,
        &session_id,
        &lineage,
        unmounted_section.as_deref(),
    )?;
    let parts::FrozenPreamble {
        system,
        resolved,
        context_block,
    } = preamble;
    // Snapshot the rendered boot preamble into the log (docs/context.md): the
    // [context] block folds in file contents that drift on disk, so the preamble is no longer
    // reconstructable from config + current files — this one entry keeps the log the complete
    // record of the wire. `efference dev wire` reads it back instead of rebuilding.
    log.append(
        0,
        crate::log::Payload::Preamble {
            system: system.clone(),
            context: resolved,
            inference: Some(recorded_inference),
        },
    )?;

    let client = parts::build_client(
        &endpoint,
        model,
        system,
        parts::ClientSettings {
            temperature,
            upstream,
            effort,
        },
        &loaded,
        &capabilities,
    );

    let spotter = parts::mount_spotter(&inference, &endpoint, &scratch, &cwd).await?;

    if autonomous {
        eprintln!(
            "autonomous: no synchronous gate — spotter Watch/Gate redirects the model to its \
             config file; config writes still gate for the orchestrator-above"
        );
    }
    let distiller = parts::mount_distiller(
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
        distiller,
        spotter,
        log,
        paths: crate::daemon::SessionPaths::new(cwd.clone(), session_dir.canonicalize()?)?,
        role: loaded,
        autonomous,
        capabilities,
        model: model.to_string(),
        inference: Some(inference.identity()),
        env_policy,
        keep_scratch,
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
mod test;
