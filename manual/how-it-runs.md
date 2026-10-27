# How efference runs

## What you need to know

efference's agent sessions run inside a **host**, a process on your machine. The terminal
interface and the web client are windows onto sessions in a host; neither one is where
the work happens. Scripts and other agents reach sessions the same way, through the same
interface.
<!-- src: src/daemon/host.rs::serve_host; src/client.rs; src/daemon/handler.rs -->

There are two ways to run one.

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

"Quit" therefore means two things: in standalone mode it ends the session, which you can
resume later; with a host running it only closes the window onto it. The terminal
interface says which: its leaving action reads "quit" in standalone mode and "detach" with
a host.
<!-- src: src/tui/control_strip.rs::strip_live_items -->

A host started in a terminal ends when that terminal does. To keep one running across
logins, run `efference --host` under your system's service manager (launchd on macOS, a
systemd user unit on Linux). A service does not see your shell's environment, so keep
model keys in the keychain rather than in environment variables. A Linux server usually
has no keychain a service can open; there, name secret commands for efference to keep
them with instead ([Credentials](credentials.md), "Secret commands"). Sessions the host
resumes, or creates for the web client, start with the service's environment too, unless
an `env_hook` names a command that sets up a project's environment
([Configuration](configuration.md)).
<!-- src: absence of service integration in src/; src/bin/efference/serve.rs::host (EnvSnapshot::capture, Secrets::configured); src/secrets/commands.rs; src/daemon/host/preparation.rs::establish_environment; src/env_table/launch.rs::run_hook -->
<!-- TODO: include the launchd plist and systemd unit from assets/guide/host.md once verified end to end. The service examples are host-as-a-service (backlog/inbox/host-as-a-service.md). -->

### What leaves your machine

The efference process itself connects only to:

- the model providers your connections name, for the conversation and for model listings
  (an OpenAI platform connection also reads model details from the ChatGPT backend);
- the MCP servers your projects configure — tools the agent can call — and the sign-in
  servers they name;
- the push service of each browser that turned on notifications in the web client.

It makes no update checks, sends no telemetry, analytics, or crash reports, and does not
contact efference.net, which is a static website. Every connection is listed below.
<!-- src: src/http.rs::client (the one outbound client; callers src/api/, src/mcp/http.rs, src/oauth.rs, src/push.rs); absence: no update, telemetry, or crash-reporting code in src/ or crates in Cargo.lock -->

The programs efference starts are a different matter: the agent's commands, and the
commands your configuration names (local MCP servers, an environment hook, secret
lookups such as `op read`). They run as you, with your network access, and efference does
not sandbox them. What it controls is which of the agent's commands run without asking
you first ([Permissions](permissions.md)).
<!-- src: src/bash.rs; src/daemon/background.rs; src/mcp/stdio.rs; src/env_table/launch.rs::run_hook; src/env_table/resolve.rs::run_command; src/context.rs::run_exec; src/policy.rs -->
<!-- TODO: permissions.md is not written yet. -->

## How this works, precisely

### The control channel

A host listens on a Unix socket: `~/.efference/host.sock` in host mode, and
`~/.efference/islands/<pid>/host.sock` for a standalone host. Every client on the machine
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

### Where a session's record lives

Each session keeps an append-only log in the project it runs in, at
`.efference/<session id>/log.jsonl`, beside a copy of the project's configuration as that
session sees it. Resuming a session reopens its log. [Credentials](credentials.md) covers
what is kept out of it.
<!-- src: src/boot.rs::LOG_NAME, CONFIG_NAME; src/persist.rs::Log; src/boot/resume.rs -->

When `efference --host` starts, it reopens the sessions that were still open when a host
last stopped, in every project, and leaves them idle: work that was interrupted does not
continue until you message or continue the session. A reopened session may still make
small requests of its own — keeping the provider's cache of the conversation warm, or
reading a notification that arrives.
<!-- src: src/daemon/host/boot_resume.rs::resume_open_sessions, reopen; src/agent.rs (idle loop; pending batch replays on the first operator action); src/recovery.rs; src/daemon/keepalive.rs; src/daemon/metrics.rs::seed_clocks -->

A standalone host reopens nothing when it starts. When you quit or close its terminal, it
archives each of its sessions before it stops, the same as archiving a session yourself, so
no host reopens them later. A session that worked in its own git worktree keeps the worktree
if it holds changes, and the worktree is removed if it holds none. A session in which
nothing was said yet is deleted rather than archived. A standalone host that crashes or is
killed archives nothing: its sessions stay open, and the next `efference --host` reopens
them, or you resume them from `efference`'s menu.
<!-- src: src/daemon/host/island_exit.rs::archive_and_shut_down; src/daemon/island.rs::OwnerWatch; src/daemon/host/boot_resume.rs::resume_open_sessions (is_island); src/daemon/host.rs::Host::end; src/worktree.rs::retire; src/discard.rs -->

### Every outbound connection

efference's own network traffic goes through one HTTP client. It trusts your operating
system's certificate store and sends `User-Agent: efference/<version>` followed by its HTTP
library's name. The built-in providers are reached over HTTPS; a custom endpoint or MCP
server given an `http://` address is reached in clear text, key included. The client does
not read `HTTPS_PROXY` or other proxy variables, so a network that only allows traffic
through a proxy blocks it.
<!-- src: src/http.rs::client, user_agent; src/setup/flow.rs::describe_endpoint (http:// accepted); src/mcp/http.rs; absence of proxy handling in src/ -->

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
<!-- src: src/api/provider/http.rs::messages_url, models_url, capabilities_url; src/api/provider/fetch.rs::fetch_capabilities, fetch_generation_cost; src/catalog.rs::CATALOG_TTL; src/daemon/host/catalog.rs::entry; src/api/provider/codex.rs::catalog, capabilities; src/daemon/runtime.rs (cost tracking for an OpenRouter main client); src/daemon/metrics.rs::price_request; src/inference/chatgpt.rs::ISSUER, refresh; src/mcp/http.rs; src/mcp/auth.rs::authorize; src/mcp/auth/credentials.rs::Credentials::renew; src/oauth/; src/push.rs::send_one; src/daemon/push.rs -->

Requests on an OpenRouter connection carry attribution headers naming efference,
including `HTTP-Referer: https://efference.net`; requests to the ChatGPT backend carry
`originator: efference`.
<!-- src: src/api/provider/http.rs::Provider::attribution -->

efference.net appears in the code once more: as the identifier efference presents when
signing in to an MCP server whose authorization server supports client metadata
documents. That authorization server may fetch
`https://efference.net/oauth/client-v1.json`; efference itself does not.
<!-- src: src/mcp/auth.rs::CLIENT_ID_METADATA_DOCUMENT, client_id_for -->

A `[host] bind` given as a name rather than an address is looked up in DNS when the host
starts.
<!-- src: src/bin/efference/serve.rs::resolve_bind (to_socket_addrs) -->
