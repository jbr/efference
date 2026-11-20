//! `efference dev spotter-request` — the request a spotter rung was sent for a recorded
//! call, built by the rung's own renderer from the session's log, config file and tools
//! (docs/permissions.md).
//!
//! What the session's files hold is used as they hold it; what they do not is stood in
//! for and named on stderr: the scratch dir's random name, the supervision facts (host
//! state), and config the file has since lost or gained beyond what the log's config
//! edits record. `dev spotter-eval` shares the log reading and the tool mount.

use super::*;
use crate::dev::{RungArg, SpotterRequestArgs};
use efference::config::policy::Directories;
use efference::config::{Config, McpServerEntry};
use efference::curation::spotter::recorded::{RecordedMove, held_in};
use efference::protocol::transcript::Handle;
use efference::record::log::LoggedInference;
use efference::record::names::tools::{AGENTS_NAMESPACE, BASH, LSP_NAMESPACE, namespace_of};
use efference::record::project::{UserPart, WireTurn};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
pub(crate) mod test;

/// The line that stands where the request places a cache breakpoint.
const BREAKPOINT_MARKER: &str = "◆ cache breakpoint\n";

/// A recorded call with what its session's spotter was given beside it.
pub(crate) struct Rebuilt {
    /// The session's id: its directory's name.
    pub(crate) session: String,
    /// The session's directory, absolute.
    pub(crate) session_dir: PathBuf,
    pub(crate) recorded: RecordedMove,
    /// The session config as the spotter read it at the call, the scratch grant
    /// included.
    pub(crate) role: Config,
    /// The session's project and working directories, which the request's scope block
    /// renders and resolves grants against.
    pub(crate) directories: Directories,
    /// The stand-in for the session's scratch dir, which no record names.
    pub(crate) scratch: PathBuf,
    /// The session's spotter selection at the call, when the log records one.
    pub(crate) spotter: Option<LoggedInference>,
    /// What may differ from the request the session sent, as sentences.
    pub(crate) notes: Vec<String>,
}

/// Rebuild the call `seq` or `call` names in the session whose log is `log`: the move
/// and its circumstances from the log, the config from the session's config file with
/// the log's later edits taken back out, the directories from the log's header. Reads
/// files only.
pub(crate) fn rebuild(
    log: &Path,
    seq: Option<Handle>,
    call: Option<&str>,
) -> Result<Rebuilt, CliError> {
    let session_dir = log
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let session = session_dir
        .file_name()
        .map_or_default(|name| name.to_string_lossy().into_owned());
    let recording = efference::record::persist::load_recording(log)?;
    let directories =
        Directories::recorded(recording.header.metadata(), &session_dir).ok_or_else(|| {
            format!(
                "{}: the log header records no working directory, and the log is not in a \
                 session directory",
                log.display()
            )
        })?;
    let recorded = RecordedMove::find(
        &session,
        &recording.entries,
        seq,
        call,
        held_in(directories.project()),
    )?;
    // The production loader: the file, with the self-surface grants laid over it.
    let file = efference::host::boot::load_session_config(
        &session_dir.join(efference::host::boot::CONFIG_NAME),
        &session_dir,
    )?;
    let (mut role, mut notes) = recorded.config(&file.role);
    notes.push(
        "the config is the session's config file as it is now, with the config edits the \
         log records after the call taken back out; its spotter notes, and any hand edit \
         that no config edit records, are as the file has them now"
            .to_string(),
    );
    // Every hosted session is granted its scratch dir, minted under a random name no
    // record keeps (docs/tools.md).
    let scratch = scratch_placeholder();
    role.paths.insert(
        scratch.to_string_lossy().into_owned(),
        efference::config::Perms {
            read: true,
            write: true,
        },
    );
    notes.push(format!(
        "the session's scratch dir is not recorded; `{}` stands in for it",
        scratch.display()
    ));
    if namespace_of(&recorded.judged.name) == AGENTS_NAMESPACE {
        notes.push(
            "the supervision facts are host state and not recorded, so none are shown; the \
             session's request carried them before the move"
                .to_string(),
        );
    }
    notes.extend(recorded.notes.iter().cloned());
    let before = match recorded.cut {
        Some(cut) => recording
            .entries
            .iter()
            .position(|entry| entry.seq == cut)
            .unwrap_or(recording.entries.len()),
        None => recording.entries.len(),
    };
    let spotter = efference::record::log::recorded_roles(&recording.entries[..before])
        .map(|roles| roles.spotter.clone())
        .or_else(|| {
            recording
                .header
                .metadata()
                .map(|metadata| metadata.spotter.clone())
        });
    Ok(Rebuilt {
        session,
        session_dir,
        recorded,
        role,
        directories,
        scratch,
        spotter,
        notes,
    })
}

