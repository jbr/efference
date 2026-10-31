//! Reopening a session from its log (docs/log-and-projection.md).
//!
//! [`resume`] is the second boot entry beside [`prepare`](super::prepare): the session
//! directory exists, its own `config.toml` is the live config, the log is reopened rather
//! than created, and the frozen preamble is read back from the log instead of rendered
//! again. Everything between those is the shared steps (`model`, `scaffold`, `env`,
//! `parts`), so a resumed session is built by the same code a created one is.
//!
//! A log whose last completion is clean (no calls, or every call answered and decided)
//! resumes after its last entry, and one with calls that never ran resumes at that batch
//! (`Resumed.pending_calls`); a call that started and has no result rolls back to just
//! before the completion ([`resume_point`]), and the
//! `Resumed` entry records where. The reopen itself is the last fallible
//! step: everything before it reads the log without writing, so a refusal or a failed boot
//! leaves the session directory as it was found. The directory is never in [`Minted`]; only
//! the scratch this attempt makes is.
//!
//! State the log carries is restored where it is consumed, not here: the turn counter from
//! the log's last turn (`agent::run`), the title from the latest `Title` entry, and the
//! background-process numbering past the highest `p#<n>` the log mentions (both in
//! `Session::full`).

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::mint::Minted;
use super::{CONFIG_NAME, env, model, parts, scaffold};
use crate::api::Error;
use crate::daemon::SessionBoot;
use crate::inference::{InferenceRegistry, ResumeSwitch, SessionOptions};
use crate::log::{LogEntry, LoggedInference, Payload, Resumed, Timestamp};
use crate::persist::{LoadedRecording, Log, SessionMetadata};
use crate::recovery::{Resumption, resume_point};
use crate::session_dir::{LOG_NAME, SESSION_BASE};
use crate::spotter::MountedSpotter;

/// What a resume needs from whoever resumes. The session's working directory, role
/// selections, and flags come from the log's header; what the header cannot hold (a
/// credential, the caller's environment, a host) is supplied here.
pub struct ResumeRequest<'a> {
    /// The existing session dir, absolute: `<project>/.efference/<id>`.
    pub session_dir: PathBuf,
    /// The base environment the resumed session's commands start from (docs/env.md).
    pub base_env: crate::env_table::EnvSnapshot,
    /// The connections the recorded role selections are re-resolved against.
    pub inference: &'a InferenceRegistry,
    pub reauth: Vec<String>,
    /// What an MCP server asking for authorization meets (docs/mcp-auth.md).
    pub authorization: super::mcp_mount::AuthorizationHandling,
    /// Spotter and triage selections to resume on instead of the recorded ones; empty
    /// keeps the recorded roles (docs/resumption.md).
    pub switch: ResumeSwitch,
    /// The hosting host, for the in-session `sessions` mount (docs/tools.md).
    pub host: Option<std::sync::Arc<crate::daemon::Host>>,
}

/// Reopen a session: [`prepare`](super::prepare)'s counterpart for a directory that
/// already holds a log. A failure removes and alters nothing under `session_dir`;
/// what it removes is the scratch dir it made.
///
/// The selections are also returned, inferred from the header rather than supplied
/// — the caller has no options to give, and a resumed session that later hands its
/// work off needs to hand over the model it is actually running on.
pub async fn resume(
    request: ResumeRequest<'_>,
) -> Result<(SessionBoot<MountedSpotter>, SessionOptions), Error> {
    let mut minted = Minted::default();
    // On an error, and when this future is dropped mid-await, `minted` rolls back as
    // it drops.
    let boot = resume_minting(request, &mut minted).await?;
    minted.keep();
    Ok(boot)
}

