//! `efference --host` — bind the control socket and host sessions created over it (docs/control-channel.md).

use super::*;

/// The raw `--bind` flag family, laid over the `[host]` table by
/// [`over`](Self::over) and resolved by [`resolve_bind`] into the daemon's
/// [`TcpBind`] — or rejected before anything else boots.
///
/// [`TcpBind`]: efference::daemon::TcpBind
pub(crate) struct BindFlags {
    pub(crate) bind: Option<String>,
    pub(crate) public_url: Option<String>,
    pub(crate) tls_cert: Option<PathBuf>,
    pub(crate) tls_key: Option<PathBuf>,
    pub(crate) open: bool,
}

impl BindFlags {
    /// Each flag given wins over its `[host]` key, one key at a time
    /// (docs/web-bind.md), so `--bind 127.0.0.1:0` keeps a configured name and
    /// `--bind ""` turns the table's bind off.
    pub(crate) fn over(self, table: efference::inference::HostConfig) -> Self {
        Self {
            bind: self.bind.or(table.bind),
            public_url: self.public_url.or(table.public_url),
            tls_cert: self.tls_cert.or(table.tls_cert),
            tls_key: self.tls_key.or(table.tls_key),
            open: self.open,
        }
    }
}

/// Validate the bind flags into a [`TcpBind`] (docs/web-bind.md), enforcing the
/// operator posture at the flag boundary: plaintext only on loopback, TLS wants an
/// explicit public name, and a bind without the machine root, read from `secrets`,
/// is refused rather than silently unauthenticated.
///
/// [`TcpBind`]: efference::daemon::TcpBind
pub(crate) fn resolve_bind(
    flags: BindFlags,
    secrets: &efference::secrets::Secrets,
) -> Result<Option<efference::daemon::TcpBind>, Box<dyn std::error::Error + Send + Sync>> {
    // An empty bind is the explicit "UDS-only" spelling: `--bind ""` overrides a
    // configured `[host] bind` (e.g. for a second concurrent host).
    let Some(addr) = flags.bind.filter(|a| !a.trim().is_empty()) else {
        if flags.open {
            return Err("--open needs a bind: there is no link to open without one".into());
        }
        return Ok(None);
    };
    let addr = loopback_addr(&addr);
    use std::net::ToSocketAddrs;
    let resolved: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| format!("--bind {addr:?} did not resolve: {e}"))?
        .collect();
    let loopback = !resolved.is_empty() && resolved.iter().all(|a| a.ip().is_loopback());
    let tls = match (flags.tls_cert, flags.tls_key) {
        (Some(cert), Some(key)) => Some(efference::daemon::TlsPem {
            cert: std::fs::read(&cert)
                .map_err(|e| format!("could not read tls-cert {}: {e}", cert.display()))?,
            key: std::fs::read(&key)
                .map_err(|e| format!("could not read tls-key {}: {e}", key.display()))?,
        }),
        (None, None) => None,
        // Checked after the merge, not by clap: the pair may come half from a
        // flag and half from the table.
        _ => {
            return Err("tls-cert and tls-key go together: set both, or neither".into());
        }
    };
    if tls.is_none() && !loopback {
        return Err(format!(
            "--bind {addr} is not loopback, and a cleartext bind is only accepted on \
             loopback: anyone who can reach this interface can replay a browser's device \
             token and drive the agent's tools. Serve it with --tls-cert/--tls-key, or \
             bind loopback (a bare port, or 127.0.0.1/::1)"
        )
        .into());
    }
    let public_url = match (flags.public_url, &tls) {
        (Some(url), tls) => checked_public_url(&url, tls.is_some())?,
        (None, None) => format!(
            "http://localhost:{}",
            addr.rsplit(':').next().unwrap_or_default()
        ),
        (None, Some(_)) => {
            return Err(
                "a TLS bind needs a public URL (--public-url, or public-url under [host]) \
                 — https://<the certificate's name>:<port>, the address browsers open"
                    .into(),
            );
        }
    };
    // The bind's credentials all derive from the stored root (docs/web-bind.md):
    // read it, or mint it if this is the first bind on this machine and the store
    // is not the `[secrets]` commands.
    let root = efference::auth::Root::load(secrets)?;
    efference::auth::Root::install(Some(root.clone()));
    Ok(Some(efference::daemon::TcpBind {
        addr,
        tls,
        public_url,
        root,
        open: flags.open,
    }))
}

