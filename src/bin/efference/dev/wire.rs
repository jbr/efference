//! `efference dev wire`: the request a recorded session's next turn would send
//! (docs/operating.md).

use super::*;

pub(super) type WireResult = Result<String, Box<dyn std::error::Error + Send + Sync>>;

/// The model used only for a legacy log with no boot identity. New logs carry
/// their native model in the Preamble; this keeps the old view renderable until
/// those logs are replaced.
pub(super) const LEGACY_WIRE_MODEL: &str = "claude-haiku-4-5";

pub(super) fn wire_placements(
    identity: Option<&efference::log::LoggedInference>,
) -> (
    efference::project::ImagePlacement,
    efference::project::BreakpointPlacement,
) {
    identity
        .map(|selection| {
            (
                selection.provider.image_placement(&selection.model),
                selection.provider.breakpoint_placement(&selection.model),
            )
        })
        .unwrap_or_default()
}

pub(super) fn wire_model(identity: Option<&efference::log::LoggedInference>) -> String {
    identity
        .map(|selection| selection.model.clone())
        .unwrap_or_else(|| LEGACY_WIRE_MODEL.to_string())
}

/// Reconstruct the wire request for a recorded session JSONL — the
/// deterministic projection the assistant sees (docs/README.md,
/// docs/log-and-projection.md). No daemon, no socket, no network: load the log,
/// fold it to `messages`, place the trailing cache breakpoint, and render the
/// envelope a real turn would send (`tool_choice: auto`, docs/transport.md).
/// The recorded identity supplies the historical model, provider, and placement;
/// the recorded `system` and `tools[]` are read back, while the current code and cwd
/// rebuild the other envelope values (and `tools[]` for a log that never recorded one).
/// Notes on stderr flag the parts that may differ from the session's environment.
pub(crate) async fn wire(
    path: PathBuf,
    messages_only: bool,
    format: WireFormat,
    view_dialect: Option<ViewDialect>,
    config: Option<PathBuf>,
) -> WireResult {
    let entries = efference::persist::load(&path)?;
    // The same turns a live request encodes, trailing breakpoint included; the
    // dialect decides the wire they render on (docs/transport.md).
    let mut turns = efference::project::project_semantic(&entries);
    let recorded = efference::project::recorded_dialect(&turns)?;
    // The fixed identity is recorded once at boot. Placement is derived here, at
    // read time, rather than stored as a second copy that could drift from the
    // provider rules (docs/log-and-projection.md).
    let identity = efference::log::recorded_inference(&entries).cloned();
    let recorded_provider = identity.as_ref().map(|selection| selection.provider);
    let (placement, breakpoint) = wire_placements(identity.as_ref());
    if identity.is_none() {
        eprintln!(
            "note: this log has no recorded inference identity; using the default image and \
             cache-breakpoint placement."
        );
    }
    // Responses caching is model-specific; the offline view assumes the explicit
    // policy every model in the compiled catalog reports.
    let cache = efference::api::PromptCachePolicy::Explicit;

    // Check the recorded provider once, before either rendering path. A legacy log
    // has already been checked by `recorded_dialect`; with an identity, this is the
    // only provider whose dialect the offline view accepts. A translated view skips
    // it: rendering in another dialect is its point, and the translation refuses
    // what it cannot carry.
    if view_dialect.is_none()
        && let Some(provider) = recorded_provider
        && let Some(dialect) = provider.inference_dialect()
    {
        efference::project::check_dialect(&turns, dialect)?;
    }
    let dialect = recorded_provider
        .and_then(|provider| provider.inference_dialect())
        .or(recorded)
        .unwrap_or(efference::api::WireDialect::Messages);
    turns.push(efference::project::WireTurn::Breakpoint);
    let messages = efference::project::encode_messages(&turns, placement, breakpoint);

    // `--messages` returns the projected array before touching config, system, or
    // tools. Both the lean and full views use the log's dialect and placements.
    if messages_only {
        eprintln!(
            "note: {} log entries → {} wire messages.",
            entries.len(),
            messages.len()
        );
        let rendered = match (view_dialect, dialect, format) {
            (Some(ViewDialect::Completions), _, _) => {
                let model = wire_model(identity.as_ref());
                serde_json::to_string_pretty(&efference::api::translate_completions(
                    &turns, "", &model,
                )?)?
            }
            (None, efference::api::WireDialect::Responses, _) => {
                serde_json::to_string_pretty(&efference::api::encode_responses(&turns, "", cache))?
            }
            (None, _, WireFormat::Json) => efference::api::render_messages_pretty(&messages)?,
            (None, _, WireFormat::Yaml) => efference::api::render_messages_yaml(&messages)?,
        };
        return Ok(rendered);
    }

    let model = wire_model(identity.as_ref());
    // The `system` a session sent is recorded in the log as the boot preamble snapshot
    // (docs/context.md) — prefer it (the last one wins on a multi-boot log) so
    // the view shows exactly what the model saw, not a re-resolve of files that may have
    // drifted on disk since. Only logs that predate the snapshot fall back to a rebuild.
    let recorded_system = entries.iter().rev().find_map(|e| match &e.payload {
        efference::log::Payload::Preamble { system, .. } => Some(system.clone()),
        _ => None,
    });
    let cwd = std::env::current_dir()?;
    let system = if let Some(system) = recorded_system {
        eprintln!(
            "note: {} log entries → {} wire messages. `system` is the recorded boot \
             preamble; `tools[]` is {}.",
            entries.len(),
            messages.len(),
            if efference::log::recorded_tools(&entries).is_some() {
                "the recorded array"
            } else {
                "rebuilt from the current code and cwd"
            }
        );
        system
    } else {
        // No snapshot (a pre-snapshot log) — rebuild the envelope the same way boot does
        // (current cwd seeds both the env block and the MCP servers' session roots; the role
        // config folds its brief into the preamble).
        let loaded = match efference::config::load(config.as_deref()) {
            Ok(l) => l,
            Err(e) => {
                e.eprint();
                return Err("could not load role config".into());
            }
        };
        let role_section = loaded.role.preamble_section();
        // Best-effort for the dev view: if the [context] files don't resolve from the current cwd
        // (e.g. inspecting a session recorded elsewhere), note it and omit the block rather than fail.
        let context_block = match efference::context::resolve(&loaded.role.context, &cwd) {
            Ok(resolved) => resolved.render(),
            Err(e) => {
                eprintln!("note: [context] not resolved ({e}); omitting from the rebuilt envelope");
                None
            }
        };
        eprintln!(
            "note: {} log entries → {} wire messages. This log predates the boot-preamble \
             snapshot: `system`/`tools[]` are rebuilt from the current code and cwd, not the \
             recorded session.",
            entries.len(),
            messages.len()
        );
        // No unmounted-servers section: this offline view mounts no connected
        // providers at all (see the note below), so it has nothing to report
        // about which of them failed.
        // The session's id is its dir's name — the log sits at
        // `.efference/<id>/log.jsonl`, so the rebuilt identity line can name the
        // session this log belongs to (docs/context.md).
        let session_id = path
            .parent()
            .and_then(|dir| dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // No creator: `created_by` is host control state, never logged
        // (docs/tools.md), so an offline rebuild from a log cannot recover it —
        // a child's rebuilt preamble is missing the creator sentence its live
        // one carried. Sessions logged since the boot-preamble snapshot read
        // back the recorded `system` instead and are unaffected.
        efference::env::system_prompt(
            &model,
            &session_id,
            &efference::env::Lineage::default(),
            None,
            context_block.as_deref(),
            role_section.as_deref(),
            None,
        )
    };
    // The full tools[] a real turn sends — world tools + the harness tools
    // (`consolidate`, the docs/process-monitor.md and docs/inbox.md tools) — so the dev view matches.
    // (No scratch dir: the wire view renders, it never executes.) One known gap:
    // the recorded array is preferred (docs/log-and-projection.md); a rebuild mounts no
    // config-declared connected providers (docs/tools.md) —
    // mounting spawns processes / opens connections, which an offline view must
    // not — so a session with `[mcp.servers.*]` has more tools[] than this shows.
    let (tools, _) = efference::agent::recorded_or_rebuilt_tools(&entries, || {
        Toolbox::bash_and_mcps(cwd, None)
    })?;

    let choice = efference::api::ToolChoice::auto();
    let rendered = match (view_dialect, dialect, format) {
        (Some(ViewDialect::Completions), _, _) => {
            efference::api::render_completions_request_pretty(
                &model, &system, &tools, choice, &turns,
            )?
        }
        (None, efference::api::WireDialect::Responses, _) => {
            efference::api::render_responses_request_pretty(
                &model, &system, &tools, choice, &turns, cache,
            )?
        }
        (None, _, WireFormat::Json) => {
            efference::api::render_request_pretty(&model, &system, &tools, choice, &messages)?
        }
        (None, _, WireFormat::Yaml) => {
            efference::api::render_request_yaml(&model, &system, &tools, choice, &messages)?
        }
    };
    Ok(rendered)
}