// One ordered protocol: every step before the reopen only reads, so a refusal leaves the
// session directory as it was found, and the reopen is the one step that writes; `minted`
// holds only the scratch this attempt makes. The steps a create shares are functions already,
// and what is inline is the resuming-only sequence, whose order would be readable only across
// pieces if it were cut apart.
#[allow(clippy::too_many_lines)]
async fn resume_minting(
    request: ResumeRequest<'_>,
    minted: &mut Minted,
) -> Result<(SessionBoot<MountedSpotter>, SessionOptions), Error> {
    let ResumeRequest {
        session_dir,
        base_env,
        inference: registry,
        reauth,
        authorization,
        switch,
        host,
    } = request;
    assert!(session_dir.is_absolute(), "session dir must be absolute");
    let log_path = session_dir.join(LOG_NAME);
    let session_config = session_dir.join(CONFIG_NAME);
    if !session_config.is_file() {
        return Err(format!(
            "cannot resume {}: its {CONFIG_NAME} is missing",
            session_dir.display()
        )
        .into());
    }

    // Read-only until the reopen below: a refusal here has written nothing.
    let recording = crate::persist::load_recording(&log_path).map_err(std::io::Error::from)?;
    resume_point(&recording.entries)
        .map_err(|why| format!("cannot resume {}: {why}", session_dir.display()))?;
    let (recorded_identity, body, context_block) =
        frozen_preamble(&recording.entries).ok_or_else(|| {
            format!(
                "cannot resume {}: the log holds no boot preamble",
                session_dir.display()
            )
        })?;
    let metadata = recording.header.metadata();
    let directories = session_directories(metadata, &session_dir)?;
    let session_id = session_dir
        .file_name()
        .map_or_default(|name| name.to_string_lossy().into_owned());
    // A session's own worktree that archiving removed is re-created below, once the
    // child environment exists; any other missing directory refuses here. A working
    // directory inside another session's worktree is not this session's to re-create.
    let restores_worktree = !directories.working().is_dir()
        && directories.working()
            == crate::session_dir::worktree::path(directories.project(), &session_id);
    for (what, dir) in [
        ("project directory", directories.project()),
        ("working directory", directories.working()),
    ] {
        if !dir.is_dir() && !(restores_worktree && what == "working directory") {
            return Err(format!(
                "cannot resume {}: its {what} {} is gone",
                session_dir.display(),
                dir.display()
            )
            .into());
        }
    }

    let mut loaded = scaffold::load_session_config(&session_config, &session_dir)?;
    let mut options = options_from_log(&recording, &loaded, &switch);
    let selected = options.effort.clone();
    let inference = registry.prepare(registry.resolve_options(&options, None, None))?;
    let (main_selection, mut endpoint) = inference.main();
    let model = main_selection.model.as_str();
    // A main model other than the recorded one: the identity line is rewritten and the
    // session resumes cold. Refused before anything is written, like every check here.
    let main = LoggedInference::new(
        &*main_selection.connection,
        main_selection.provider,
        &*main_selection.model,
    );
    let main_switched = switch.switches_main() && recorded_main(&recording).as_ref() != Some(&main);
    if main_switched {
        check_main_switch(&recording.entries, &recorded_identity, main_selection)
            .map_err(|why| format!("cannot switch the main model of {session_id}: {why}"))?;
    }
    if main_switched {
        // Handed on as the native id, as a header-recorded model is.
        options.connection = Some(main.connection.clone());
        options.model = Some(main.model.clone());
    }
    let identity = if main_switched {
        crate::env::identity_line(model)
    } else {
        recorded_identity.clone()
    };
    let system = format!("{identity}{body}");
    endpoint.session_id = Some(crate::api::sticky_session_id(&session_dir));

    let upstream = metadata.map_or_default(|m| m.upstream.clone());
    let temperature = metadata.and_then(|m| m.temperature);
    let autonomous = metadata.is_some_and(|m| m.autonomous);
    let keep_scratch = metadata.is_some_and(|m| m.keep_scratch);
    minted.keep_scratch = keep_scratch;
    let capabilities = model::resolve_capabilities(
        &endpoint,
        model,
        &upstream,
        temperature,
        selected.as_deref(),
    )
    .await?;

    let scratch = scaffold::mint_scratch(minted, &mut loaded)?;

    let resolver = std::sync::Arc::new(crate::env_table::Resolver::with_environment(
        directories.project().to_path_buf(),
        base_env.clone(),
    ));
    let env_policy = env::build_env_policy(&resolver, &loaded.role.env, &inference, base_env)?;
    inference.absorb_secrets(&env_policy.masker());

    // Before the toolbox mounts on the working directory. The path and branch are the
    // ones the frozen preamble names. Git runs the project's `post-checkout` hook, so
    // this is on a blocking thread. A later refusal leaves the worktree in place: it
    // is the directory this session was already entitled to.
    let restored = if restores_worktree {
        let (project, id, child_env) = (
            directories.project().to_path_buf(),
            session_id.clone(),
            env_policy.child_env(),
        );
        let added = tokio::task::spawn_blocking(move || {
            crate::worktree::restore(&project, &id, &child_env)
        })
        .await
        .map_err(|e| format!("worktree restore task failed: {e}"))?
        .map_err(|e| {
            format!(
                "cannot resume {}: could not re-create its worktree: {e}",
                session_dir.display()
            )
        })?;
        Some(added)
    } else {
        None
    };

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
    let client = parts::build_client(
        &endpoint,
        model,
        system,
        parts::ClientSettings {
            temperature,
            upstream,
            effort: selected,
        },
        &loaded,
        &capabilities,
    );
    let spotter = parts::mount_spotter(&inference, &endpoint, &scratch, &directories).await?;
    let (triager, triage_prices) = parts::mount_triager(
        &inference,
        &endpoint,
        model,
        context_block.as_deref(),
        &env_policy,
        &loaded,
    )
    .await?;

    // The one step that writes (a torn tail is repaired), so it comes after every step
    // that can refuse. Its lock refuses a second writer on this log. The project's
    // `.efference/` is tightened with it, so a refused resume changes no mode either
    // (docs/state-store.md).
    if let Some(base) = session_dir.parent() {
        crate::daemon::permissions::tighten_project_base(base);
    }
    let opened = Log::open(&log_path).map_err(std::io::Error::from)?;
    let mut log = opened.log;
    if let Some(repair) = &opened.repaired {
        log::debug!("log: repaired a torn tail ({repair:?})");
    }
    // The log can have moved between the read-only load and the reopen; the entries the
    // session continues from are the reopened ones.
    let resumption = resume_point(log.entries())
        .map_err(|why| format!("cannot resume {}: {why}", session_dir.display()))?;
    log.set_masker(env_policy.masker());
    let (turn, mut resumed) = resumed_entry(log.entries(), resumption);
    // Against the last `tools[]` the log recorded: a difference is named in the notice,
    // and the new array is recorded so the next resume compares with it. A log that
    // never recorded one has nothing to compare with.
    let advertised = crate::agent::tool_definitions(&tools.toolbox);
    let recorded = crate::log::recorded_tools(log.entries());
    if let Some(recorded) = recorded {
        resumed.tools_changed = crate::log::ToolsChange::between(recorded, &advertised);
    }
    let record_tools = recorded.is_none_or(|recorded| recorded != advertised.as_slice());
    log.append(turn, Payload::Resumed(resumed))?;
    // A switched role is recorded as a preamble that repeats the frozen one with the
    // new roles, so the latest preamble stays the record a later resume reads. Only a
    // difference is written: a switch naming what the session already runs adds nothing.
    let roles = super::metadata::roles(&inference);
    let roles_switched = !switch.is_empty() && recorded_roles(&recording).as_ref() != Some(&roles);
    if (main_switched || roles_switched)
        && let Some(preamble) = latest_preamble(log.entries())
    {
        let mut preamble = preamble.clone().with_roles(roles);
        if main_switched {
            preamble = preamble.with_main(identity.clone(), main);
        }
        log.append(turn, preamble)?;
    }
    if record_tools {
        log.append(
            turn,
            Payload::Telemetry(crate::log::Telemetry::Tools { tools: advertised }),
        )?;
    }
    // After `Resumed`, so the gap and turn it records still read the log as it was left.
    if let Some(added) = restored {
        log.append(turn, Payload::Worktree(added.into_event(true)))?;
    }

    Ok((
        SessionBoot {
            client,
            toolbox: tools.toolbox,
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
            cache_lost_at: main_switched.then(Timestamp::now),
        },
        options,
    ))
}

