# How efference runs

You can sit with an agent in the terminal, leave it working while you’re away, or come
back to it from a browser. The difference is how you run the **host**, the process on your
machine that holds your sessions. The terminal and web clients are windows onto those
sessions; scripts and other agents use the same interface.
<!-- src: src/daemon/host.rs::serve_host; src/client.rs; src/daemon/handler.rs -->

## Choose when sessions keep running

Start in standalone mode for work you’ll stay with. Use host mode when you want sessions
to outlive the terminal interface or be available in the web client.

- **Standalone.** With no host running, `efference` starts one for this terminal alone.
  When you quit the terminal interface, or close the terminal, that host archives its
  sessions and stops, and any work in progress stops with them. Each session's record
  stays in the project, and `efference` there offers to resume it under "resume an
  archived session…".
  <!-- src: src/bin/efference/island.rs::Island::launch, end; src/daemon/island.rs::OwnerWatch; src/daemon/host/island_exit.rs::archive_and_shut_down; src/bin/efference/picker.rs::resume_on; src/bin/efference/picker/app.rs::RESUME -->
- **Host mode.** `efference --host` starts a host that runs until you stop it. Every project
  on the machine uses it. Quitting the terminal interface detaches from a session, which
  keeps working, and you can attach again later. This is the mode that can serve the web
  client ([Remote access](remote-access.md)).
  <!-- src: src/bin/efference/serve.rs::host; src/bin/efference/cli.rs::user_host_socket, host_candidates -->

You don’t have to remember which mode you’re in when you leave. The terminal interface
labels the action **quit** when leaving ends the session, and **detach** when the session
will keep running in a host.
<!-- src: src/tui/control_strip.rs::strip_live_items -->

### Keep a host running across logins

A host started in a terminal ends when that terminal does. To keep it available across
logins, run `efference --host` under your system’s service manager: launchd on macOS, or a
systemd user unit on Linux.

