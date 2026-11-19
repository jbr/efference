# Changelog

All notable changes to efference. Format follows [keep-a-changelog](https://keepachangelog.com);
one line per feature.

## [Unreleased]

### Added

- Sent web messages show filesystem, session, and download chips; attached images appear on their own lines where they were inserted and open in the lightbox.

- The web composer completes host paths and other session handles into inline references: Tab or Enter opens a folder or inserts the selected reference without sending; filesystem chips request read access with optional write.

- A file or directory referenced in a composer message grants the session read — or read and write, if asked — access to it once the message reaches the agent, and says so in the inbox.

- A composer turn can carry inline references: a user message is one structured document of prose and reference occurrences, submitted as `content`+`references` and rendered to the model with a `references:` block. The web composer edits that document directly, with atomic chips, undo, and reference identity through copy/paste within the session; the TUI recalls such a turn with its references held and puts an edited hold back to its spliced spelling rather than silently flattening them. Both clients show attachments by name beside the readable form.

- A web-composer file is an inline upload at the point it was pasted, dropped, or picked: its chip reads upload status without replacing editor content, a pending or failed upload blocks the send by name, an upload a send named is never deleted as unused, and one this draft never sent is cleaned up only once undo can no longer restore it.

- A composer can ask the control channel for discovery candidates: one directory's entries for a typed path prefix (`GET /v1/discovery/paths`) and the host's sessions for a typed `@` prefix (`GET /v1/discovery/agents`), read from names and metadata only.

- Hosted agents and external MCP clients can read `efference://models` to discover exact model slugs, known prices and capabilities, with combined filters and pagination.
- A declared MCP server that offers resources gets a `resources__<server>` tool that lists its resources and templates, or reads one by URI. It is on with the server; `"resources.<server>"` in `[tools]` grants it or turns it off.

- efference's http client now picks up HTTP proxy related environment variables: `http_proxy`/`HTTP_PROXY`, `https_proxy`/`HTTPS_PROXY`, `all_proxy`/`ALL_PROXY`, and `no_proxy`/`NO_PROXY`. Lowercase variables take precedence, including when empty. A missing or empty per-scheme proxy uses the all-proxy fallback.
- The installed web app cycles session tabs with platform keyboard shortcuts, including sessions in the overflow menu.
- A session's tab row offers a right-aligned `+` to start another session in the same project without returning to the overview first.
- Cmd+N in the installed web app opens same-project session settings from a session, or location selection from the home screen.
- The TUI's approval gate names who escalated it and shows their note, as the web client does.
- Fixed the TUI linking artifact fences to loopback pages that loaded broken outside the web client; it now says the fence renders in the web client, and the host no longer binds the extra listener.
- `efference --version` names the git revision and cargo profile the binary was built from.
- A web page left open across a host upgrade now notices that the host serves a newer build and offers a reload, instead of going on rendering with the old one.
- A supplied session name is a preferred base: `POST /sessions {"name": "foo"}` takes `foo`, or the first free `foo-1`, `foo-2`, … when the name is held by a live session, a session directory, or a worktree checkout an earlier session left behind — instead of being refused.

### Changed

- The web client's dev builds no longer install the stream scrubber or the stream debug overlay unless each is switched on in app settings.

- The prose spotter is told the session's supervisor and the sessions it supervises, with whether it created each and its status.

- A session's `agents__archive` of a session it supervises clears without a spotter request.

### Fixed

- `efference --host` refuses to start when another host is answering its socket, and a host whose web bind fails exits with the error instead of running on the socket alone.
- Inline composer image uploads use bounded thumbnails rather than their intrinsic image dimensions.

- Both permission spotters receive an explicit explanation of managed worktree path mapping, distinguishing project grants from the session-storage deny.

- Archiving a session removed a managed worktree other sessions still worked in (the hand-off case): a worktree is now shared occupancy — any session whose working directory sits in it holds it, it is retired at the last occupant leaving, and any former occupant restores it at its original path on resume.
- A create retried after a failed one could start in the broken checkout the failure left behind: a creation now refuses an existing checkout path or `efference/<id>` branch instead of adopting it, and archiving a session while its worktree is still being checked out no longer leaves the checkout behind.
- Mounting an HTTP MCP server intermittently failed with `connection aborted`: the HTTP/2 client closed its shared connection when a late frame arrived for an SSE response it had already canceled (fixed in trillium-http 1.8.2). A mount error now names the JSON-RPC method that was pending.
- Removed false-positive truncation errors caused by providers' token accounting; local-server setup now explains how to configure Ollama's context window.
- A session created under another spelling of its directory (`/users/…` for `/Users/…` on macOS, or through a symlink) refused to resume from the directory's usual spelling; the host now canonicalizes every working directory a client names.
- The recent-locations list showed a directory twice when it gained an `efference.toml` between two launches with no configuration specified.

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