/// The directories the session ran in: the header's project (`cwd`) and working
/// directory (the project when none is recorded), else the project root a
/// `<project>/.efference/<id>` directory implies, with the session working there.
fn session_directories(
    metadata: Option<&SessionMetadata>,
    session_dir: &std::path::Path,
) -> Result<crate::policy::Directories, Error> {
    if let Some(metadata) = metadata {
        let working = metadata.working.clone().unwrap_or(metadata.cwd.clone());
        return Ok(crate::policy::Directories::new(
            metadata.cwd.clone(),
            working,
        ));
    }
    let base = session_dir.parent();
    match base.and_then(|base| Some((base, base.parent()?))) {
        Some((base, root)) if base.ends_with(SESSION_BASE) => {
            Ok(crate::policy::Directories::at(root))
        }
        _ => Err(format!(
            "cannot resume {}: the log header records no working directory",
            session_dir.display()
        )
        .into()),
    }
}

/// The role selections the log recorded, as the options the registry resolves, with
/// `switch` laid over them. A log with no header metadata names only its main inference
/// (from the boot `Preamble`); the other roles and the effort level resolve as a create
/// would.
fn options_from_log(
    recording: &LoadedRecording,
    loaded: &crate::config::LoadedConfig,
    switch: &ResumeSwitch,
) -> SessionOptions {
    let mut options = match recording.header.metadata() {
        Some(metadata) => SessionOptions {
            effort: metadata.effort.clone(),
            temperature: metadata.temperature,
            ..SessionOptions::default()
        },
        None => SessionOptions {
            effort: loaded.role.transport.effort.clone(),
            ..SessionOptions::default()
        },
    };
    recorded_selections(recording).apply(&mut options);
    switch.apply(&mut options);
    options
}

