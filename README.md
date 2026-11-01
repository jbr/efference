# efference

An agentic coding harness, written in Rust, that curates its own context. Before the agent
sees a tool result, a forked turn reads the full output and keeps the lines that answer
what the agent said it was looking for; the full output stays in the session's append-only
log. Sessions run in a long-lived host process, so you can start one in the terminal, leave
it working, and pick it up again from a browser or your phone. efference speaks four wire
protocols — Anthropic Messages, OpenAI Responses, OpenAI Chat Completions and System One —
so it works with Anthropic, OpenRouter, OpenAI, a ChatGPT subscription, decision models
such as TypeSafe's jev, and other endpoints that speak one of those APIs, local servers
included.

The manual is at [efference.net](https://efference.net).

## Get started

Install the latest release:

```sh
curl -fsSL https://efference.net/install.sh | sh
```

The installer puts `efference` in `$XDG_BIN_HOME`, or `~/.local/bin` when that is unset,
and adds the directory to your shell's `PATH` if it isn't there already.

To skip the installer, download the archive for your machine from a release and put the
`efference` binary inside it on your `PATH`:

```
https://efference.net/releases/<tag>/efference-<target>.tar.xz
```

`<tag>` is a release tag such as `v0.1.0`, and `<target>` is one of
`aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu` or
`x86_64-unknown-linux-gnu`. Releases built in this repository are also attached to its
[GitHub releases](https://github.com/jbr/efference/releases).

Then run it in a project:

```sh
cd ~/code/your-project
efference
```

The first launch walks you through connecting a model, and offers an agent session that
helps you write the project's `efference.toml`: what the project is for, what sessions
should know at startup, and what they may do without asking. After that, `efference` opens
a new session, or a short menu when the project already has some: attach to one that is
running, or resume an earlier one. The [quick start](https://efference.net/quick-start)
walks through the rest.

efference runs on macOS and Linux, on x86_64 and aarch64. The agent's commands need
`bash`; git is needed only for worktree sessions. To update, run the install command again;
`efference update --check` reports whether a newer version is out.

## What it does

### Context the agent can use

- **Triage of every tool result.** Shell output, file reads and MCP results go through a
  triage request that branches from the agent's conversation and selects the lines that
  serve the intent the agent stated with the call. The agent can run `cargo test` and say
  what it is looking for instead of guessing with `| tail -3`. Triage uses the agent's own
  model, sharing its prompt cache, or a separate one, and the clients can show the selected
  lines beside the full output. ([Triage](https://efference.net/triage))
- **Consolidation and compaction.** The agent can replace a finished stretch of work with a
  summary it writes, having seen the work in full. When a session has gone idle and its
  cache is cold, the harness can instead compact from a digest of one line per tool call.
- **A log that keeps everything.** Each session is an append-only jsonl log. The request
  sent to the model is projected from the log every turn, and curation decisions are
  stored as verdicts on log entries, so the raw output is never overwritten and every
  decision can be audited.
- **Stable prompt caching.** Identical logs project to byte-identical requests, and a
  request changes only after its last cache breakpoint. An idle session can send small
  keepalive requests to hold the provider's cache warm.
- **Declared startup context.** The `[context]` table in `efference.toml` names the files
  (such as `AGENTS.md`) and command outputs a session starts with; nothing is discovered
  implicitly.

### Sessions that outlive the terminal

- **Standalone or host mode.** With no host running, `efference` starts one for that
  terminal alone. `efference --host` runs one host for every project on the machine;
  leaving the terminal interface then detaches, and the session keeps working.
  ([How efference runs](https://efference.net/how-it-runs))
- **Resumption.** Archived sessions can be resumed from the project's menu. A host that
  restarts reopens the sessions that were still open, idle until you continue them.
- **Worktree sessions.** A session can work in its own git worktree and branch, so its
  edits don't land in the checkout you're using.

### Terminal, browser and phone

- **Terminal interface** with syntax themes, and images drawn inline in terminals that
  support them.
- **Web client** served by the host and compiled into the same binary: installable as an
  app on desktop or mobile, with image attachments and opt-in push notifications when a
  session needs an approval, asks you a question, or finishes a turn.
- **Artifacts.** The agent can write HTML pages and Mermaid diagrams, which the web client
  shows in a sandboxed frame with bundled libraries; the terminal interface links to them
  for a local browser.
- **Remote access** over Tailscale or efference's own TLS (HTTP/1.1, HTTP/2 and HTTP/3),
  with per-device sign-in that can be revoked one device at a time.
  ([Remote access](https://efference.net/remote-access))

### Models and wire protocols

Each connection in `~/.efference/config.toml` names a provider efference knows, or
describes another endpoint by its protocol and base URL. A session can use different
connections for its main model, its triage model and its permission spotter.

| Protocol | Used for | Providers |
| --- | --- | --- |
| Anthropic Messages | turns | Anthropic, OpenRouter, any Messages endpoint |
| OpenAI Responses | turns | OpenAI's platform API, a ChatGPT subscription (the Codex backend), any Responses endpoint |
| OpenAI Chat Completions | turns | any Chat Completions endpoint, including local servers that take no key |
| System One | the permission spotter | TypeSafe's jev directly or through OpenRouter, or another System One endpoint |

System One is the decision-model contract: one request asks typed questions about a state
and gets back a calibrated probability for each answer rather than generated text. A
decision model backs the spotter, not the agent's turns.
([Configuration](https://efference.net/configuration))

### Tools

- **Shell** commands through `bash`. A long command moves to the background; the agent ends
  its turn and is woken when the process exits or prints a line it is watching for.
- **File tools and structural editing** built on tree-sitter, and **LSP** definition,
  references and rename, with language servers shared across the sessions in a project.
  These are enabled per project in `efference.toml`.
- **`read_image`** to bring an image file, or a region of one, into context.
- **MCP servers** over stdio or streamable HTTP, including OAuth sign-in.
- **Questions to you.** The agent can ask you a question and wait for the answer.

### Agents working together

A session can create, message and check on other sessions in its project. A child's status
arrives in its creator's inbox instead of through a waiting call, a child ends with its
creator, and a session can hand its work to a fresh successor. A child's held calls go to
the session supervising it, which can answer them or pass them up to you.

### Permissions you choose

- **Rules in `efference.toml`** for paths, tools and shell command prefixes decide what runs
  without asking.
- **A spotter model** reviews the remaining calls against a scope you write in prose, and
  holds the ones it doesn't clear for you. It sees the call, not the agent's reasoning for
  it. It can suggest permitting a command it keeps seeing; nothing changes until you
  accept.
- **Review levels** decide who answers a held call: you, the supervising session, or no one.
  Calls can be approved from the terminal, the browser or a phone.
  ([Permissions](https://efference.net/permissions))
- **`efference check`** validates an `efference.toml`, and a published JSON schema gives
  editors completion for it. ([Project configuration](https://efference.net/project-configuration))

### Credentials, environment and privacy

- **Keys** from your OS keychain, environment variables (including through 1Password's
  `op run`), or secret commands such as `pass`, AWS Parameter Store or `systemd-creds`.
  ([Credentials](https://efference.net/credentials))
- **The `[env]` table** removes inherited variables from the agent's commands and masks
  known secret values in the session record.
- **An environment hook** gives sessions your project's environment from direnv, Nix,
  devenv, devbox, Flox, mise, asdf and similar tools, including sessions a host creates for
  the web client.
- **No telemetry, analytics or crash reporting.** efference connects to the model
  providers and MCP servers you configure, the push service of a browser that turned on
  notifications, and efference.net for a daily version check only if you opt in.

### Driving it from scripts and other agents

Every client — the terminal interface, the web client, the CLI — speaks the same HTTP
control API: over the host's Unix socket, or its TCP listener for the web client.
`efference control` drives a running session from
a script: message it, decide its held calls, read its transcript. `efference mcp` serves
the same verbs as an MCP server, so another agent harness can create and drive efference
sessions.

## This repository

The source is published here for reading and audit. It is generated from a development
repository by leaving out the paths that stay private (session notes and planned work), so
its commit hashes differ from the development repository's, and some links in `docs/` name
files that are not here. Pull requests are read; one that is accepted is applied in the
development repository and arrives here with the next publish.

[docs/README.md](docs/README.md) is the top of the architecture documentation.

## Building

Stable Rust, and pnpm with Node for the web client, which the build compiles into the
binary. The syntax themes are submodules.

```sh
git clone --recurse-submodules https://github.com/jbr/efference
cd efference
cargo build --release --no-default-features
```

`--no-default-features` matches a release build, which leaves out the `efference dev`
verbs.

## Verifying a release

Every file under `efference.net/releases/` has a [minisign](https://jedisct1.github.io/minisign/)
signature beside it, made with the key whose public half release binaries carry to check
their own updates:

```sh
minisign -Vm efference-aarch64-apple-darwin.tar.xz \
  -P RWQVdWuS5d3d6sqA6eXxVuPYvV7/DdkJDgAyDTLmYd3Lz7yssgny/Vq0
```

The signature's trusted comment, which minisign prints, names the version and the file:
`efference <tag> <file name>`.

Releases built in this repository also carry a GitHub build-provenance attestation for each
archive, naming the commit and workflow run that built it:

```sh
gh attestation verify efference-aarch64-apple-darwin.tar.xz --repo jbr/efference
```

## License

MIT OR Apache-2.0, at your option.
