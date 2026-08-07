//! `efference serve` — boot the session dir, wire the toolbox, and run the daemon (docs/README.md).

use super::*;

pub(crate) async fn serve(
    session_dir: PathBuf,
    model: &str,
    spotter_model: &str,
    config: Option<PathBuf>,
    autonomous: bool,
    keep_scratch: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Daemon logging: RUST_LOG controls verbosity (default: efference at info, deps
    // quiet). The transport (request sizes, full API error bodies), the agent loop
    // (capped tool results), and the distiller (fork-peek fallbacks) emit through `log`.
    // `try_init` so a second `serve` in-process (tests) doesn't panic.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,efference=info"),
    )
    .try_init();

    // Provider resolution (docs/transport.md): the declared EFFERENCE_PROVIDER
    // (default anthropic) implies the messages URL and auth shape; the secret arrives
    // alone via EFFERENCE_API_KEY (or the ANTHROPIC_API_KEY fallback) so it can be a
    // whole-value `op://…` reference under `op run`.
    let mut endpoint = efference::api::Endpoint::from_env()?;

    // The sticky-routing key (docs/transport.md): derived once here, from the session
    // dir, which is fixed for the session's life. It pins the session to one upstream
    // so consolidation — which rewrites the opening messages, and with them the key
    // OpenRouter would otherwise derive by hashing them — can't re-route mid-session.
    // Whether it rides a request is the provider's call; a session that never talks to
    // a provider with sticky routing simply derives a key nothing reads.
    endpoint.session_id = Some(efference::api::sticky_session_id(&session_dir));

    // The required model-metadata lookup (docs/transport.md): capability conditionals
    // (the context ceiling now; more as they land) must not run on guesses, so a failed
    // lookup fails serve rather than starting blind.
    let capabilities = match endpoint.fetch_capabilities(model).await {
        Ok(caps) => caps,
        Err(e) => {
            return Err(format!(
                "model metadata lookup for {model:?} on {} failed: {e}",
                endpoint.provider.name()
            )
            .into());
        }
    };
    eprintln!(
        "model: {model} · context {} tokens{}",
        capabilities.context_length,
        capabilities
            .max_output_tokens
            .map(|m| format!(" · max output {m}"))
            .unwrap_or_default()
    );

    // Everything for this session is colocated in its own dir (gitignored), standard-named since
    // the dir is the unique unit (docs/permissions.md).
    // Create it first; a re-`serve` of an existing `--name` reuses it.
    std::fs::create_dir_all(&session_dir)?;

    // Snapshot the origin config into the session's own `config.toml` *once* (a fresh session). The
    // origin (`--config`/the `efference.toml` probe) is a template consumed at boot; thereafter the
    // per-session snapshot is the live, editable, watched policy — so two sessions sharing an origin
    // diverge freely, and the session can `cat` its config to see its perimeter and edit it to
    // request changes. On resume the snapshot already exists and is kept verbatim.
    let session_config = session_dir.join(CONFIG_NAME);
    if !session_config.exists() {
        // Validate the origin first (fail fast on a malformed template, before binding anything),
        // then copy it verbatim (comments intact) — or scaffold an empty config when there's none.
        let origin = match efference::config::load(config.as_deref()) {
            Ok(l) => l,
            Err(e) => {
                e.eprint();
                return Err("could not load role config".into());
            }
        };
        match &origin.source {
            efference::config::ConfigSource::Path(p) => {
                std::fs::copy(p, &session_config)?;
                eprintln!(
                    "config: snapshot of {} → {}",
                    p.display(),
                    session_config.display()
                );
            }
            efference::config::ConfigSource::Default => {
                std::fs::write(&session_config, CONFIG_SCAFFOLD)?;
                eprintln!("config: scaffolded {}", session_config.display());
            }
        }
    } else {
        eprintln!("config: reusing {}", session_config.display());
    }

    // The per-session snapshot *is* the live config now (a malformed hand-edit on resume fails fast
    // here). Everything downstream — the preamble, fs-watch, `/state`, persisted config-edits — keys
    // off this path, not the origin.
    let mut loaded = match efference::config::load(Some(&session_config)) {
        Ok(l) => l,
        Err(e) => {
            e.eprint();
            return Err("could not load session config".into());
        }
    };

    // Layer the harness's self-surface protection over the operator's grants (docs/permissions.md).
    // The session dir holds this session's control socket, its append-only log, and — under the same
    // base — every *other* session's dir; all of it sits inside the operator's `.` grant, so nothing
    // stops the session from writing its own log (corrupting the log) or POSTing to its own
    // control socket (approving its own gates, injecting a user turn) without this. Deny the whole
    // base dir, then re-allow read-only on *this* session's `config.toml` — the sanctioned
    // scope-expansion path: the session reads its own perimeter and *edits* the file, which is a
    // write the fast-path won't clear, so it gates for the operator. Inserted after the operator
    // config so an equal-specificity operator key is overridden; `policy::path_granted`'s
    // longest-prefix rule still lets a strictly deeper operator grant win.
    loaded
        .role
        .paths
        .insert(BASE_DIR.to_string(), efference::config::Perms::default());
    loaded.role.paths.insert(
        session_config.to_string_lossy().into_owned(),
        efference::config::Perms {
            read: true,
            write: false,
        },
    );

    // The per-session scratch dir (docs/tools.md): minted mkdtemp-style, exported to
    // every tool child as `$TMPDIR`, and always granted rw — the sanctioned answer
    // to driven sessions reaching for `/tmp` (the most-repeated gate in the
    // spotter-log survey). It lives outside `.` and outside the session dir, so it
    // collides with neither the operator's grants nor the self-surface deny above.
    let scratch = mint_scratch_dir()?;
    eprintln!("scratch: {} (exported as $TMPDIR)", scratch.display());
    loaded.role.paths.insert(
        scratch.to_string_lossy().into_owned(),
        efference::config::Perms {
            read: true,
            write: true,
        },
    );

    let log_path = session_dir.join(LOG_NAME);
    let mut log = Log::create(&log_path)?;
    eprintln!("session dir: {}", session_dir.display());
    eprintln!("logging to {}", log_path.display());
    let socket = session_dir.join(SOCKET_NAME);
    let _ = std::fs::remove_file(&socket); // ignore if absent.

    // Build the system prompt once, here at startup, and freeze it into the cached
    // prefix — it must stay byte-stable for the whole session or it would invalidate
    // the cache every turn (docs/log-and-projection.md). Everything it folds in is resolved
    // once at boot: the [context] block (files read + exec sections run now, frozen —
    // docs/context.md; orientation rides here as a config exec entry), the role's
    // brief, and the config note.
    let cwd = std::env::current_dir()?;
    let role_section = loaded.role.preamble_section();
    // The [context] preamble builder (docs/context.md): resolve the operator's listed
    // files from cwd and fold the rendered block into the frozen prefix. Fail closed — configured
    // context that can't be read aborts startup rather than silently vanishing.
    let resolved = match efference::context::resolve(&loaded.role.context, &cwd) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("error: {e}");
            return Err("could not resolve [context] entries".into());
        }
    };
    let context_block = resolved.render();
    let system = efference::env::system_prompt(context_block.as_deref(), role_section.as_deref());
    // Snapshot the rendered boot preamble into the log (docs/context.md): the
    // [context] block folds in file contents that drift on disk, so the preamble is no longer
    // reconstructable from config + current files — this one entry keeps the log the complete
    // record of the wire. `efference wire` reads it back instead of rebuilding.
    log.append(
        0,
        efference::log::Payload::Preamble {
            system: system.clone(),
            context: resolved,
        },
    )?;

    let client = Client::new(endpoint.clone(), model, system);

    // The independent permission rung (docs/permissions.md): a fresh-context spotter
    // on its own Haiku-band client, built from the same endpoint. It re-decides the calls
    // the deterministic fast-path can't clear, before the human gate. It learns the
    // scratch dir so its scope block can alias `$TMPDIR` to the granted path. Its own
    // sticky key (`for_spotter`): same endpoint, but a prefix the agent lineage never
    // shares, so pointing both at one model doesn't pin them to one route.
    let spotter =
        ForkSpot::new(endpoint.for_spotter(), spotter_model).with_scratch(Some(scratch.clone()));

    // The model-driven fork (docs/redaction-fork.md): every tool result is peeked and the
    // canonical wire keeps only what the model retains. `Toolbox` is the docs/tools.md tool
    // layer; mount bash + the in-process MCP servers (fs-mcp + semantic-edit-mcp),
    // their session cwd seeded to where we're running (resolved once above for [context]).
    // The scratch dir rides in as bash's `$TMPDIR`; the daemon reads it back off the
    // toolbox for the expander and background spawns (one source of truth).
    let toolbox = Toolbox::bash_and_mcps(cwd, Some(scratch.clone()))?;
    if autonomous {
        eprintln!(
            "autonomous: no synchronous gate — spotter Watch/Gate redirects the model to its \
             config file; config writes still gate for the orchestrator-above"
        );
    }
    let result = daemon::serve(
        client,
        toolbox,
        ForkPeek,
        spotter,
        log,
        socket,
        loaded,
        autonomous,
        capabilities,
    )
    .await;
    // Scratch lifecycle (docs/tools.md): cleaned on session end unless retained as
    // a post-mortem artifact.
    if keep_scratch {
        eprintln!("scratch kept: {}", scratch.display());
    } else if let Err(e) = std::fs::remove_dir_all(&scratch)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("[scratch: could not remove {}: {e}]", scratch.display());
    }
    result
}