A service doesn’t inherit your shell’s environment. For model keys, use the keychain, or
[secret commands](credentials.md#secret-commands) on a Linux server without an accessible
keychain. For project tools and variables, an
[`env_hook`](configuration.md#give-sessions-your-project-environment) can prepare the
environment for sessions the host resumes or creates for the web client.
<!-- src: absence of service integration in src/; src/bin/efference/serve.rs::host (EnvSnapshot::capture, Secrets::configured); src/platform/secrets/commands.rs; src/daemon/host/preparation.rs::establish_environment; src/config/env_table/launch.rs::run_hook -->
<!-- TODO: include the launchd plist and systemd unit from assets/guide/host.md once verified end to end. The service examples are host-as-a-service (backlog/inbox/host-as-a-service.md). -->

## Understand what leaves your machine

The efference process itself connects only to:

- the model providers your connections name, for the conversation and for model listings
  (an OpenAI platform connection also reads model details from the ChatGPT backend);
- the MCP servers your projects configure — tools the agent can call — and the sign-in
  servers they name;
- the push service of each browser that turned on notifications in the web client;
- efference.net, to check for a newer version of efference, only if you said yes when
  `efference setup` asked, or when you run `efference update --check`.

There is no telemetry, analytics, or crash reporting. If you want to audit the destinations
and what each receives, see [Every outbound connection](#every-outbound-connection) below.
<!-- src: src/platform/http.rs::client (the one outbound client; callers src/inference/api/, src/tools/mcp/http.rs, src/platform/oauth.rs, src/platform/push.rs, src/platform/update_check.rs); src/bin/efference/serve.rs::host (UpdatesConfig::load gates check_for_updates); absence: no telemetry or crash-reporting code in src/ or crates in Cargo.lock -->

The programs efference starts are a different matter: the agent's commands, and the
commands your configuration names (local MCP servers, an environment hook, secret
lookups such as `op read`). They run as you, with your network access, and efference does
not sandbox them. What it controls is which of the agent's commands run without asking
you first ([Permissions](permissions.md)).
<!-- src: src/tools/bash.rs; src/session/background.rs; src/tools/mcp/stdio.rs; src/config/env_table/launch.rs::run_hook; src/config/env_table/resolve.rs::run_command; src/config/context.rs::run_exec; src/config/policy.rs -->

## Host and session details

### The control channel

A host listens on a Unix socket: `~/.efference/host.sock` in host mode, and
`~/.efference/standalone/<pid>/host.sock` for a standalone host. Every client on the machine
— the terminal interface, `efference control`, `efference mcp` — speaks HTTP to the host
over that socket. `efference` looks for a host at `.efference/host.sock` in the current
directory, then at `~/.efference/host.sock`; it never finds another terminal's standalone
host.
<!-- src: src/bin/efference/cli.rs::user_host_socket, host_candidates; src/daemon/island.rs::island_dir, ISLAND_SOCKET; src/client.rs::Client -->

Connections over the socket are not authenticated. Any program that can open the socket
has full control of every session in the host, including starting one, and — when the
host has a web listener — can obtain a sign-in for it. Treat access to the socket like
access to your shell. The host makes the socket and `~/.efference` owner-only when it
starts, so another account on the machine cannot connect. A project's `.efference/`
directory, which holds its session logs, is owner-only too: efference creates it that way,
and makes an existing one that way the next time a session starts or resumes in it.
<!-- src: src/daemon/handler.rs::Transport::Socket; src/daemon/tcp/routes.rs::enrollment_routes; src/daemon/permissions.rs::tighten_state_dir, tighten_socket, ensure_project_base, tighten_project_base -->

A host can also listen on TCP, for browsers. That listener requires a sign-in for
everything except the web client's own files and the sign-in request itself
([Remote access](remote-access.md)).
<!-- src: src/daemon/tcp/gate.rs::Surface::anonymous, gate -->

### Where your work is saved

Each session keeps an append-only log in the project it runs in, at
`.efference/<session id>/log.jsonl`, beside a copy of the project's configuration as that
session sees it. Resuming a session reopens its log. [Credentials](credentials.md) covers
what is kept out of it.
<!-- src: src/record/session_dir.rs::LOG_NAME; src/boot.rs::CONFIG_NAME; src/record/persist.rs::Log; src/boot/resume.rs -->

### What happens after a restart

Starting `efference --host` reopens sessions that were still open when a host last stopped,
across all projects. They wait idle: interrupted work doesn’t continue until you message
or continue the session. An idle session can still make small requests to keep the
provider’s conversation cache warm or read an incoming notification.
<!-- src: src/daemon/host/boot_resume.rs::resume_open_sessions, reopen; src/agent.rs (idle loop; pending batch replays on the first operator action); src/record/recovery.rs; src/session/keepalive.rs; src/session/metrics.rs::seed_clocks -->

Standalone mode starts fresh rather than reopening sessions automatically. On a normal
quit, it archives its sessions so no host reopens them later. Sessions with nothing said
yet are deleted instead. If a session used a git worktree, the worktree stays if it has
changes and is removed if it has none.

A crash or forced kill is different: no archiving happens. Those sessions remain open for
the next `efference --host` to reopen, or for you to resume from `efference`’s menu.
<!-- src: src/daemon/host/island_exit.rs::archive_and_shut_down; src/daemon/island.rs::OwnerWatch; src/daemon/host/boot_resume.rs::resume_open_sessions (is_island); src/daemon/host.rs::Host::end; src/worktree.rs::retire; src/discard.rs -->

### Every outbound connection

efference's own network traffic goes through one HTTP client. It trusts your operating
system's certificate store and sends `User-Agent: efference/<version>` followed by its HTTP
library's name. The built-in providers are reached over HTTPS; a custom endpoint or MCP
server given an `http://` address is reached in clear text, key included. The client does
not read `HTTPS_PROXY` or other proxy variables, so a network that only allows traffic
through a proxy blocks it.
<!-- src: src/platform/http.rs::client, user_agent; src/setup/flow.rs::describe_endpoint (http:// accepted); src/tools/mcp/http.rs; absence of proxy handling in src/ -->

| Destination | When | Notes |
| --- | --- | --- |
| Each connection's model endpoint | Every model request: the agent's turns, and the triage, permission-check, titling, summarizing, and cache-keepalive requests made on that connection | Carries the conversation and tool results |
| Each connection's model details | Session start and resume, and setup | Carries the connection's key |
| Each connection's model list | When a model picker opens and the stored list is more than a day old; setup | Carries the connection's key |
| `chatgpt.com/backend-api/codex/models` | Session start on an OpenAI platform connection | Reads model details; sends the platform key |
| `openrouter.ai/api/v1/generation` | After a request in a session whose main model is on OpenRouter, when the response did not include its cost | Reads what the request cost |
| `auth.openai.com` | Signing in to a ChatGPT subscription in setup, and renewing that sign-in while the host runs | Not used when the connection reuses the Codex CLI's sign-in |
| Each MCP server a session's configuration names with a URL | Session start and resume, and each call to one of its tools | Carries tool arguments and results |
| The authorization servers an MCP server names | When that server asks you to sign in, and when the sign-in is renewed | The server's own metadata names them |
| The push address each notifying browser registered | When a session needs a decision from you, or finishes a turn | The message is encrypted to that browser; the push service sees where and when it is sent |
| `efference.net/releases/latest.json` and `latest.json.minisig` | When a host starts and about once a day after, if `[updates] check = true`; and each time you run `efference update --check` | Sends nothing beyond the request itself. The answer is believed only when it is signed with efference's release key. To turn the daily check off, set `check = false` under `[updates]` in `~/.efference/config.toml`, or answer no in `efference setup`'s "Update check" entry |
<!-- src: src/platform/update_check.rs::LATEST_URL, UpdateCheck::newest, verify; src/daemon/host/update_check.rs::INTERVAL, RETRY; src/config/user_config/updates.rs::UpdatesConfig; src/setup/updates.rs; src/bin/efference/update.rs -->
<!-- src: src/inference/api/provider/http.rs::messages_url, models_url, capabilities_url; src/inference/api/provider/fetch.rs::fetch_capabilities, fetch_generation_cost; src/inference/catalog.rs::CATALOG_TTL; src/daemon/host_http/catalog.rs::entry; src/inference/api/provider/codex.rs::catalog, capabilities; src/daemon/runtime.rs (cost tracking for an OpenRouter main client); src/session/metrics.rs::price_request; src/inference/chatgpt.rs::ISSUER, refresh; src/tools/mcp/http.rs; src/tools/mcp/auth.rs::authorize; src/tools/mcp/auth/credentials.rs::Credentials::renew; src/platform/oauth/; src/platform/push.rs::send_one; src/session/push.rs -->

Requests on an OpenRouter connection carry attribution headers naming efference,
including `HTTP-Referer: https://efference.net`; requests to the ChatGPT backend carry
`originator: efference`.
<!-- src: src/inference/api/provider/http.rs::Provider::attribution -->

efference.net appears in the code once more: as the identifier efference presents when
signing in to an MCP server whose authorization server supports client metadata
documents. That authorization server may fetch
`https://efference.net/oauth/client-v1.json`; efference itself does not.
<!-- src: src/tools/mcp/auth.rs::CLIENT_ID_METADATA_DOCUMENT, client_id_for -->

A `[host] bind` given as a name rather than an address is looked up in DNS when the host
starts.
<!-- src: src/bin/efference/serve.rs::resolve_bind (to_socket_addrs) -->
