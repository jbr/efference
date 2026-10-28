# Project configuration

A little project context saves you from explaining the same things in every conversation.
Your project’s `efference.toml` tells new sessions what they’re here to do, what they may do
without asking, and what they should know at the start. It lives in the project’s top
directory, and every key is optional.
<!-- src: src/config/raw.rs::RawConfig; src/config/load.rs -->

## Set up the project with an agent

You don’t have to write the file from scratch. The first time you run `efference` in a
project without one, it offers a setup session that asks questions and writes the file
with you ([Quick start](quick-start.md)). Later, you can ask any session in the project to
help revise it. The agent can read a description of every key through its `read` tool at
`efference://guide/config-format`.
<!-- src: src/setup/handoff.rs::offer_project_setup; assets/guide/new-project.md; assets/guide/config-format.md; src/guide.rs::MOUNT; src/policy/paths.rs::context_granted (the guide prefix is read-granted) -->

Review the changes before starting your next session: this file decides what future
sessions can do without asking. If you’re opening someone else’s project, read its
`efference.toml` before starting a session, too. Some settings run commands at startup,
without a permission prompt; [the details below](#what-runs-at-startup) explain which ones.
<!-- src: src/config/load.rs; src/context.rs::run_exec; src/mcp/stdio.rs; src/boot/mcp_mount.rs::mount_one; src/env_table/resolve.rs::run_command; absence of a project-trust check in src/boot/ -->

## Choose what sessions start with

Use the file to give sessions their working context and tools, and to decide how much
review you want along the way:

| Key | What you can use it for | More |
| --- | --- | --- |
| `scope`, `watch-for` | Describe the work in a sentence, and flag changes the permission spotter should look out for | [Permissions](permissions.md) |
| `[tools]` | Choose which tools sessions have, and which run without asking; efference’s own tools are off unless enabled | [Permissions](permissions.md) |
| `[paths]`, `[commands]` | Allow file access and shell command prefixes without asking | [Permissions](permissions.md) |
| `[context]` | Include files such as `AGENTS.md`, or the output of commands run at startup | |
| `[env]` | Set variables for the agent’s commands, remove inherited ones, or mask their values in the session record; `op://` references are supported | [Credentials](credentials.md) |
| `[mcp.servers.<name>]` | Connect MCP servers whose tools sessions can call | |
| `[transport]` | Set `effort`, the prompt-cache keepalive interval, and how long one response may stream | [Triage](triage.md) |
| `[bash]` | Choose how long a shell command holds the turn before moving to the background | |
| `first-turn` | Give new sessions a starting message, so the agent speaks first | |
| `consolidation-bytes` | Choose how much finished tool work prompts the agent to summarize it | [Triage](triage.md) |
<!-- src: src/config/raw.rs::RawConfig, RawMcp, RawBash, RawTransport; assets/schemas/project.v1.json -->

## Know which sessions a change affects

Think of `efference.toml` as the starting point for new sessions, not a live control for
all of them. Each session copies it into `.efference/<session>/config.toml` at startup and
works from that copy. Editing the project file affects sessions you start afterwards;
running sessions are unchanged, and resumed sessions keep their copies.
<!-- src: src/boot/scaffold.rs::snapshot_config; src/boot/resume.rs (load_session_config) -->

A permission granted during a session goes into that session’s copy. If you want future
sessions to have it too, add it to `efference.toml` ([Permissions](permissions.md)).
<!-- src: src/agent/config_edit.rs::commit_config_edit; src/boot/scaffold.rs::snapshot_config -->

## Check your changes

Run `efference check` to see how the configuration will load without starting a session.
It reads the file, runs its `[context]` commands, starts its MCP servers, and prints what
the file permits. It is a startup check, not a way to inspect an unfamiliar configuration
without executing it.
<!-- src: src/bin/efference/check.rs; src/config/load.rs -->

For help as you edit, add this as the file’s first line:

```toml
#:schema https://efference.net/schemas/project.v1.json
```

With a TOML language server, your editor can complete keys, explain them on hover, and
mark misspellings, just as for [your own configuration](configuration.md#editor-support).
The setup session asks before adding the line, because it makes the editor fetch that
description from efference.net.
<!-- src: assets/guide/new-project.md (the interview asks first); src/config_schemas.rs -->

## Startup details

### When a configuration won’t load

An unknown key or an invalid entry stops the session from starting rather than being
ignored. So does an unreadable `[context]` file, a failing `[context]` command, or an MCP
server that cannot connect. You can mark an MCP server `optional = true` if a session
should be able to start without it.
<!-- src: src/config/raw.rs::RawConfig (deny_unknown_fields), validate; src/context.rs; src/boot/mcp_mount.rs::mount_all (optional) -->

### What runs at startup

Starting a session runs these commands from the configuration without asking you first:

- `[context]` entries with `exec`;
- MCP servers with a `command`;
- `op read` for each `op://` reference in `[env]`.

That’s why a project’s configuration deserves the same review as other code you run from
that project.
<!-- src: src/context.rs::run_exec (bash -c at boot, no review); src/mcp/stdio.rs; src/boot/mcp_mount.rs::mount_one; src/env_table/resolve.rs::run_command; absence of a project-trust check in src/boot/ -->

### If there is no project configuration

A session without `efference.toml`, including a setup session, may read and write the
project directory and use efference’s own tools. It also has reviewed access to the tools
that reach other sessions. Everything else goes to the permission spotter.
<!-- src: src/boot/scaffold.rs::CONFIG_SCAFFOLD, snapshot_config -->
