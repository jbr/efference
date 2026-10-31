# Changelog

All notable changes to efference. Format follows [keep-a-changelog](https://keepachangelog.com);
one line per feature.

## [Unreleased]

- Fixed the TUI's policy drawer, which never showed pending config proposals.
- Fixed the web client labelling a process with no pid as "pid null".
- Fixed an archived session leaving the session list before its archive was recorded; an archive now answers once the session and the agents it ends have left.
- Fixed waiting for a session to settle: it could answer while a queued turn was starting, or wait out its timeout when an agent's message reached a paused session.

## [0.1.0] - 2026-10-27

- Reworked the manual around first use and reader tasks, with descriptive navigation and technical limits kept alongside the choices they affect.
