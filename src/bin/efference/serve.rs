//! `efference serve` — boot the session dir, wire the toolbox, and run the daemon (docs/README.md).

use super::*;

/// The raw `--bind` flag family, resolved by [`resolve_bind`] into the daemon's
/// [`TcpBind`] — or rejected before anything else boots.
///
/// [`TcpBind`]: efference::daemon::TcpBind
pub(crate) struct BindFlags {
    pub(crate) bind: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) tls_cert: Option<PathBuf>,
    pub(crate) tls_key: Option<PathBuf>,
    pub(crate) open: bool,
}

/// Validate the bind flags into a [`TcpBind`] (docs/web-bind.md), enforcing the
/// operator posture at the flag boundary: plaintext only on loopback, TLS wants an
/// explicit public name, and a bind without the machine root is refused rather than
/// silently unauthenticated.
///
/// [`TcpBind`]: efference::daemon::TcpBind
fn resolve_bind(
    flags: BindFlags,
) -> Result<Option<efference::daemon::TcpBind>, Box<dyn std::error::Error + Send + Sync>> {
    // An empty bind is the explicit "UDS-only" spelling: `--bind ""` overrides a
    // direnv'd EFFERENCE_BIND default (e.g. for a second concurrent session).
    let Some(addr) = flags.bind.filter(|a| !a.trim().is_empty()) else {
        return Ok(None);
    };
    use std::net::ToSocketAddrs;
    let resolved: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| format!("--bind {addr:?} did not resolve: {e}"))?
        .collect();
    let loopback = !resolved.is_empty() && resolved.iter().all(|a| a.ip().is_loopback());
    let tls = match (flags.tls_cert, flags.tls_key) {
        (Some(cert), Some(key)) => Some(efference::daemon::TlsPem {
            cert: std::fs::read(&cert)
                .map_err(|e| format!("could not read --tls-cert {}: {e}", cert.display()))?,
            key: std::fs::read(&key)
                .map_err(|e| format!("could not read --tls-key {}: {e}", key.display()))?,
        }),
        // clap's `requires` pairing makes one-without-the-other unrepresentable.
        _ => None,
    };
    if tls.is_none() && !loopback {
        return Err(format!(
            "--bind {addr} is not loopback: --tls-cert/--tls-key are required — \
             never cleartext on a routable interface (docs/web-bind.md)"
        )
        .into());
    }
    let host = match (flags.host, &tls) {
        (Some(host), _) => host,
        (None, None) => "localhost".into(),
        (None, Some(_)) => {
            return Err(
                "--bind with TLS needs --host <name> — the cert's DNS name, which browsers \
                 must address the daemon by"
                    .into(),
            );
        }
    };
    let root = efference::auth::Root::from_env().ok_or_else(|| {
        format!(
            "--bind requires {} (inject it with the op env-file pattern, like provider keys)",
            efference::auth::ROOT_ENV
        )
    })?;
    Ok(Some(efference::daemon::TcpBind {
        addr,
        tls,
        host,
        root,
        open: flags.open,
    }))
}

