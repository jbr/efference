# Changelog

All notable changes to efference. Format follows [keep-a-changelog](https://keepachangelog.com);
one line per feature.

## [Unreleased]

### Added

- efference's http client now picks up HTTP proxy related environment variables: `http_proxy`/`HTTP_PROXY`, `https_proxy`/`HTTPS_PROXY`, `all_proxy`/`ALL_PROXY`, and `no_proxy`/`NO_PROXY`. Lowercase variables take precedence, including when empty. A missing or empty per-scheme proxy uses the all-proxy fallback.
- The installed web app cycles session tabs with platform keyboard shortcuts, including sessions in the overflow menu.
- A session's tab row offers a right-aligned `+` to start another session in the same project without returning to the overview first.
- Cmd+N in the installed web app opens same-project session settings from a session, or location selection from the home screen.
- The TUI's approval gate names who escalated it and shows their note, as the web client does.
- Fixed the TUI linking artifact fences to loopback pages that loaded broken outside the web client; it now says the fence renders in the web client, and the host no longer binds the extra listener.
- `efference --version` names the git revision and cargo profile the binary was built from.
- A web page left open across a host upgrade now notices that the host serves a newer build and offers a reload, instead of going on rendering with the old one.

### Fixed

- Removed false-positive truncation errors caused by providers' token accounting; local-server setup now explains how to configure Ollama's context window.
- A session created under another spelling of its directory (`/users/…` for `/Users/…` on macOS, or through a symlink) refused to resume from the directory's usual spelling; the host now canonicalizes every working directory a client names.
- The recent-locations list showed a directory twice when it gained an `efference.toml` between two launches that named no configuration.

## [0.1.1] - 2026-11-01

- The source is public at [github.com/jbr/efference](https://github.com/jbr/efference), under MIT OR Apache-2.0.
- Release archives carry a GitHub build-provenance attestation, checked with `gh attestation verify`.
- Fixed the TUI's policy drawer, which never showed pending config proposals.
- Fixed the web client labelling a process with no pid as "pid null".
- Fixed an archived session leaving the session list before its archive was recorded; an archive now answers once the session and the agents it ends have left.
- Fixed an interrupt pressed just before a model request started taking effect only after that request finished.
- Fixed waiting for a session to settle: it could answer while a queued turn was starting, or wait out its timeout when an agent's message reached a paused session.
- Fixed project setup writing a `.gitignore` in a directory that is not a git repository.
- Clients find the host only at `~/.efference/host.sock`; a project's `.efference/host.sock` is no longer consulted, which also stops `efference` in the home directory listing each session twice. A host bound elsewhere is reached with `--host-socket`.
- A session directory may be typed as `~` or `~/…`, read as the host's home directory.
- The new-session forms offer "Start in its own worktree" only in a directory that can start one: inside a git work tree whose `HEAD` resolves.
- With one connection, the model picker opens on that connection's models instead of a one-row list of connections.
- Fixed the web client's composer input keeping the previous theme's colour after the system switched between light and dark, and not showing its focus colour in Lilac.

## [0.1.0] - 2026-10-27

- Reworked the manual around first use and reader tasks, with descriptive navigation and technical limits kept alongside the choices they affect.
