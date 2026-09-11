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
//! The CLI's `serve` builds a request from its flags and calls this; a host's
//! session-creation handler builds the same request from a control-channel body.
//! Boot progress is reported on stderr, which is the CLI's surface; a hosted
//! creation will need that report returned instead (backlog/host-process.md).

use std::path::{Path, PathBuf};

use crate::api::{Client, Error};
use crate::daemon::SessionBoot;
use crate::distill::{ForkPeek, TriageModel};
use crate::persist::Log;
use crate::spotter::ForkSpot;
use crate::tools::Toolbox;

pub mod mcp_mount;

/// The base directory for all session dirs (gitignored). Each session is
/// `.efference/<id>/`, holding its standard-named socket, log, and config
/// (docs/permissions.md).
pub const BASE_DIR: &str = crate::daemon::SESSION_BASE;

pub const LOG_NAME: &str = "log.jsonl";

pub const CONFIG_NAME: &str = "config.toml";

/// Written into a fresh session's `config.toml` when there's no origin to snapshot — a header that
/// teaches the schema, plus the one default that isn't empty: an `Environment at boot` `[context]`
/// exec entry, orientation expressed as config rather than hardcoded (docs/context.md). It's a real,
/// visible line in the session's own file, so a session that `cat`s its config sees it and an
/// operator can edit or delete it (docs/permissions.md).
pub const CONFIG_SCAFFOLD: &str = "\
# Your session's permissions config. Edit it to request changes — edits are reviewed before they
# take effect, and a malformed file is rejected wholesale (fail-closed): if an edit doesn't seem to
# \"take\", check that the file still parses. Unknown keys are rejected. Schema (all keys optional):
#   scope = \"...\"                       # STRING, top-level — what this session is for (NOT a [scope] table)
#   watch-for = [\"...\"]                 # hazards the permission spotter should watch for
#   trust-threshold = 0.5               # where the watch-window/gate cutoffs sit (optional float)
#   tools = [\"read\", \"list\", \"search\"]  # structured tools you may use
#   [commands]                          # bash prefixes → clear/watch/gate/deny (true = clear, false = deny),
#                                       #   e.g. cargo = true, \"git diff\" = true, \"git push\" = \"deny\".
#                                       #   The longest matching prefix governs, so a broad grant can carry
#                                       #   carve-outs: git = true with \"git push\" = \"deny\".
#   [paths]                             # filesystem grants, e.g. \".\" = true or \"/refs\" = { read = true }
#   [context]                           # knowledge the session boots with (docs/context.md), e.g.
#                                       #   \"CLAUDE.md\" = true, Label = { exec = \"...\" } (stdout at boot),
#                                       #   or Label = { builtin = \"guide\" } (the harness's own docs)
#   [env]                               # per-variable env policy (docs/env.md): strip = keep it out of
#                                       #   spawned children, mask = never store its value in the log.
#                                       #   e.g. \"*_KEY\" = { mask = true } or \"CI_*\" = { strip = true }.
#                                       #   *_KEY/*_SECRET/*_TOKEN/*_PASSWORD/*_CREDENTIALS mask by default.
#                                       #   An entry may also name the value: RUST_LOG = { value = \"debug\" }
#                                       #   (literal), SOME_KEY = \"op://Vault/Item/field\" (resolved at boot
#                                       #   and always masked), or { cmd = \"...\" } for any other vault.

# The one command a session with no project config needs cleared: validating the template it
# is about to write (docs/operating.md). Everything else goes through the spotter until granted.
[commands]
\"efference check\" = true

# Orientation as config: this section's stdout is frozen into the session preamble at boot.
[context]
\"Environment at boot\" = { exec = 'echo \"working directory: $PWD\"; echo \"platform: $(uname -sm)\"; echo \"top-level entries:\"; ls' }
# The harness's own usage documents, listed as read-on-demand stubs. This is what makes a
# session with no project config recoverable: it starts knowing how to learn everything
# else, including how to write this file. Delete the line to stop listing them; the
# documents stay readable at their paths either way.
EfferenceGuide = { builtin = \"guide\" }
";

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
    pub model: String,
    pub spotter_model: Option<String>,
    pub triage: Option<TriageModel>,
    pub temperature: Option<u8>,
    pub effort: Option<String>,
    pub upstream: Vec<String>,
    pub autonomous: bool,
    pub reauth: Vec<String>,
    pub keep_scratch: bool,
}

