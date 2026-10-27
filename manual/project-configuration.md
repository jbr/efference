# Project configuration

## What you need to know

A project's `efference.toml`, in its top directory, holds its settings for efference: what
sessions there are for, what they may do without asking you, and what they start out
knowing. Every key in it is optional.
<!-- src: src/config/raw.rs::RawConfig; src/config/load.rs -->

The first time you run `efference` in a directory with no `efference.toml`, it offers to set
the project up: a session that asks you a few questions and writes the file with you
([Quick start](quick-start.md)). To change the file later, you can ask a session in the
project to do it. efference's own description of every key is at
`efference://guide/config-format`, which a session can read with its `read` tool. Read the
change before you start the next session, since the file decides what later sessions may do
without asking.
<!-- src: src/setup/handoff.rs::offer_project_setup; assets/guide/new-project.md; assets/guide/config-format.md; src/guide.rs::MOUNT; src/policy/paths.rs::context_granted (the guide prefix is read-granted) -->

Each session copies `efference.toml` when it starts, into its own
`.efference/<session>/config.toml`, and works from that copy. Editing `efference.toml`
changes the sessions you start afterwards, not the ones already running, and a resumed
session keeps its copy. A permission granted during a session goes into its copy
([Permissions](permissions.md)).
<!-- src: src/boot/scaffold.rs::snapshot_config; src/boot/resume.rs (load_session_config); src/agent/config_edit.rs::commit_config_edit -->

### What the file holds

| Key | What it sets | More |
| --- | --- | --- |
| `scope`, `watch-for` | What sessions here are for, in a sentence, and what the spotter should look out for | [Permissions](permissions.md) |
| `[tools]` | Which tools sessions have — efference's own are off unless enabled — and which of them run without asking | [Permissions](permissions.md) |
| `[paths]`, `[commands]` | What else runs without asking: files and directories, shell commands by prefix | [Permissions](permissions.md) |
| `[context]` | What a session starts out knowing: files such as `AGENTS.md`, and the output of commands run at start | |
| `[env]` | Environment variables for the agent's commands: which are removed, which are masked in the session's record, and values to set, including `op://` references | [Credentials](credentials.md) |
| `[mcp.servers.<name>]` | MCP servers whose tools sessions here can call | |
| `[transport]` | `effort`, the prompt-cache keepalive interval, and how long one response may stream | [Triage](triage.md) |
| `[bash]` | How long a shell command holds the turn before it moves to the background | |
| `first-turn` | A message sent when a session starts, so the agent speaks first | |
| `consolidation-bytes` | How much finished tool work prompts the agent to summarize it | [Triage](triage.md) |
<!-- src: src/config/raw.rs::RawConfig, RawMcp, RawBash, RawTransport; assets/schemas/project.v1.json -->

`efference check` runs the checks a session runs when it starts, without starting one:
it reads the file, runs its `[context]` commands, starts its MCP servers, and prints what
the file permits.
<!-- src: src/bin/efference/check.rs; src/config/load.rs -->

### Editor support

A first line of `#:schema https://efference.net/schemas/project.v1.json` gives an editor with
a TOML language server completion, a description of each key on hover, and a mark on a
misspelled one, as for [your own configuration](configuration.md#editor-support). The setup
session asks before adding it, since the editor then fetches the description from
efference.net.
<!-- src: assets/guide/new-project.md (the interview asks first); src/config_schemas.rs -->

## How this works, precisely

A key efference does not know, or an entry it cannot read soundly, fails the whole file, and
no session starts until it is fixed. So does a file listed in `[context]` that cannot be
read, a `[context]` command that fails, and an MCP server that fails to connect, unless
the server is marked `optional = true`.
<!-- src: src/config/raw.rs::RawConfig (deny_unknown_fields), validate; src/context.rs; src/boot/mcp_mount.rs::mount_all (optional) -->

Starting a session runs commands the file names, without asking you: each `[context]` entry
with `exec`, each MCP server with a `command`, and `op read` for each `op://` reference in
`[env]`. A project's `efference.toml` comes with the project, so read one you did not write
before starting a session in it.
<!-- src: src/context.rs::run_exec (bash -c at boot, no review); src/mcp/stdio.rs; src/boot/mcp_mount.rs::mount_one; src/env_table/resolve.rs::run_command; absence of a project-trust check in src/boot/ -->

A session started with no `efference.toml`, the setup session or any other, may read and
write the project directory and use efference's own tools; it also has the tools that reach
other sessions, reviewed. Everything else goes to the spotter.
<!-- src: src/boot/scaffold.rs::CONFIG_SCAFFOLD, snapshot_config -->
