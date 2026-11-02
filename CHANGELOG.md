# Changelog

All notable changes to efference. Format follows [keep-a-changelog](https://keepachangelog.com);
one line per feature.

## [Unreleased]

- `efference --version` names the git revision and cargo profile the binary was built from.
- Fixed a session created under another spelling of its directory (`/users/…` for `/Users/…` on macOS, or through a symlink) refusing to resume from the directory's usual spelling; the host now canonicalizes every working directory a client names.
- Fixed the recent-locations list showing a directory twice when it gained an `efference.toml` between two launches that named no configuration.

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