/// Prepare a session: everything from the origin config to the provider client,
/// in the order the boot report prints it. Fails before minting anything when the
/// config, a reference, or the provider is unusable; fails after minting when a
/// later step does, leaving the minted dir for the operator.
pub async fn prepare(request: SessionRequest) -> Result<SessionBoot<ForkSpot>, Error> {
    let SessionRequest {
        cwd,
        base_env,
        session_dir,
        config,
        model,
        spotter_model,
        triage,
        temperature,
        effort,
        upstream,
        autonomous,
        reauth,
        keep_scratch,
    } = request;
    assert!(cwd.is_absolute(), "session cwd must be absolute");
    assert!(session_dir.is_absolute(), "session dir must be absolute");
    let model = model.as_str();
    let spotter_model = spotter_model.as_deref();
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
    let injected = resolve_env_values(&resolver, &origin.role.env)?;

    // Provider resolution (docs/transport.md): the declared EFFERENCE_PROVIDER
    // (default anthropic) implies the messages URL and auth shape; the secret arrives
    // alone via EFFERENCE_API_KEY (or the ANTHROPIC_API_KEY fallback) so it can be a
    // whole-value `op://…` reference — either resolved from `[env]` above, or
    // substituted by `op run` into the process environment.
    let mut endpoint = crate::api::Endpoint::from_snapshot(&cwd, &base_env, &injected)?;

    // The sticky-routing key (docs/transport.md): derived once here, from the session
    // dir, which is fixed for the session's life. It pins the session to one upstream
    // so consolidation — which rewrites the opening messages, and with them the key
    // OpenRouter would otherwise derive by hashing them — can't re-route mid-session.
    // Whether it rides a request is the provider's call; a session that never talks to
    // a provider with sticky routing simply derives a key nothing reads.
    endpoint.session_id = Some(crate::api::sticky_session_id(&session_dir));

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
    // A model that refuses the parameter fails here rather than on the first
    // request, where the rejection would read as a transport error.
    if temperature.is_some() && !capabilities.accepts_temperature {
        return Err(
            format!("--temperature: {model} does not accept a sampling temperature").into(),
        );
    }
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
            crate::config::ConfigSource::Path(p) => {
                std::fs::copy(p, &session_config)?;
                eprintln!(
                    "config: snapshot of {} → {}",
                    p.display(),
                    session_config.display()
                );
            }
            crate::config::ConfigSource::Default => {
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
    let mut loaded = match crate::config::load(Some(&session_config)) {
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
    if let Err(why) = crate::api::check_effort_thinking(
        effort.as_deref(),
        crate::api::auto_turns_disable_thinking(model, capabilities.supports_reasoning),
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
        .insert(BASE_DIR.to_string(), crate::config::Perms::default());
    loaded.role.paths.insert(
        session_config.to_string_lossy().into_owned(),
        crate::config::Perms {
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
        crate::config::Perms {
            read: true,
            write: true,
        },
    );

    // The `[env]` table (docs/env.md), resolved against the caller's captured
    // environment: the child-env policy every spawn site applies (strip, and the
    // values the table names), and the session's masker. An unresolvable
    // precedence tie is a startup config error; a value under the maskability
    // floor is a warning naming the variable only. The session snapshot is a copy
    // of the origin, so its references are already in the resolver's cache — no
    // second vault prompt.
    let session_values = resolve_env_table(&resolver, &loaded.role.env)?;
    let env_report = crate::env_table::EnvPolicy::build_snapshot(
        &loaded.role.env,
        base_env,
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
            crate::env_table::MASKABILITY_FLOOR
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

    // Build the system prompt once, here at startup, and freeze it into the cached
    // prefix — it must stay byte-stable for the whole session or it would invalidate
    // the cache every turn (docs/log-and-projection.md). Everything it folds in is resolved
    // once at boot: the [context] block (files read + exec sections run now, frozen —
    // docs/context.md; orientation rides here as a config exec entry), the role's
    // brief, and the config note.
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
    let store = crate::mcp::auth::store::FileStore::new(&cwd.join(BASE_DIR));
    let unmounted = mcp_mount::mount_all(
        &mut toolbox,
        &loaded.role.mcp_servers,
        &cwd,
        &store,
        &reauth,
    )
    .await?;
    let unmounted_section = crate::mcp::unmounted_section(&unmounted);
    // The frozen toolbox against the dialect's name rule (docs/tools.md): a name the
    // request would reject fails the boot here, by its display name, instead of the
    // first turn failing with a 400 that names nothing.
    let illegal = toolbox.illegal_names(endpoint.provider.dialect());
    if !illegal.is_empty() {
        for problem in &illegal {
            eprintln!("error: tool name {problem}");
        }
        return Err(format!(
            "{} mounted tool name{} the {} dialect rejects",
            illegal.len(),
            if illegal.len() == 1 { "" } else { "s" },
            endpoint.provider.dialect()
        )
        .into());
    }

    let role_section = loaded.role.preamble_section();
    // The [context] preamble builder (docs/context.md): resolve the operator's listed
    // files from cwd and fold the rendered block into the frozen prefix. Fail closed — configured
    // context that can't be read aborts startup rather than silently vanishing.
    let mut resolved = match crate::context::resolve_with_child_env(
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
    let system = env_policy.mask(&crate::env::system_prompt(
        model,
        context_block.as_deref(),
        role_section.as_deref(),
        unmounted_section.as_deref(),
    ));
    // Snapshot the rendered boot preamble into the log (docs/context.md): the
    // [context] block folds in file contents that drift on disk, so the preamble is no longer
    // reconstructable from config + current files — this one entry keeps the log the complete
    // record of the wire. `efference dev wire` reads it back instead of rebuilding.
    log.append(
        0,
        crate::log::Payload::Preamble {
            system: system.clone(),
            context: resolved,
        },
    )?;

    // The upstream pin rides the agent client, which the fork peek shares — so
    // turns and gates route alike. It deliberately does **not** reach the spotter:
    // endpoint slugs are per-model, the spotter runs a different (Haiku-band)
    // model, and an order naming endpoints that do not serve it would fail
    // outright with fallbacks disabled.
    let upstream = crate::api::Upstream::pinned(upstream);
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
    // omitted, it is the provider's cheap band; explicit, it still passes the catalog
    // check the main model already passed (docs/transport.md).
    let spotter_model = spotter_model.unwrap_or_else(|| endpoint.provider.default_spotter_model());
    eprintln!("spotter: {spotter_model}");
    if endpoint.provider.uses_openai_catalog()
        && let Err(e) = crate::api::openai_model_capabilities(spotter_model)
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
    // The fork peek's model (docs/redaction-fork.md): the agent's own client unless
    // `--triage-model` names a different model, in which case the peek gets its own
    // client — own identity and persona over the same [context] block, own sticky
    // key, thinking off unless asked for, and none of the agent client's effort,
    // temperature, or upstream pin (endpoint slugs are per-model, as for the
    // spotter). Its context window is looked up like the agent's; nothing yet acts
    // on it being smaller (backlog/distiller-experiment.md).
    let distiller = match &triage {
        Some(t) if t.model != model => {
            let system = env_policy.mask(&crate::env::triage_system_prompt(
                &t.model,
                model,
                context_block.as_deref(),
            ));
            let (peek, caps) = ForkPeek::for_triage_model(
                &endpoint,
                t,
                system,
                loaded.role.transport.stream_ceiling,
            )
            .await
            .map_err(|e| format!("--triage-model: {e}"))?;
            eprintln!(
                "triage: {} · context {} tokens · thinking {} · tool choice {}",
                t.model,
                caps.context_length,
                match (&t.effort, t.thinking) {
                    (Some(effort), _) => format!("on, effort {effort}"),
                    (None, true) => "on".into(),
                    (None, false) => "off".into(),
                },
                if t.auto_choice { "auto" } else { "forced" }
            );
            peek
        }
        Some(_) => {
            eprintln!("triage: {model} (same as the agent; shared peek)");
            ForkPeek::shared()
        }
        None => ForkPeek::shared(),
    };
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
        env_policy,
        keep_scratch,
    })
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
pub fn mint_session_dir(session_dir: &Path) -> Result<(), Error> {
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

/// Mint the per-session scratch dir (docs/tools.md): `<system-temp>/efference-<rand>/`,
/// mkdtemp-style — a random name and create-or-retry (`create_dir` fails if the name
/// exists, so there is no check-then-create race), mode `0700` so it is private on a
/// shared-`/tmp` system (macOS `$TMPDIR` is already per-user). `std::env::temp_dir()`
/// rather than a literal `/tmp`, which on macOS is a symlink and not the real temp dir.
pub fn mint_scratch_dir() -> std::io::Result<PathBuf> {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let base = std::env::temp_dir();
    for _ in 0..16 {
        // `RandomState` seeds from OS entropy — an unpredictable-enough name;
        // create-or-retry (not check-then-create) is what closes the race.
        let n = RandomState::new().build_hasher().finish();
        let candidate = base.join(format!("efference-{n:016x}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "could not mint a scratch dir (16 name collisions)",
    ))
}

/// Resolve every value the `[env]` table names, failing the boot on the first
/// unresolvable reference (docs/env.md). Loud rather than silent: a reference
/// that does not resolve would otherwise surface much later as a tool's opaque
/// auth error, with nothing connecting the two. The error names the *reference*,
/// which is a pointer and safe to print.
fn resolve_env_table(
    resolver: &crate::env_table::Resolver,
    table: &std::collections::BTreeMap<String, crate::env_table::EnvEntry>,
) -> Result<Vec<crate::env_table::ResolvedValue>, Error> {
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
    resolver: &crate::env_table::Resolver,
    table: &std::collections::BTreeMap<String, crate::env_table::EnvEntry>,
) -> Result<std::collections::BTreeMap<String, String>, Error> {
    Ok(resolve_env_table(resolver, table)?
        .into_iter()
        .map(|v| (v.name, v.value))
        .collect())
}

#[cfg(test)]
mod test;