#[allow(clippy::too_many_arguments)] // one flag, one parameter; the CLI is the arity
pub(crate) async fn serve(
    session_dir: PathBuf,
    model: &str,
    spotter_model: &str,
    config: Option<PathBuf>,
    temperature: Option<u8>,
    autonomous: bool,
    reauth: Vec<String>,
    keep_scratch: bool,
    bind_flags: BindFlags,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Resolve the bind first: a bad flag combination or a missing root should fail
    // before a session dir is minted or a provider is dialed.
    let bind = resolve_bind(bind_flags)?;
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

    // Mount the config-declared connected providers (docs/tools.md) before the
    // first projection, so the session's tools[] is fixed from turn one — and
    // before the system prompt is built, because what did *not* mount is part
    // of what the session must know. A server that answers `401` is authorized
    // here, which is the one point in the boot that waits for a human
    // (docs/mcp-auth.md).
    let mut toolbox = Toolbox::bash_and_mcps(cwd.clone(), Some(scratch.clone()))?;
    // Credentials live in the base dir, not the session dir: an authorization
    // belongs to the operator and the server, not to one session.
    let store = efference::mcp::auth::store::FileStore::new(Path::new(BASE_DIR));
    let unmounted = mcp_mount::mount_all(
        &mut toolbox,
        &loaded.role.mcp_servers,
        &cwd,
        &store,
        &reauth,
    )
    .await?;
    let unmounted_section = efference::mcp::unmounted_section(&unmounted);

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
    let system = efference::env::system_prompt(
        model,
        context_block.as_deref(),
        role_section.as_deref(),
        unmounted_section.as_deref(),
    );
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

    let mut client = Client::new(endpoint.clone(), model, system, temperature)
        .with_model_reasoning(capabilities.supports_reasoning);
    // The output budget follows the model's reported cap (a reasoning model's
    // thinking bills against it; the 4096 default starves a long think). No
    // report → the default stands.
    if let Some(cap) = capabilities.max_output_tokens {
        client = client.with_max_output(cap);
    }
    let client = client;

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
        bind,
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

/// Every mounted tool name, dotted — what the tester prints when `--tool` names
/// something no provider owns.
fn available_tools(toolbox: &Toolbox) -> Vec<String> {
    toolbox
        .definitions()
        .iter()
        .filter_map(|d| d.get("name").and_then(|n| n.as_str()))
        .map(efference::mcp::display_name)
        .collect()
}

/// Dev tool: run the spotter on one move and print the verdict
/// (docs/permissions.md). No daemon — it builds a config and a [`ToolCall`] and calls
/// the exact `assess` path a real gate would, so what it prints is what the loop would
/// see. For iterating on the persona and config.
///
/// Two modes. A **bash** move needs nothing but the flags. A **`--tool`** move is
/// judged against a real tool definition, so it first builds the toolbox and mounts the
/// servers `--config` declares — the brief the spotter reads is then the same one a
/// session's gate would render, annotations and all, rather than a hand-written stand-in
/// that could drift from what the server actually advertises.
///
/// [`ToolCall`]: efference::log::ToolCall
#[allow(clippy::too_many_arguments)] // a dev tester: every knob is one flag
pub(crate) async fn spotter_check(
    scope: Option<String>,
    watch_for: Vec<String>,
    paths: Vec<String>,
    model: &str,
    probe: Option<String>,
    tool: Option<String>,
    input: String,
    config: Option<PathBuf>,
    show_move: bool,
    no_brief: bool,
    command: String,
) -> CliResult {
    // `--config` supplies the mission and the server declarations; the individual flags
    // override it, so a config can be borrowed for its servers while the perimeter under
    // test is varied from the command line.
    let base = match config.as_deref() {
        Some(path) => match efference::config::load(Some(path)) {
            Ok(loaded) => loaded.role,
            Err(e) => {
                e.eprint();
                return Err("could not load config".into());
            }
        },
        None => efference::config::Config::default(),
    };
    let role = efference::config::Config {
        scope: scope.or(base.scope),
        watch_for: if watch_for.is_empty() {
            base.watch_for
        } else {
            watch_for
        },
        paths: if paths.is_empty() {
            base.paths
        } else {
            paths.iter().map(|s| parse_path_grant(s)).collect()
        },
        mcp_servers: base.mcp_servers,
        ..Default::default()
    };

    // A structured move needs the mounted table to have a brief at all, so the toolbox is
    // built only in that mode — a bash check keeps costing nothing and spawning nothing.
    let toolbox = match &tool {
        Some(_) => {
            let cwd = std::env::current_dir()?;
            let mut toolbox = Toolbox::bash_and_mcps(cwd.clone(), None)?;
            // Unlike `serve`, a mount failure is fatal here: the whole point is judging
            // against that server's real definition, and a skipped mount would silently
            // downgrade the check to a name with no brief.
            for (name, server) in &role.mcp_servers {
                toolbox
                    // No stored credentials and no dance: a spotter check must
                    // never open a browser (docs/mcp-auth.md). A server needing
                    // authorization fails here, pointing at `efference serve`.
                    .mount_connected(name, &server.transport, &cwd, None)
                    .await
                    .map_err(|e| format!("could not mount `{name}`: {e}"))?;
            }
            Some(toolbox)
        }
        None => None,
    };

    let call = match &tool {
        // Either spelling is accepted, resolved by the same rule the config uses, so the
        // tester and a `tools` grant agree on what a dotted name means.
        Some(name) => efference::log::ToolCall {
            id: efference::log::CallId("toolu_spotter_check".into()),
            name: efference::config::wire_tool_name(name.clone()),
            input: serde_json::from_str(&input)
                .map_err(|e| format!("--input is not valid JSON: {e}"))?,
        },
        None => efference::log::ToolCall {
            id: efference::log::CallId("toolu_spotter_check".into()),
            name: efference::bash::NAME.into(),
            input: serde_json::json!({ "command": command }),
        },
    };

    let brief = toolbox.as_ref().and_then(|t| t.brief(&call.name));
    // The lookup runs either way, so `--no-brief` still catches a name nothing mounts —
    // the A/B compares two verdicts on the same real tool, not one on a fictional one.
    let sent = if no_brief { None } else { brief.as_ref() };
    if tool.is_some() && brief.is_none() {
        // Naming a tool nothing mounts is the likely mistake (a typo, or a server the
        // config doesn't declare), and it would otherwise pass silently as a briefless
        // call — the exact thing this mode exists to avoid.
        return Err(format!(
            "no mounted tool named `{}`. Mounted: {}",
            efference::mcp::display_name(&call.name),
            match toolbox.as_ref().map(available_tools) {
                Some(names) if !names.is_empty() => names.join(", "),
                _ => "(none)".into(),
            }
        )
        .into());
    }
    if show_move || tool.is_some() {
        println!(
            "{}\n",
            efference::spotter::preview_move(&call, sent, probe.as_deref())
        );
    }
    // Credentials are resolved last, after the config, the mount, and the brief: a typo'd
    // tool name or an unreachable server should fail on its own terms, and the move
    // preview is useful with no key at all.
    let endpoint = efference::api::Endpoint::from_env()?;
    let spotting = ForkSpot::new(endpoint, model)
        .assess(&role, &call, sent, probe.as_deref())
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
