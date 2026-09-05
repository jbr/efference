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
pub(crate) fn resolve_bind(
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

/// Create the session dir, refusing a name that already has one (docs/control-channel.md).
///
/// A session does not outlive its process: `Log::create` opens the log for append with an
/// empty in-memory `entries`, so a second run over the same file restarts `seq` at 0 and the
/// file ends up with a duplicated seq space — which every offline reader (`view`, `wire`, the
/// TUI fold) reads as a single sequence. Refusing an occupied dir is what keeps that
/// unreachable until resumption is designed; when it is, this guard changes with it rather
/// than gaining exceptions.
///
/// `create_dir` rather than `create_dir_all` on the leaf: the leaf creation is the exclusive
/// one, so two `serve`s racing the same name cannot both pass the check. The parent
/// (`.efference/`) is still created recursively — it is shared by every session.
pub(crate) fn mint_session_dir(session_dir: &Path) -> Result<(), CliError> {
    if let Some(parent) = session_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::create_dir(session_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "session dir {} already exists — `serve` mints a new session and does not resume one. \
             Pick another --name; the existing session's log is intact at {}.",
            session_dir.display(),
            session_dir.join(LOG_NAME).display()
        )
        .into()),
        Err(e) => Err(format!(
            "could not create session dir {}: {e}",
            session_dir.display()
        )
        .into()),
    }
}

/// Resolve every value the `[env]` table names, failing the boot on the first
/// unresolvable reference (docs/env.md). Loud rather than silent: a reference
/// that does not resolve would otherwise surface much later as a tool's opaque
/// auth error, with nothing connecting the two. The error names the *reference*,
/// which is a pointer and safe to print.
fn resolve_env_table(
    resolver: &efference::env_table::Resolver,
    table: &std::collections::BTreeMap<String, efference::env_table::EnvEntry>,
) -> Result<Vec<efference::env_table::ResolvedValue>, Box<dyn std::error::Error + Send + Sync>> {
    let (values, errors) = resolver.resolve_table(table);
    for error in &errors {
        eprintln!("error: [env] {error}");
    }
    if !errors.is_empty() {
        return Err("could not resolve [env] references".into());
    }
    Ok(values)
}

/// [`resolve_env_table`] as a name → value map, for the credential lookup that
/// runs before the session's policy exists.
fn resolve_env_values(
    resolver: &efference::env_table::Resolver,
    table: &std::collections::BTreeMap<String, efference::env_table::EnvEntry>,
) -> Result<std::collections::BTreeMap<String, String>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(resolve_env_table(resolver, table)?
        .into_iter()
        .map(|v| (v.name, v.value))
        .collect())
}