/// The path rendered for a session's scratch dir, which is minted under the temp dir
/// with a random name.
fn scratch_placeholder() -> PathBuf {
    std::env::temp_dir().join("efference-<scratch>")
}

/// The toolbox a session's spotter read a structured call's brief from, mounted as the
/// session mounted it: the built-in tools, the `agents__*` verbs, the `lsp` tools when
/// `namespaces` names them, and each server the config declares that `namespaces`
/// names. A server that does not mount is left out, with a note; nothing asks for an
/// authorization.
pub(crate) async fn mount_toolbox(
    role: &Config,
    working: &Path,
    session: &str,
    namespaces: &BTreeSet<&str>,
) -> Result<(Toolbox, Vec<String>), CliError> {
    let mut toolbox = builtin_toolbox(working, session)?;
    let mut notes = Vec::new();
    // Binding starts the project's language servers, so it waits on a call naming one.
    if namespaces.contains(LSP_NAMESPACE) && efference::tools::lsp::enabled(role) {
        match efference::tools::lsp::Lsp::bind(
            efference::tools::lsp::language_servers(),
            session,
            working.to_path_buf(),
        )
        .await
        {
            Ok(lsp) => toolbox.mount_lsp(lsp),
            Err(error) => notes.push(format!(
                "the `lsp` tools did not bind ({error}); their calls render without a brief"
            )),
        }
    }
    let servers: BTreeMap<String, McpServerEntry> = role
        .mcp_servers
        .iter()
        .filter(|(name, _)| namespaces.contains(name.as_str()))
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect();
    notes.extend(mount_servers(&mut toolbox, servers, working).await?);
    toolbox.retain_enabled(|name| role.tool_enabled(name));
    Ok((toolbox, notes))
}

/// The tools every hosted session mounts whatever its config declares: bash, the
/// built-in file and edit tools, and the in-session `agents__*` verbs as the session
/// `session` holds them. The verbs are built over a host; an empty one serves their
/// definitions, and nothing here runs them.
pub(crate) fn builtin_toolbox(working: &Path, session: &str) -> Result<Toolbox, CliError> {
    let mut toolbox = Toolbox::bash_and_mcps(working.to_path_buf(), None)?;
    let host = efference::host::Host::new(
        efference::inference::InferenceRegistry::empty(),
        efference::config::env_table::EnvSnapshot::new(Default::default())?,
    );
    toolbox.mount_control_server(efference::clients::control_tools::in_session_server(
        &host,
        session.to_string(),
    ));
    Ok(toolbox)
}

/// Mount `servers` into `toolbox` through the boot's own mount, with the stored
/// credentials and never a sign-in, each as optional so one failure leaves the rest
/// mounted. Returns a note per server that did not mount.
pub(crate) async fn mount_servers(
    toolbox: &mut Toolbox,
    servers: BTreeMap<String, McpServerEntry>,
    cwd: &Path,
) -> Result<Vec<String>, CliError> {
    if servers.is_empty() {
        return Ok(Vec::new());
    }
    let servers: BTreeMap<String, McpServerEntry> = servers
        .into_iter()
        .map(|(name, entry)| {
            (
                name,
                McpServerEntry {
                    optional: true,
                    ..entry
                },
            )
        })
        .collect();
    let secrets = std::sync::Arc::new(efference::platform::secrets::Secrets::configured(
        efference::config::user_config::InferenceConfig::user_secret_commands()?,
    ));
    let store: std::sync::Arc<dyn efference::tools::mcp::auth::store::TokenStore> =
        std::sync::Arc::new(efference::tools::mcp::auth::store::KeychainStore::new(
            secrets,
        ));
    let unmounted = efference::host::boot::mcp_mount::mount_all(
        toolbox,
        &servers,
        cwd,
        &store,
        &[],
        &efference::host::boot::mcp_mount::AuthorizationHandling {
            refuse: true,
            waiting: None,
        },
    )
    .await?;
    Ok(unmounted
        .into_iter()
        .map(|server| {
            format!(
                "the MCP server `{}` did not mount ({}); its tools render without a brief",
                server.name, server.reason
            )
        })
        .collect())
}

