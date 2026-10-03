//! Reopening a session from its log (docs/log-and-projection.md).
//!
//! [`resume`] is the second boot entry beside [`prepare`](super::prepare): the session
//! directory exists, its own `config.toml` is the live config, the log is reopened rather
//! than created, and the frozen preamble is read back from the log instead of rendered
//! again. Everything between those is the shared phases (`model`, `scaffold`, `env`,
//! `parts`), so a resumed session is built by the same code a created one is.
//!
//! A log whose last completion is clean (no calls, or every call answered and decided)
//! resumes after its last entry; any other resumes from just before that completion's
//! assistant entry ([`rollback_point`]), and the `Resumed` entry records where. The reopen itself is the last fallible
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
use super::{BASE_DIR, CONFIG_NAME, LOG_NAME, env, model, parts, scaffold};
use crate::api::Error;
use crate::daemon::SessionBoot;
use crate::inference::{InferenceRegistry, SessionOptions, TriageChoice};
use crate::log::{LogEntry, Payload, Resumed, Seq, Timestamp};
use crate::persist::{LoadedRecording, Log, SessionMetadata, TriageModel};
use crate::spotter::MountedSpotter;

/// What a resume needs from whoever resumes. The session's working directory, role
/// selections, and flags come from the log's header; what the header cannot hold (a
/// credential, the caller's environment, a host) is supplied here.
pub struct ResumeRequest<'a> {
    /// The existing session dir, absolute: `<cwd>/.efference/<id>`.
    pub session_dir: PathBuf,
    /// The base environment the resumed session's commands start from (docs/env.md).
    pub base_env: crate::env_table::EnvSnapshot,
    /// The connections the recorded role selections are re-resolved against.
    pub inference: &'a InferenceRegistry,
    pub reauth: Vec<String>,
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
    match resume_minting(request, &mut minted).await {
        Ok(boot) => Ok(boot),
        Err(error) => {
            minted.roll_back();
            Err(error)
        }
    }
}

async fn resume_minting(
    request: ResumeRequest<'_>,
    minted: &mut Minted,
) -> Result<(SessionBoot<MountedSpotter>, SessionOptions), Error> {
    let ResumeRequest {
        session_dir,
        base_env,
        inference: registry,
        reauth,
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
    rollback_point(&recording.entries)
        .map_err(|why| format!("cannot resume {}: {why}", session_dir.display()))?;
    let (system, context_block) = frozen_preamble(&recording.entries).ok_or_else(|| {
        format!(
            "cannot resume {}: the log holds no boot preamble",
            session_dir.display()
        )
    })?;
    let metadata = recording.header.metadata();
    let cwd = session_cwd(metadata, &session_dir)?;
    if !cwd.is_dir() {
        return Err(format!(
            "cannot resume {}: its working directory {} is gone",
            session_dir.display(),
            cwd.display()
        )
        .into());
    }

    let mut loaded = scaffold::load_session_config(&session_config, &session_dir)?;
    let options = options_from_log(&recording, &loaded);
    let selected = options.effort.clone();
    let inference = registry.prepare(registry.resolve_options(&options, None, None))?;
    let (main_selection, mut endpoint) = inference.main();
    let model = main_selection.model.as_str();
    endpoint.session_id = Some(crate::api::sticky_session_id(&session_dir));

    let upstream = metadata.map(|m| m.upstream.clone()).unwrap_or_default();
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
        cwd.clone(),
        base_env.clone(),
    ));
    let env_policy = env::build_env_policy(&resolver, &loaded.role.env, &inference, base_env)?;
    inference.absorb_secrets(&env_policy.masker());

    let session_id = session_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
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
    let spotter = parts::mount_spotter(&inference, &endpoint, &scratch, &cwd).await?;
    let distiller = parts::mount_distiller(
        &inference,
        &endpoint,
        model,
        context_block.as_deref(),
        &env_policy,
        &loaded,
    )
    .await?;

    // The one step that writes (a torn tail is repaired), so it comes after every step
    // that can refuse. Its lock refuses a second writer on this log.
    let opened = Log::open(&log_path).map_err(std::io::Error::from)?;
    let mut log = opened.log;
    if let Some(repair) = &opened.repaired {
        eprintln!("log: repaired a torn tail ({repair:?})");
    }
    // The log can have moved between the read-only load and the reopen; the entries the
    // session continues from are the reopened ones.
    let resumes_after = rollback_point(log.entries())
        .map_err(|why| format!("cannot resume {}: {why}", session_dir.display()))?;
    log.set_masker(env_policy.masker());
    let (turn, resumed) = resumed_entry(log.entries(), resumes_after);
    log.append(turn, Payload::Resumed(resumed))?;
    eprintln!("resumed {}", session_dir.display());

    Ok((
        SessionBoot {
            client,
            toolbox: tools.toolbox,
            distiller,
            spotter,
            log,
            paths: crate::daemon::SessionPaths::new(cwd, session_dir.canonicalize()?)?,
            role: loaded,
            autonomous,
            capabilities,
            model: model.to_string(),
            inference: Some(inference.identity()),
            env_policy,
            keep_scratch,
        },
        options,
    ))
}