/// A bare port is the loopback address with that port: `--bind 8080` and
/// `--bind 127.0.0.1:8080` are the same listener, so the short spelling never
/// widens exposure past the loopback-only rule below. Anything with a colon,
/// or a name, is left for `to_socket_addrs` to resolve as written.
pub(crate) fn loopback_addr(addr: &str) -> String {
    match addr.parse::<u16>() {
        Ok(port) => format!("127.0.0.1:{port}"),
        Err(_) => addr.to_string(),
    }
}

/// A public URL is an origin — `http(s)://name[:port]`, no path — and one the host
/// serves over TLS itself is `https`. A plaintext listener may sit behind an `https`
/// one (a proxy terminating TLS), so the reverse is allowed.
pub(crate) fn checked_public_url(url: &str, terminates_tls: bool) -> Result<String, CliError> {
    let url = url.trim().trim_end_matches('/');
    let Some((scheme, authority)) = url.split_once("://") else {
        return Err(
            format!("public-url {url:?} is not a URL: expected https://name[:port]").into(),
        );
    };
    if scheme != "https" && scheme != "http" {
        return Err(format!("public-url {url:?} must be http or https").into());
    }
    if authority.is_empty() || authority.contains('/') {
        return Err(
            format!("public-url {url:?} is an origin, scheme://name[:port], with no path").into(),
        );
    }
    if terminates_tls && scheme != "https" {
        return Err(format!(
            "public-url {url:?} is http, but this host terminates TLS itself: browsers reach \
             it at https"
        )
        .into());
    }
    Ok(url.to_ascii_lowercase())
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
    // Credentials are resolved after the config, the mount, and the brief, so a typo'd
    // tool name or an unreachable server fails on its own terms rather than behind a
    // missing key. They come before the preview because what gets previewed depends on
    // which rung the model is: the prose spotter reads a rendered move block, the
    // decision model reads a JSON state (docs/permissions.md).
    let (model, endpoint) = crate::inference::spotter_endpoint(model)?;
    let kind = endpoint.spotter_kind(&model).await?;
    let thinking = endpoint.spotter_thinking(&model, kind).await?;
    // The tester has one directory, so it stands as both.
    let dirs = efference::policy::Directories::at(&cwd);
    let spotter =
        MountedSpotter::mount(endpoint, &model, kind, None, dirs.clone()).with_thinking(thinking);
    if show_move || tool.is_some() {
        let preview = match &spotter {
            MountedSpotter::Jev(_) => efference::spotter::jev::preview_state(
                &role,
                &call,
                sent,
                probe.as_deref(),
                Some(&dirs),
                None,
            ),
            MountedSpotter::Fork(_) => {
                efference::spotter::preview_move(&role, &call, sent, probe.as_deref(), &dirs)
            }
        };
        println!("{preview}\n");
    }
    let spotting = spotter
        .assess(&role, &call, sent, &[], probe.as_deref(), &[])
        .await;
    println!("stance: {:?}", spotting.read.stance);
    println!("reason: {}", spotting.read.reason);
    // The decision rung's stance is a fold over per-axis probabilities
    // (docs/permissions.md), and the synthesized reason names only the deciding
    // one. Iterating on a question's wording means reading all of them, so the
    // tester prints the whole vector.
    if let Some(axes) = &spotting.axes {
        let line = axes.summary();
        println!("axes:   {}", line.join(" · "));
    }
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

/// `efference --host`: a hosting process with no sessions until `POST /sessions`.
/// Binds the user's host socket, `~/.efference/host.sock`, which every project's
/// clients look for after their own directory; `socket` (`--socket`)
/// binds elsewhere — `.efference/host.sock` for a project-local host
/// (docs/control-channel.md).
///
/// `island` binds the host's life to the process that launched it: stdin is the
/// ownership connection, watched from before anything else initializes
/// ([`efference::daemon::island`]). When it closes, the island archives its
/// sessions and exits.
pub(crate) async fn host(
    bind_flags: BindFlags,
    socket: Option<PathBuf>,
    island: bool,
) -> CliResult {
    let socket = match socket {
        Some(socket) => socket,
        None => user_host_socket().ok_or("no HOME to place ~/.efference/host.sock under")?,
    };
    let home = std::env::var_os("HOME").ok_or("no HOME to read ~/.efference/config.toml under")?;
    // An island takes no bind from the table (docs/operating.md): the machine's
    // bind belongs to the machine's host, and two islands would contend for
    // it. `--bind` still reaches one, since the operator named that address for
    // this launch alone.
    let bind_flags = if island {
        bind_flags
    } else {
        bind_flags.over(efference::inference::HostConfig::load(Path::new(&home))?)
    };
    let owner = island.then(|| {
        let socket = socket.clone();
        efference::daemon::island::OwnerWatch::spawn(std::io::stdin(), move || {
            // No sessions exist yet, so there is nothing to wind down.
            efference::daemon::island::remove_island_files(&socket);
            std::process::exit(0);
        })
    });
    // Shared rather than borrowed: a keychain-backed connection keeps the store
    // to re-read its credential from when one expires (docs/transport.md). Every
    // host reads `[secrets]`, an island included: it names where this user's
    // secrets are, not the machine's bind.
    let secrets = std::sync::Arc::new(efference::secrets::Secrets::configured(Path::new(&home))?);
    let bind = resolve_bind(bind_flags, &secrets)?;
    if let Some(dir) = socket.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&socket);
    // Output discipline: with RUST_LOG unset the host announces session
    // lifecycle (created, resumed, archived, ended — `info`) and transport and
    // operational failures (`warn`, `error`), and nothing else. Per-session
    // detail — the boot report, turn-loop chatter, keepalive, model prose —
    // logs at `debug`; `RUST_LOG=efference=debug` brings it back. No direct
    // prints outside the interactive MCP authorization prompt.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,efference=info"),
    )
    .try_init();
    let host_cwd = std::env::current_dir()?;
    let host_env = efference::env_table::EnvSnapshot::capture()?;
    // Cloned before the registry consumes it: the host needs the same snapshot as
    // the baseline a session launched without one of its own inherits
    // (docs/env.md).
    let launch_baseline = host_env.clone();
    let inference = efference::inference::InferenceRegistry::load(&host_cwd, host_env, &secrets)?;
    refuse_broken_connections(&inference)?;
    for (name, auth) in inference.renewing() {
        log::info!("{name}: renewing its credential ahead of expiry while the host runs");
        tokio::spawn(auth.keep_renewed());
    }
    let host = efference::daemon::Host::new(inference, launch_baseline);
    // Adopted first: an island archives its sessions when its owner goes, and so
    // reopens none at start (docs/resumption.md).
    if let Some(owner) = &owner {
        owner.adopt(&host);
    }
    // The sessions the last host left open, reopened in the background while this one
    // serves; on an island, nothing (docs/resumption.md).
    tokio::spawn({
        let host = std::sync::Arc::clone(&host);
        async move {
            host.resume_open_sessions().await;
        }
    });
    // A host in the foreground titles its terminal; an island's stdout is its log
    // file, so it never does.
    let titling = std::io::IsTerminal::is_terminal(&std::io::stdout())
        .then(|| tokio::spawn(std::sync::Arc::clone(&host).title_terminal(std::io::stdout())));
    let served = daemon::serve_host(host, socket.clone(), bind).await;
    // The shutdown that ended the serve ends the titling, which puts back the
    // terminal title it found.
    if let Some(titling) = titling {
        let _ = titling.await;
    }
    served?;
    if island {
        efference::daemon::island::remove_island_files(&socket);
    }
    Ok(())
}

/// A host does not start half-working (docs/operating.md). A connection whose
/// credential will not load is reported here, at launch, rather than as a
/// failure on every session that happens to touch that provider — which hides
/// the problem at exactly the moment someone could fix it.
///
/// The remedy is named, not performed: this host has nobody to ask. A host
/// launched by a TUI for one session can offer the repair inline once that
/// exists (backlog/inbox/host-process.md).
fn refuse_broken_connections(
    inference: &efference::inference::InferenceRegistry,
) -> Result<(), efference::api::Error> {
    let broken = inference.unavailable();
    if broken.is_empty() {
        return Ok(());
    }
    let mut message = String::from("these configured connections cannot authenticate:\n");
    for (name, why) in &broken {
        message.push_str(&format!("  {name}: {why}\n"));
    }
    message.push_str("run `efference setup` to repair one, or to disable it and start without it");
    Err(message.into())
}