/// `efference dev spotter-request`: print a recorded call's spotter request, and on
/// stderr what may differ from the one the session sent.
pub(crate) async fn spotter_request(args: SpotterRequestArgs) -> CliResult {
    let SpotterRequestArgs {
        session,
        read,
        seq,
        call,
        rung,
        system,
        json,
    } = args;
    let target = session
        .or(read.name)
        .ok_or_else(|| CliError::from(format!("name a session to read.{}", session_listing())))?;
    let (log, _) = resolve_session_log(&target)?;
    let seq = seq.as_deref().map(Handle::parse).transpose()?;
    let rebuilt = rebuild(&log, seq, call.as_deref())?;
    let mut notes = rebuilt.notes.clone();

    // Bash is judged as its command text, so only a structured call needs the tools.
    let judged = &rebuilt.recorded.judged;
    let toolbox = match judged.name == BASH {
        true => None,
        false => {
            let namespaces = BTreeSet::from([namespace_of(&judged.name)]);
            let (toolbox, mounted) = mount_toolbox(
                &rebuilt.role,
                rebuilt.directories.working(),
                &rebuilt.session,
                &namespaces,
            )
            .await?;
            notes.extend(mounted);
            Some(toolbox)
        }
    };
    let brief = toolbox
        .as_ref()
        .and_then(|toolbox| efference::agent::brief(toolbox, &judged.name));
    let circumstances = rebuilt.recorded.circumstances(brief);

    let rendered = match json {
        true => request_bodies(&rebuilt, &circumstances, rung).await?,
        false => render(
            &rebuilt,
            &circumstances,
            rung.unwrap_or_else(|| judging_rung(&rebuilt)),
            system,
        )?,
    };
    if let Some((seq, spot)) = &rebuilt.recorded.spot {
        eprintln!(
            "verdict at seq {seq}: {:?} — {}",
            spot.stance,
            spot.reason.trim()
        );
    }
    if let Some(spotter) = &rebuilt.spotter {
        eprintln!(
            "the session's spotter: {}/{} ({})",
            spotter.connection, spotter.model, spotter.provider
        );
    }
    for note in &notes {
        eprintln!("note: {note}");
    }
    print!("{rendered}");
    Ok(())
}

/// The text view of `rung`'s request for the rebuilt call; `system` puts the prose
/// rung's persona first.
pub(crate) fn render(
    rebuilt: &Rebuilt,
    circumstances: &efference::curation::spotter::Circumstances<'_>,
    rung: RungArg,
    system: bool,
) -> Result<String, CliError> {
    let judged = &rebuilt.recorded.judged;
    Ok(match rung {
        RungArg::Prose => {
            let (persona, turns) = efference::curation::spotter::preview_request(
                &rebuilt.role,
                judged,
                circumstances,
                &rebuilt.directories,
                Some(&rebuilt.scratch),
            );
            render_prose(system.then_some(persona), &turns)
        }
        RungArg::Jev => render_jev(&efference::curation::spotter::jev::requests(
            &rebuilt.role,
            judged,
            circumstances,
            Some(&rebuilt.directories),
            Some(&rebuilt.scratch),
        ))?,
    })
}

/// The rung that judged the call, read off its verdict: the decision rung records the
/// axes behind its stance and the prose rung records none. A call with no verdict, or a
/// fail-closed one, which records no axes either, gets the prose rung.
fn judging_rung(rebuilt: &Rebuilt) -> RungArg {
    match &rebuilt.recorded.spot {
        Some((_, spot)) if spot.axes.is_some() => RungArg::Jev,
        _ => RungArg::Prose,
    }
}