/// The latest boot preamble, which a switch repeats with its new roles.
fn latest_preamble(entries: &[LogEntry]) -> Option<&Payload> {
    entries
        .iter()
        .rev()
        .map(|entry| &entry.payload)
        .find(|payload| matches!(payload, Payload::Preamble { .. }))
}

/// The latest boot preamble's identity line and body, and the `[context]` block the triage
/// prompt is re-rendered from (docs/context.md).
fn frozen_preamble(entries: &[LogEntry]) -> Option<(String, String, Option<String>)> {
    entries.iter().rev().find_map(|entry| match &entry.payload {
        Payload::Preamble {
            identity,
            body,
            context,
            ..
        } => Some((identity.clone(), body.clone(), context.render())),
        _ => None,
    })
}

/// Why `target` cannot take over this log's main model, or `Ok` when it can: the log's
/// assistant turns are recorded in one wire dialect and the new model must speak it
/// (docs/resumption.md), and the preamble must have an identity line to replace.
fn check_main_switch(
    entries: &[LogEntry],
    recorded_identity: &str,
    target: &crate::user_config::ModelSelection,
) -> Result<(), String> {
    if recorded_identity.is_empty() {
        return Err(
            "its system prompt predates the identity line, so there is none to replace; \
             hand the work off to a new session instead"
                .into(),
        );
    }
    let recorded = entries.iter().find_map(|entry| match &entry.payload {
        Payload::Assistant { output, .. } => {
            Some(crate::project::WireTurn::Assistant(output.clone()))
        }
        _ => None,
    });
    let Some(turn) = recorded else {
        return Ok(());
    };
    let dialect = target
        .provider
        .inference_dialect()
        .ok_or_else(|| format!("{} serves no turns", target.connection))?;
    crate::project::check_dialect(&[turn], dialect).map_err(|conflict| conflict.to_string())
}

/// The `Resumed` entry for a log ending at `entries`, and the turn it files under: the
/// log's last, skipped entries included, so turn numbers never repeat. Lost processes are
/// the background handles spawned with no recorded exit, over the whole log: a spawn in a
/// skipped range may have started a process.
fn resumed_entry(entries: &[LogEntry], resumption: Resumption) -> (crate::log::Turn, Resumed) {
    let last = entries.last();
    let gap_ms = last.map_or(0, |e| Timestamp::now().0.saturating_sub(e.at.0));
    let mut running = BTreeSet::new();
    for entry in entries {
        if let Payload::ProcessEvent(event) = &entry.payload {
            match event.kind {
                crate::log::ProcessEventKind::Spawned(_) => {
                    running.insert(event.id);
                }
                crate::log::ProcessEventKind::Exited(_) => {
                    running.remove(&event.id);
                }
            }
        }
    }
    (
        last.map_or(0, |e| e.turn),
        Resumed {
            gap_ms,
            lost_processes: running
                .into_iter()
                .map(crate::process_monitor::handle)
                .collect(),
            resumes_after: resumption.resumes_after,
            pending_calls: resumption.pending,
            tools_changed: Default::default(),
        },
    )
}

mod recorded;
pub use recorded::recorded_selections;
use recorded::{recorded_main, recorded_roles};

#[cfg(test)]
mod test;
