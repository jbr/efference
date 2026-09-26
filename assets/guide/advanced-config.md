# Beyond the first setup

What efference can do that is not part of every project's setup. Each section is offered
when something in the project, or something the operator says, points at it — not walked
through on every run. `efference://guide/new-project` is the setup itself.

## Driving efference from another agent harness

When the project shows another harness at work — a `CLAUDE.md`, a `.claude/` or `.cursor/`
directory, an `.mcp.json` — efference can be mounted in it as an MCP server. The other
harness then gets the tools a person has at the TUI: list sessions, start one in a
directory, send it a turn, read its transcript, resolve its approvals, end it. That is how
one harness supervises efference sessions or hands work to them.

The server is the `efference` binary itself: `efference mcp` speaks MCP on stdin and stdout.
For Claude Code, the project's `.mcp.json` gains one entry:

```json
{
  "mcpServers": {
    "efference": {
      "command": "efference",
      "args": ["mcp"]
    }
  }
}
```

Other harnesses take the same command and arguments in their own MCP configuration. The
mount needs no running session and no host when it starts: it reaches the host when a tool
is called, so it can sit in the configuration permanently.

`.mcp.json` belongs to the other harness, and is often committed. Write it only when the
operator says yes, merge into an existing file rather than replacing it, and show them the
result.

## Secrets the project's tools need

A session's commands start with the environment of whoever started the session. When a
project's own tooling needs a credential — a test suite that calls an API — the `[env]`
table can supply it at session start without it being written anywhere:

```toml
[env]
TEST_API_KEY = "op://Private/Test API/credential"  # resolved by 1Password at start
OTHER_TOKEN = { cmd = "pass show project/token" }  # any command that prints it
DEPLOY_TOKEN = { strip = true }                    # present in the environment, kept from commands
```

A value supplied this way is masked: it never appears in the session's log, even if a
command prints it. Names ending in `_KEY`, `_SECRET`, `_TOKEN`, `_PASSWORD`, or
`_CREDENTIALS` are masked by default wherever they come from. `strip = true` keeps a
variable out of every command the session runs — the right answer for a credential only a
person should use. `efference://guide/config-format` has the full table.

This is only for credentials the *project* needs. The keys efference itself uses to reach a
model live in the user-wide `~/.efference/config.toml`, not here.

## Files that hold secrets

A `[paths]` deny keeps the file tools out of a path:

```toml
[paths]
"." = true
".env" = false
"secrets" = false
```

It governs `read`, `write`, `search`, and the rest of the file tools only. A shell command is
judged by `[commands]` and the permission spotter, never by `[paths]`, so a deny is a
guardrail against a mistaken tool call rather than a wall; the `scope` saying the file is off
limits is what the spotter reads.

## Claude Code memories

A `CLAUDE.md` in the repo means Claude Code has worked here, and it may have kept
per-project memories at
`~/.claude/projects/<workspace path with every "/" replaced by "-">/memory/` — for a
workspace at `/home/<user>/code/<project>`, that is
`~/.claude/projects/-home-<user>-code-<project>/memory/`. Each memory is one Markdown file
with a frontmatter `description`, so listing the directory rides as one stub per memory,
read on demand. The index file `MEMORY.md` is Claude Code's own and is excluded. Whether
efference sessions should see them is the operator's call — they were written for another
harness and may say things that are not true here — so ask before looking, and if yes the
entry is:

```toml
[context]
Memories = { path = "/home/<user>/.claude/projects/-home-<user>-code-<project>/memory", exclude = ["MEMORY.md"] }
```

Read-only as written; `create = true` would let sessions add memories of their own there,
which is a further question.

## Hazards for the spotter

`watch-for` names things the permission spotter should look out for in this project —
never shown to the session itself, which is the point: it directs the reviewer's attention
without teaching the reviewed to route around it.

```toml
watch-for = [
  "anything that runs scripts/deploy.sh",
  "connections to the production database",
]
```

Set it when the operator names a hazard, not from a guess.