/// The bodies the session's spotter connection sends for the call: that connection
/// resolved from the operator's config, its rung chosen by what the provider says the
/// model is, as boot chooses it.
async fn request_bodies(
    rebuilt: &Rebuilt,
    circumstances: &efference::curation::spotter::Circumstances<'_>,
    rung: Option<RungArg>,
) -> Result<String, CliError> {
    let spotter = rebuilt
        .spotter
        .as_ref()
        .ok_or("the log records no spotter selection to resolve")?;
    let (model, mut endpoint) =
        inference::spotter_endpoint(Some(&format!("{}/{}", spotter.connection, spotter.model)))?;
    // The key boot gives the session's endpoints, which the spotter's derives from.
    endpoint.session_id = Some(efference::inference::api::sticky_session_id(
        &rebuilt.session_dir,
    ));
    let kind = endpoint.spotter_kind(&model).await?;
    let capabilities = endpoint.spotter_capabilities(&model, kind).await?;
    let mounted = MountedSpotter::mount(
        endpoint.for_spotter(),
        &model,
        kind,
        Some(rebuilt.scratch.clone()),
        rebuilt.directories.clone(),
    )
    .with_capabilities(capabilities.as_ref());
    let own = match &mounted {
        MountedSpotter::Fork(_) => RungArg::Prose,
        MountedSpotter::Jev(_) => RungArg::Jev,
    };
    if rung.is_some_and(|rung| rung != own) {
        return Err(format!(
            "the session's spotter, {}/{}, is the {} rung, so its client sends no other \
             rung's body",
            spotter.connection,
            spotter.model,
            mounted.kind()
        )
        .into());
    }
    let mut out = String::new();
    for body in mounted.request_bodies(&rebuilt.role, &rebuilt.recorded.judged, circumstances)? {
        let body: serde_json::Value = serde_json::from_str(&body)?;
        out.push_str(&serde_json::to_string_pretty(&body)?);
        out.push('\n');
    }
    Ok(out)
}

/// The prose rung's request as text: with `persona`, the system prompt first; then each
/// part of the user turn as written, and a marker line where the request places a cache
/// breakpoint. A part that does not end its last line has one ended here, so a marker
/// starts its own line.
fn render_prose(persona: Option<&str>, turns: &[WireTurn]) -> String {
    let mut out = String::new();
    let push = |out: &mut String, text: &str| {
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    };
    if let Some(persona) = persona {
        out.push_str("── system ──\n");
        push(&mut out, persona);
        out.push_str("── user ──\n");
    }
    for turn in turns {
        match turn {
            WireTurn::User(parts) => {
                for part in parts {
                    match part {
                        UserPart::Text(text) => push(&mut out, text),
                        UserPart::Breakpoint => out.push_str(BREAKPOINT_MARKER),
                        // The spotter's turn carries prose only.
                        UserPart::ToolResult { content, .. } => push(&mut out, content),
                    }
                }
            }
            WireTurn::Breakpoint => out.push_str(BREAKPOINT_MARKER),
            // The spotter's request is one user turn.
            WireTurn::Assistant(_) => {}
        }
    }
    out
}

/// The decision rung's requests as text: the cold request's state and questions, then
/// the relation request's, or a line saying it is not sent.
fn render_jev(
    requests: &efference::curation::spotter::jev::Requests<'_>,
) -> Result<String, CliError> {
    let mut out = String::new();
    let mut section = |title: &str, value: String| {
        out.push_str(&format!("── {title} ──\n{value}\n"));
    };
    section(
        "cold request: state",
        serde_json::to_string_pretty(&requests.state)?,
    );
    section(
        "cold request: questions",
        serde_json::to_string_pretty(&requests.questions)?,
    );
    match &requests.relation {
        Some(relation) => {
            section(
                "relation request: state",
                serde_json::to_string_pretty(&relation.state)?,
            );
            section(
                "relation request: questions",
                serde_json::to_string_pretty(&relation.questions)?,
            );
        }
        None => out
            .push_str("── relation request: not sent, no consented call of the move's family ──\n"),
    }
    Ok(out)
}
