# Changelog

All notable changes to efference. Format follows [keep-a-changelog](https://keepachangelog.com).

## [Unreleased]

## [2026.11.0] - 2026-11-25

This release focuses on making day-to-day work in efference smoother and quieter:
richer composer input in the web client, more accurate permission review with fewer
unnecessary stops, faster tool-output triage, and MCP resource support.

In the web composer, you can now reference files, directories, and other sessions
inline with path and `@` completion, and paste, drop, or pick files and images right
into a message — referencing a path automatically grants the session access when you
send. Behind the scenes, both permission spotters now review calls with the full
session conversation and managed-worktree context, cutting down false escalations,
while result triage uses lower reasoning effort without breaking prompt caching.
Rounding out the release are MCP resource tools, agent model discovery, installed
web-app shortcuts, shared worktree lifecycles across hand-offs, proxy support, and
provider compatibility fixes for Gemini, Claude, OpenRouter, and HTTP/3 endpoints.

Starting with this release, efference uses [Calendar Versioning (CalVer)](https://calver.org/)
— `YYYY.M.N` (year, month, and zero-based release number within that month) — rather than
semantic versioning. Semver's major/minor/patch compatibility levels are designed for
libraries, not a standalone binary where there is just one size of release. Durable files
on disk (configuration and session logs) keep a migration or compatibility path across
upgrades (with published deprecation windows if a format is ever retired), while running
components are only guaranteed to be compatible within the same version — restart the
host after upgrading rather than mixing a client from one release with a host from another.

### Added

- **Inline file, folder, and session references:** In the web composer, complete host
  paths and `@session` handles into inline chips (`Tab` or `Enter` opens a folder or
  inserts a reference without sending) and paste, drop, or pick files and images
  directly into your message. Referencing a file or directory grants the session read
  access — or read and write, when toggled on the chip — once the message is sent.
  Sent messages display reference chips and inline images with a lightbox, and the TUI
  preserves references when recalling a message rather than flattening them.
- **MCP resources:** Declared MCP servers that expose resources automatically get a
  `resources__<server>` tool so your agent can list and read them. Controlled in
  `[tools]` via `"resources.<server>"`.
- **Agent model discovery:** Your agent and external MCP clients can read
  `efference://models` to discover available model slugs, capabilities, and known
  prices, with filtering and pagination.
- **Installed web-app shortcuts and session creation:** Cycle session tabs (including
  sessions in the overflow menu) with platform keyboard shortcuts, start another
  session in the same project from the `+` button on the tab bar, or press `Cmd+N` to
  open a new session from anywhere in the installed web app.
- **Host-upgrade reload banner:** Open web tabs detect when the host is running a
  newer build and offer a reload instead of continuing with stale client assets.
- **HTTP proxy support:** Outbound HTTP requests honor `http_proxy`, `https_proxy`,
  `all_proxy`, and `no_proxy` (and their uppercase equivalents, with lowercase taking
  precedence).
- **Automatic suffixing for taken session names:** Requesting a session name that is
  already in use picks the next free suffix (`foo-1`, `foo-2`, …) instead of failing.
- **`spotter-notes` in `efference.toml`:** Replaces `watch-for` (which is still
  accepted) so you can give the permission spotter both cautions to look out for and
  routine facts about your project.

### Changed

- **More accurate permission review:** Both permission spotters now evaluate tool
  calls with the full session conversation (attributed by operator, supervisor, and
  agent), unclipped tool schemas, supervision relationships, and managed-worktree path
  rules. The decision spotter (`jev`, now defaulting to `jev-1.14.0`) weighs reasons to
  allow against concrete hazards, and the prose spotter retries once if a reasoning
  model hits its output cap or finishes without a verdict instead of escalating
  straight to you.
- **Quieter multi-agent archiving:** Archiving a session that an agent supervises now
  clears without a permission spotter request.
- **Faster, cheaper result triage:** Tool-result triage uses lower reasoning effort —
  keeping the agent's prompt cache intact when sharing the agent's model (falling back
  automatically if a provider splits caches by effort), and disabling thinking or
  using `low` effort on a separate triage model.
- **Shared worktree occupancy:** Archiving a session no longer deletes a managed
  worktree that another session (such as a hand-off successor) is still using. The
  worktree is retired only when its last occupant is archived, and resuming a former
  occupant restores it at its original path.
- **TUI approval gates and artifacts:** Approval gates in the TUI now show who
  escalated a call and their note, matching the web client. Artifact fences in the TUI
  note that they render in the web client instead of linking to broken loopback pages.
- **And more:** `efference --version` includes the git revision and build profile;
  interactive setup links to the Credentials manual beside API key choices; and
  `efference control settle --help` notes that omitting `--agent` targets the sole
  live session in the current directory.

### Fixed

- **Provider compatibility:**
  - Long inference requests to HTTP/3 providers (such as Google) no longer drop after
    ~30 seconds of silence while waiting for a response.
  - Parallel tool calls from Gemini's OpenAI-compatible endpoint (streamed without
    indices) now stay separate instead of merging into malformed arguments.
  - Newer Claude models that reject turning thinking off retry once with adaptive
    thinking at low effort, including for permission spotter calls.
  - OpenRouter main turns cap requested output tokens at 65,536 to avoid unnecessary
    credit refusals.
  - Removed false-positive truncation errors caused by providers' prompt-token
    accounting, and documented Ollama context-window configuration in the manual.
- **HTTP MCP server mounts:** Fixed intermittent `connection aborted` failures over
  HTTP/2 SSE connections; mount errors now name the pending JSON-RPC method.
- **Web client reconnection and polish:**
  - Stale connections are detected on window focus, page restore, and notification
    taps so pending questions appear without waiting for a timeout.
  - Installed web windows show the app name before the session title, while regular
    browser tabs show it after.
  - Typing a `~` path in the new-session form selects the matching saved location
    instead of showing a duplicate.
- **Worktree and directory handling:**
  - Failed or cancelled worktree creation no longer leaves behind broken checkouts
    that interfere with later sessions.
  - Resuming a session from a symlinked path or alternate macOS case spelling
    (`/users/…` vs. `/Users/…`) now works reliably.
  - Recent locations no longer list a directory twice after it gains an
    `efference.toml`.
- **Host, loop, and TUI reliability:**
  - Interrupting a turn immediately cancels an active retry wait for both the main
    request and result triage.
  - Failed cache keepalives wait a full ping interval before retrying, and streamed
    keepalives accept accounted input usage when the output cap cuts off unused tool
    arguments.
  - Partially written session-log entries are rolled back before the next append is
    accepted.
  - `efference --host` refuses to start when another host is already answering its
    socket, and exits with an error if the web bind fails instead of silently serving
    the Unix socket alone.
  - The TUI shows a compact upstream error indicator even when a failure has no HTTP
    status.

## [0.1.1] - 2026-11-01

- The source is public at [github.com/jbr/efference](https://github.com/jbr/efference), under MIT OR Apache-2.0.
- Release archives have a GitHub build-provenance attestation, checked with `gh attestation verify`.
- Fixed the TUI's policy drawer, which never showed pending config proposals.
- Fixed the web client labelling a process with no pid as "pid null".
- Fixed an archived session leaving the session list before its archive was recorded; an archive request now returns once the session and the agents it ends have left.
- Fixed an interrupt pressed just before a model request started taking effect only after that request finished.
- Fixed waiting for a session to settle: it could answer while a queued turn was starting, or wait out its timeout when an agent's message was delivered to a paused session.
- Fixed project setup writing a `.gitignore` in a directory that is not a git repository.
- Clients find the host only at `~/.efference/host.sock`; a project's `.efference/host.sock` is no longer consulted, which also stops `efference` in the home directory listing each session twice. A host bound elsewhere is reached with `--host-socket`.
- A session directory may be typed as `~` or `~/…`, read as the host's home directory.
- The new-session forms offer "Start in its own worktree" only in a directory that can start one: inside a git work tree whose `HEAD` resolves.
- With one connection, the model picker opens on that connection's models instead of a one-row list of connections.
- Fixed the web client's composer input keeping the previous theme's colour after the system switched between light and dark, and not showing its focus colour in Lilac.

## [0.1.0] - 2026-10-27

- Reworked the manual around first use and reader tasks, with descriptive navigation and technical limits kept alongside the choices they affect.