#[allow(clippy::too_many_arguments)] // one flag, one parameter; the CLI is the arity
pub(crate) async fn serve(
    session_dir: PathBuf,
    model: &str,
    spotter_model: &str,
    config: Option<PathBuf>,
    temperature: Option<u8>,
    effort: Option<String>,
    upstream: Vec<String>,
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

    // The origin config is read here — before the session dir is minted — because
    // its `[env]` table may supply the harness's own credential as a reference
    // (docs/env.md), and because a malformed template or an unresolvable
    // reference should fail before anything is minted or bound.
    let origin = match efference::config::load(config.as_deref()) {
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
    let resolver = std::sync::Arc::new(efference::env_table::Resolver::default());
    let injected = resolve_env_values(&resolver, &origin.role.env)?;

    // Provider resolution (docs/transport.md): the declared EFFERENCE_PROVIDER
    // (default anthropic) implies the messages URL and auth shape; the secret arrives
    // alone via EFFERENCE_API_KEY (or the ANTHROPIC_API_KEY fallback) so it can be a
    // whole-value `op://…` reference — either resolved from `[env]` above, or
    // substituted by `op run` into the process environment.
    let mut endpoint = efference::api::Endpoint::from_env_with(&injected)?;

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
    // The `--upstream` pin narrows which endpoints answer the question: a pinned
    // session's ceilings are the pinned endpoints', not the aggregate across every
    // upstream serving the slug (docs/transport.md).
    let capabilities = match endpoint.fetch_capabilities(model, &upstream).await {
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
    // the dir is the unique unit (docs/permissions.md). Minting it is also the guard against
    // re-serving a name: an existing dir is refused here, before the log is opened and before
    // the socket is unlinked, so neither the sequential nor the concurrent corruption path is
    // reachable (docs/control-channel.md).
    mint_session_dir(&session_dir)?;

    // Snapshot the origin config into the session's own `config.toml` *once* (a fresh session). The
    // origin (`--config`/the `efference.toml` probe) is a template consumed at boot; thereafter the
    // per-session snapshot is the live, editable, watched policy — so two sessions sharing an origin
    // diverge freely, and the session can `cat` its config to see its perimeter and edit it to
    // request changes. The `else` branch (a snapshot that already exists) is unreachable while
    // `mint_session_dir` refuses an existing dir; it is what a resumption design would re-enter.
    let session_config = session_dir.join(CONFIG_NAME);
    if !session_config.exists() {
        // The origin was validated above (fail fast on a malformed template, before binding
        // anything); copy it verbatim (comments intact) — or scaffold an empty config when
        // there's none.
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

    // The session's effort level (docs/transport.md): the flag if given, else the
    // session config's `[transport] effort`, else nothing on the wire at all. Boot-time
    // and frozen — no runtime toggle, because a mid-session change would move the cached
    // prefix. The one cross-axis coupling is checked here rather than forwarded as the
    // provider's 400: `xhigh`/`max` need thinking, which either `EFFERENCE_THINKING=off`
    // or a model whose turns fall through to explicit `disabled` takes away — the
    // capability lookup above is what makes the effective answer computable here.
    let effort = effort.or_else(|| loaded.role.transport.effort.clone());
    // Validate before the coupling check: the metadata's list is the level's
    // only defense, so an unsupported spelling is named first (docs/transport.md).
    if let Some(effort) = &effort
        && endpoint.provider.validates_effort_levels()
        && let Err(why) = capabilities.check_effort(effort, model)
    {
        return Err(why.into());
    }
    if let Err(why) = efference::api::check_effort_thinking(
        effort.as_deref(),
        efference::api::auto_turns_disable_thinking(model, capabilities.supports_reasoning),
    ) {
        return Err(why.into());
    }
    match (&effort, &capabilities.default_effort) {
        (Some(effort), _) => eprintln!("effort: {effort}"),
        (None, Some(default)) => {
            eprintln!("effort: unset · provider default {default}");
        }
        (None, None) => {}
    }

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

    // The `[env]` table (docs/env.md), resolved once against this process's
    // environment: the child-env policy every spawn site applies (strip, and the
    // values the table names), and the session's masker. An unresolvable
    // precedence tie is a startup config error; a value under the maskability
    // floor is a warning naming the variable only. The session snapshot is a copy
    // of the origin, so its references are already in the resolver's cache — no
    // second vault prompt.
    let session_values = resolve_env_table(&resolver, &loaded.role.env)?;
    let env_report = efference::env_table::EnvPolicy::build_with_values(
        &loaded.role.env,
        &session_values,
        resolver.clone(),
    );
    for name in &env_report.forced_mask {
        eprintln!(
            "[env: `{name}` resolves from a command, so its value is masked despite \
             `mask = false` — a value the harness fetched for the session is known to be \
             a secret]"
        );
    }
    for conflict in &env_report.conflicts {
        eprintln!("error: [env] {conflict}");
    }
    if !env_report.conflicts.is_empty() {
        return Err("ambiguous [env] table".into());
    }
    for name in &env_report.below_floor {
        eprintln!(
            "[env: `{name}` is set to fewer than {} characters and will not be masked — \
             too short a needle would shred unrelated output]",
            efference::env_table::MASKABILITY_FLOOR
        );
    }
    let env_policy = env_report.policy;

    let log_path = session_dir.join(LOG_NAME);
    let mut log = Log::create(&log_path)?;
    // In force before the first append — the boot `Preamble` below is the first
    // entry, and its `[context]` exec snapshot is exactly the kind of env capture
    // masking exists for.
    log.set_masker(env_policy.masker());
    eprintln!("session dir: {}", session_dir.display());
    eprintln!("logging to {}", log_path.display());
    let socket = session_dir.join(SOCKET_NAME);
    // The dir was just minted, so there is no socket to unlink and no live daemon whose socket
    // this could take away — `mint_session_dir` refused before reaching here if the dir existed.
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
    let mut toolbox = Toolbox::bash_and_mcps(cwd.clone(), Some(scratch.clone()))?
        .with_bash_limits(loaded.role.bash)
        .with_child_env(env_policy.child_env())
        // The image gate (docs/tools.md): what the boot capability lookup said
        // about this model's image input, so an image-producing tool refuses
        // clearly instead of attaching bytes the model cannot read.
        .with_image_input(capabilities.supports_image_input);
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
    let mut resolved = match efference::context::resolve_with_child_env(
        &loaded.role.context,
        &cwd,
        &env_policy.child_env(),
    ) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("error: {e}");
            return Err("could not resolve [context] entries".into());
        }
    };
    // Mask the resolved sections *before* rendering (docs/env.md). The preamble is
    // the one string that both rides the request's `system` field and is
    // snapshotted into the log, so masking it at the log's choke point alone would
    // leave the wire copy unfiltered and the two copies no longer byte-identical —
    // which is the whole point of the `Preamble` entry (docs/context.md). A
    // `[context]` exec that prints the environment is the concrete case.
    env_policy.mask_context(&mut resolved);
    let context_block = resolved.render();
    let system = env_policy.mask(&efference::env::system_prompt(
        model,
        context_block.as_deref(),
        role_section.as_deref(),
        unmounted_section.as_deref(),
    ));
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

    // The upstream pin rides the agent client, which the fork peek shares — so
    // turns and gates route alike. It deliberately does **not** reach the spotter:
    // endpoint slugs are per-model, the spotter runs a different (Haiku-band)
    // model, and an order naming endpoints that do not serve it would fail
    // outright with fallbacks disabled.
    let upstream = efference::api::Upstream::pinned(upstream);
    if let Some(pin) = &upstream {
        eprintln!("upstream: {} (fallbacks disabled)", pin.order.join(", "));
    }
    let mut client = Client::new(endpoint.clone(), model, system, temperature)
        .with_upstream(upstream.clone())
        // `[transport] stream-ceiling` (docs/transport.md): the bound on a stream
        // that stays open without finalizing. It rides the agent client, which
        // the fork peek shares, so both stream surfaces are bounded alike.
        .with_stream_ceiling(loaded.role.transport.stream_ceiling)
        // The effort level rides the agent client, so the turn and the fork peek send
        // one envelope (docs/redaction-fork.md). The spotter's client is built
        // separately and never carries it; the keepalive omits it by construction.
        .with_effort(effort)
        .with_model_reasoning(capabilities.supports_reasoning)
        // Who places cache-write boundaries (docs/transport.md): the Responses
        // request reads this to decide whether to send explicit markers.
        .with_prompt_cache(capabilities.prompt_cache);
    // The output budget follows the model's reported cap (a reasoning model's
    // thinking bills against it; the 4096 default starves a long think). No
    // report → the default stands.
    if let Some(cap) = capabilities.max_output_tokens {
        client = client.with_max_output(cap);
    }
    // The window the cap is reserved against: each request sends the cap
    // shrunk to what the window spares after the prompt (`api/reservation.rs`).
    client = client.with_context_window(capabilities.context_length);
    let client = client;

    // The independent permission rung (docs/permissions.md): a fresh-context spotter
    // on its own Haiku-band client, built from the same endpoint. It re-decides the calls
    // the deterministic fast-path can't clear, before the human gate. It learns the
    // scratch dir so its scope block can alias `$TMPDIR` to the granted path, and this
    // session's cwd so relative grants and relative moves resolve the same way the
    // deterministic rung resolves them. Its own
    // sticky key (`for_spotter`): same endpoint, but a prefix the agent lineage never
    // shares, so pointing both at one model doesn't pin them to one route.
    // The spotter shares the endpoint, so its model must be one the provider serves:
    // the Haiku default is meaningless on OpenAI, and the catalog is the check the
    // main model already passed (docs/transport.md).
    if endpoint.provider.uses_openai_catalog()
        && let Err(e) = efference::api::openai_model_capabilities(spotter_model)
    {
        return Err(format!("--spotter-model {spotter_model:?}: {e}").into());
    }
    let spotter = ForkSpot::new(endpoint.for_spotter(), spotter_model)
        .with_scratch(Some(scratch.clone()))
        .with_cwd(cwd.clone());

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
    let image_placement = client.image_placement();
    let breakpoint_placement = client.breakpoint_placement();
    let result = daemon::serve(
        client,
        toolbox,
        ForkPeek {
            image_placement,
            breakpoint_placement,
        },
        spotter,
        log,
        socket,
        loaded,
        autonomous,
        capabilities,
        // The session's identity topic (docs/control-api.md): the model resolved
        // above and the cwd the toolbox and [context] were built from — the two
        // values a client needs to say which session it is looking at.
        efference::daemon::SessionIdentity {
            model: model.to_string(),
            cwd: cwd.to_string_lossy().into_owned(),
        },
        bind,
        env_policy,
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

    // The tester judges a move as if made from here, so this is the cwd the scope
    // block renders and resolves grants against.
    let cwd = std::env::current_dir()?;

    // A structured move needs the mounted table to have a brief at all, so the toolbox is
    // built only in that mode — a bash check keeps costing nothing and spawning nothing.
    let toolbox = match &tool {
        Some(_) => {
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

    let brief = toolbox
        .as_ref()
        .and_then(|t| efference::agent::brief(t, &call.name));
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
        .with_cwd(cwd)
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