/// Dev tool: run the spotter on one `(scope, watch-for, paths, command)` and print
/// the verdict (docs/permissions.md). No daemon — it builds a throwaway config and a
/// bash [`ToolCall`] and calls the exact `assess` path a real gate would, so what it
/// prints is what the loop would see. For iterating on the persona and config.
///
/// [`ToolCall`]: efference::log::ToolCall
pub(crate) async fn spotter_check(
    scope: Option<String>,
    watch_for: Vec<String>,
    paths: Vec<String>,
    model: &str,
    probe: Option<String>,
    command: String,
) -> CliResult {
    let endpoint = efference::api::Endpoint::from_env()?;
    let role = efference::config::Config {
        scope,
        watch_for,
        paths: paths.iter().map(|s| parse_path_grant(s)).collect(),
        ..Default::default()
    };
    let call = efference::log::ToolCall {
        id: efference::log::CallId("toolu_spotter_check".into()),
        name: efference::bash::NAME.into(),
        input: serde_json::json!({ "command": command }),
    };
    let spotting = ForkSpot::new(endpoint, model)
        .assess(&role, &call, probe.as_deref())
        .await;
    println!("stance: {:?}", spotting.read.stance);
    println!("reason: {}", spotting.read.reason);
    if let Some(prefix) = &probe {
        // The probe's answer is the stance: `clear_always` (from a real model verdict) is the safe-vote.
        let safe = spotting.origin == efference::log::Origin::Model
            && spotting.read.stance == efference::log::Stance::ClearAlways;
        let verdict = if !safe {
            "not a safe-vote (stance wasn't clear_always) — no promotion".to_string()
        } else if let Some(key) = efference::policy::promotable_command(&role, prefix) {
            format!("safe-vote → would queue `{key}` as a Proposed(Command) at K=1")
        } else {
            "safe-vote, but already covered by the allowlist — nothing to queue".to_string()
        };
        println!("probe `{prefix}`: {verdict}");
    }
    println!("origin: {:?}", spotting.origin);
    if let Some(u) = spotting.usage {
        println!(
            "usage:  in {} · out {} · cache read {} · cache write {}",
            u.usage.input_tokens,
            u.usage.output_tokens,
            u.usage.cache_read_input_tokens,
            u.usage.cache_creation_input_tokens
        );
    }
    Ok(())
}
