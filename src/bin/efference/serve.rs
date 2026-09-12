//! `efference host` — bind the control socket and host sessions created over it (docs/control-channel.md).

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
    // The bind's credentials all derive from the stored root (docs/web-bind.md):
    // read it, or mint it if this is the first bind on this machine.
    let root = efference::auth::Root::load(&efference::secrets::Secrets::detect()?)?;
    efference::auth::Root::install(Some(root.clone()));
    Ok(Some(efference::daemon::TcpBind {
        addr,
        tls,
        host,
        root,
        open: flags.open,
    }))
}

/// Every mounted tool name, dotted — what the tester prints when `--tool` names
/// something no provider owns.
#[cfg(feature = "dev")]
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
#[cfg(feature = "dev")]
pub(crate) async fn spotter_check(
    scope: Option<String>,
    watch_for: Vec<String>,
    paths: Vec<String>,
    model: Option<&str>,
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
            paths
                .iter()
                .map(|s| crate::dev::parse_path_grant(s))
                .collect()
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
            // Unlike a session boot, a mount failure is fatal here: the whole point is judging
            // against that server's real definition, and a skipped mount would silently
            // downgrade the check to a name with no brief.
            for (name, server) in &role.mcp_servers {
                toolbox
                    // No stored credentials and no dance: a spotter check must
                    // never open a browser (docs/mcp-auth.md). A server needing
                    // authorization fails here, pointing at a hosted session.
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
        Some(name) => efference::log::ToolCall::new(
            efference::config::wire_tool_name(name.clone()),
            "toolu_spotter_check",
            serde_json::from_str(&input).map_err(|e| format!("--input is not valid JSON: {e}"))?,
        ),
        None => efference::log::ToolCall::new(
            efference::bash::NAME,
            "toolu_spotter_check",
            serde_json::json!({ "command": command }),
        ),
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
    let (model, endpoint) = crate::inference::spotter_endpoint(model)?;
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

/// `efference host`: a hosting process with no sessions until `POST /sessions`.
pub(crate) async fn host(bind_flags: BindFlags) -> CliResult {
    let bind = resolve_bind(bind_flags)?;
    // The socket keeps its relative spelling (Darwin's socket path limit); the
    // base dir is created so the bind has somewhere to land.
    std::fs::create_dir_all(BASE_DIR)?;
    let socket = host_socket();
    let _ = std::fs::remove_file(&socket);
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,efference=info"),
    )
    .try_init();
    let host_cwd = std::env::current_dir()?;
    let host_env = efference::env_table::EnvSnapshot::capture()?;
    let secrets = efference::secrets::Secrets::detect()?;
    let inference = efference::inference::InferenceRegistry::load(&host_cwd, host_env, &secrets)?;
    daemon::serve_host(efference::daemon::Host::new(inference), socket, bind).await?;
    Ok(())
}