/// The working directory the session ran in: the header's, else the project root a
/// `<cwd>/.efference/<id>` directory implies.
fn session_cwd(
    metadata: Option<&SessionMetadata>,
    session_dir: &std::path::Path,
) -> Result<PathBuf, Error> {
    if let Some(metadata) = metadata {
        return Ok(metadata.cwd.clone());
    }
    let base = session_dir.parent();
    match base.and_then(|base| Some((base, base.parent()?))) {
        Some((base, root)) if base.ends_with(BASE_DIR) => Ok(root.to_path_buf()),
        _ => Err(format!(
            "cannot resume {}: the log header records no working directory",
            session_dir.display()
        )
        .into()),
    }
}

/// The role selections the header recorded, as the options the registry resolves.
/// A log with no header metadata names only its main inference (from the boot
/// `Preamble`); the other roles and the effort level resolve as a create would.
fn options_from_log(
    recording: &LoadedRecording,
    loaded: &crate::config::LoadedConfig,
) -> SessionOptions {
    match recording.header.metadata() {
        Some(metadata) => SessionOptions {
            connection: Some(metadata.main.connection.clone()),
            model: Some(metadata.main.model.clone()),
            spotter_connection: Some(metadata.spotter.connection.clone()),
            spotter_model: Some(metadata.spotter.model.clone()),
            triage: Some(match &metadata.triage {
                TriageModel::Shared => TriageChoice::Shared,
                TriageModel::Separate {
                    selection,
                    thinking,
                    auto_choice,
                    effort,
                } => TriageChoice::Separate {
                    connection: Some(selection.connection.clone()),
                    model: selection.model.clone(),
                    thinking: *thinking,
                    auto_choice: *auto_choice,
                    effort: effort.clone(),
                },
            }),
            effort: metadata.effort.clone(),
            temperature: metadata.temperature,
        },
        None => {
            let main = recording
                .entries
                .iter()
                .rev()
                .find_map(|e| match &e.payload {
                    Payload::Preamble { inference, .. } => inference.as_ref(),
                    _ => None,
                });
            SessionOptions {
                connection: main.map(|m| m.connection.clone()),
                model: main.map(|m| m.model.clone()),
                effort: loaded.role.transport.effort.clone(),
                ..SessionOptions::default()
            }
        }
    }
}

/// The latest boot preamble's system prompt, and the `[context]` block the triage
/// prompt is re-rendered from (docs/context.md).
fn frozen_preamble(entries: &[LogEntry]) -> Option<(String, Option<String>)> {
    entries.iter().rev().find_map(|entry| match &entry.payload {
        Payload::Preamble {
            system, context, ..
        } => Some((system.clone(), context.render())),
        _ => None,
    })
}

/// Where the session continues from: `None` when the log ends cleanly (its last live
/// completion has no calls, or every call has a result and every result a decision),
/// else `Some(seq)` of the last entry before that completion's assistant entry
/// (docs/resumption.md). The earlier prefix is checked the same way, and the walk
/// repeats until it ends cleanly. Entries an earlier `Resumed` skipped are not live: they
/// are neither the completion nor the rollback point. The error is the reason a log has
/// nothing to continue from.
pub(super) fn rollback_point(entries: &[LogEntry]) -> Result<Option<Seq>, String> {
    let mut end = entries.len();
    let mut rolled_back = None;
    loop {
        let live = &entries[..end];
        let skipped = crate::project::skipped_seqs(live);
        let is_live = |e: &LogEntry| !skipped.contains(&e.seq);
        // A record-only assistant entry (a failed stream) is not a completion.
        let Some(index) = live.iter().rposition(|e| {
            is_live(e)
                && matches!(
                    &e.payload,
                    Payload::Assistant {
                        upstream_failure: None,
                        ..
                    }
                )
        }) else {
            return Ok(rolled_back);
        };
        if completion_is_clean(
            &live[index],
            live[index + 1..].iter().filter(|e| is_live(e)),
        )? {
            return Ok(rolled_back);
        }
        let Some(before) = live[..index].iter().rposition(is_live) else {
            return Err("the log ends mid-batch with nothing before it to resume from".into());
        };
        rolled_back = Some(live[before].seq);
        end = before + 1;
    }
}

/// Whether every call of the assistant entry `completion` has a result in `after`, and
/// every result a decision.
fn completion_is_clean<'a>(
    completion: &LogEntry,
    after: impl Iterator<Item = &'a LogEntry> + Clone,
) -> Result<bool, String> {
    let Payload::Assistant { output, .. } = &completion.payload else {
        return Ok(true);
    };
    let calls = output
        .tool_calls()
        .map_err(|e| format!("the last completion's calls do not parse: {e}"))?;
    for call in calls {
        let result = after.clone().find_map(|entry| match &entry.payload {
            Payload::ToolResult { call_id, wire, .. } if *call_id == call.id => {
                Some((entry.seq, wire))
            }
            _ => None,
        });
        let Some((seq, wire)) = result else {
            return Ok(false);
        };
        let decided = !matches!(wire, crate::log::WireState::Pending)
            || after.clone().any(|entry| {
                matches!(&entry.payload, Payload::ResultVerdict { for_result, .. } if *for_result == seq)
            });
        if !decided {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The `Resumed` entry for a log ending at `entries`, and the turn it files under: the
/// log's last, skipped entries included, so turn numbers never repeat. Lost processes are
/// the background handles spawned with no recorded exit, over the whole log: a spawn in a
/// skipped range may have started a process.
fn resumed_entry(entries: &[LogEntry], resumes_after: Option<Seq>) -> (crate::log::Turn, Resumed) {
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
            lost_processes: running.into_iter().map(crate::supervisor::handle).collect(),
            resumes_after,
        },
    )
}

#[cfg(test)]
mod test;
