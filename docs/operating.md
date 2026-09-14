# Operating a session

You are supervising a running efference session. This is what you can see, what you can
change, and what each situation calls for. It is written for whoever is at the controls —
a human with a terminal or an agent with a socket; there is no privileged client, so the
same surface serves both.

The rest of `docs/` describes how the harness is built. This one describes how it is
driven, and it is refined from use rather than written ahead of it.

## The command groups

`efference --help` lists only what a person types: `host`, `tui`, `check`. The rest is grouped by who it is for.

- **`efference control <tool>`** — the calls that drive a *running* session:
  `turn`, `approve`, `settle`, `interrupt`, `background`, `pace`, `review-level`,
  `notify`, `dismiss`, `image`, `state`, `transcript`, `entries`, `title`, each one control-API call
  over the session's control channel ([control-channel.md](control-channel.md)) —
  and the three that address the host: `sessions` (list, with titles), `create`
  (a session in a working directory, this process's environment as its base), and
  `end` (tear one down).
- **`efference mcp`** — the same calls, served as an MCP server over stdin/stdout
  (below). One definition, three entry points: they are written once as mcplease tools
  (`src/control_tools.rs` and `src/control_tools/`, one file per tool) and mounted by
  the binary for the command line and the stdio server, and by a hosted session's own
  toolbox for its model (below), so the paths cannot drift apart.
- **`efference dev <verb>`** — instruments for developing efference itself: `wire`,
  `gallery`, `peek`, `spotter`, `spotter-log`, `spotter-eval`, `cache`, `view` (a recorded log in the TUI, inert), and `open`, the
  web-bind bootstrap-link mint ([web-bind.md](web-bind.md)). Compiled under the `dev`
  cargo feature, which is in `default`; a release build passes `--no-default-features`.

`--name` and `--socket` are global, so they sit before the group: `efference --name foo
control state`.

### Mounting the control tools in another harness

`efference mcp` speaks newline-delimited JSON-RPC on stdin/stdout and blocks until stdin
closes, which is the transport a locally-spawned MCP server speaks. A Claude Code
`.mcp.json` mounts it with:

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

The mount addresses the host, not a session: nothing is resolved at startup, so the
host need not be running yet and the mount outlives any one session. The host is the
first live of `.efference/host.sock` in the mount's working directory and
`~/.efference/host.sock`, else the latter, or `--host-socket <path>`.
Every session tool takes a `session` id — what `sessions` lists and `create` returns —
and a call without one is an error that names what the host currently holds. The
command line resolves the way the other client verbs do (`--name`/`--socket`, else
the single live session), and `efference --name my-session mcp` gives the mount
that same default, which a call's `session` then overrides. The server's
`instructions` and each tool's description are written for the model reading
`tools/list`; the behaviour hints say which tools read and which mutate, since the
spec's defaults are pessimistic and a mounting harness's approval UX reads them. A
`turn` submitted over the mount is attributed as `mcp[<clientInfo.name>]` from the
name the client sent at initialize, so the session reads which harness is speaking
without the caller saying so (docs/control-api.md).

The three waiting tools — `settle`, and `turn`/`approve` with `wait` — take a `timeout_s`,
because MCP clients time tool calls out. On expiry they answer with the current state
marked `settled: false` rather than with an error, so a caller never has to tell "still
working" from "the socket broke".

### The control tools inside a session

A session created on a host also mounts these verbs in its own toolbox, under the wire
names `sessions__<verb>` (`sessions__turn`, `sessions__state`, `sessions__create`). The
session's model can commission a child, send it a `turn`, resolve its gate, read its
`state` or `transcript`, and end it, without an operator relaying between them. The calls
reach the host in-process and carry the calling session's id, so a `turn` lands on the
child as `session[<caller id>]` ([control-api.md](control-api.md)).

Two rules are enforced by the mount, not by config. A session is never its own target: a
call naming the caller, or naming no `session`, is an error. And the mutating verbs —
`turn`, `approve`, `interrupt`, `end`, `review-level`, `pace`, `title`, `background`,
`dismiss` — reach only sessions the caller created; `notify` and the reads reach any
session in the project. Reach is the project: `sessions__sessions` lists, and every
`sessions__*` target resolves, only among sessions sharing the caller's `.efference/`
base, so an id from another project reads as not found.

`create` from a session runs in the caller's cwd with the caller's child environment as
the new session's base, and records the caller as the child's creator — the field
`GET /sessions` reports as `created_by` ([control-api.md](control-api.md)). `settle` is
not mounted, and `wait` is refused in-session: a session has its own inbox to be woken
through rather than a client that must block.

## Where a session lives

Creating a session mints `.efference/<id>/` and never reuses one. The id is an
`adjective-noun` pair unless `--name` supplied one, and it never changes; the
one-line title beside it in the sidebar and the session list does
([session-title.md](session-title.md)). The directory holds:

- `control.sock` — the HTTP control channel, the only input path into the loop.
- `log.jsonl` — the append-only record; every entry, raw.
- `config.toml` — this session's live perimeter, copied once from the project template at
  boot and independent of it thereafter.

A client verb finds the socket by `--name <id>`, or by scanning for the single
*connectable* one, or from an explicit `--socket`. A stale directory from a dead daemon is
skipped, so discovery finds the running session rather than the most recent one.

## Reading the session

Each read is at its own grain.

- **`efference control state`** — the whole ephemeral picture in one JSON object: the pending gate,
  the inbox, running processes, token usage and cost, the pace and review-level dials, the
  **effective** policy in force, and the three condition topics (`stall`, `upstream_retry`,
  `config_repair`). Effective, not boot: a grant made twenty minutes ago shows up here.
- **`efference control transcript [--since N | --tail N]`** — what the session has said and
  done, one line per entry with its `seq`: prose in full, each call by name, arguments and
  intent, each result by size, error flag and triage verdict, never a body. Entries are
  immutable, so `--since <last_seq>` after a wait reads only what is new.
- **`efference control entries <seq>…`** — the whole entries behind transcript lines: a
  result's complete content, a call's exact arguments. The raw window (`GET /log`) is not
  a verb: at a terminal the jsonl is the same bytes, and over MCP a window of raw bodies
  was too large to read and too small to audit.
- **`GET /events`** — one SSE stream carrying everything as it happens, in one order:
  durable log entries, pre-commit output (assistant text, thinking, tool arguments
  arriving, live process output), and whole-topic state replacements. A client holding this
  stream never needs to poll.

`efference tui` is the interactive view of all three; `efference dev view <session>` opens a
recorded log in the same rendering, inert. `efference tui --new` first creates a session on a
running `efference host` (the first live of `.efference/host.sock` here and
`~/.efference/host.sock`, or `--host-socket <path>`) from this
directory, this shell's environment, `--name`, and the session flags,
then attaches under `/sessions/<id>/`; quitting detaches, and the session stays on the host
([control-api.md](control-api.md)). Without `--new`, `tui --name <id>` attaches to that
session whether it runs on the host or on its own socket, and a plain `tui` attaches to
the single live session if there is exactly one. Without `--new`, a session flag is an error rather than
silently ignored.

The host owns inference: `~/.efference/config.toml` names its connections and
where each credential comes from, and `efference host` loads them once at launch —
from the secret store, or from its own environment, where `op run -- efference host`
unlocks the vault once for every session the host will create
([transport.md](transport.md)). A creating TUI
passes model selections (`--model work/anthropic/claude-sonnet-4-5`, or a native id
on the default connection) and no inference credentials; with no `--model`, the
host's `[defaults].model` applies, and with none configured, creation says so. A
host with no config file refuses to start, naming `efference setup`.
The boot report and the sidebar's SESSION section name each role by connection.

**First run: `efference setup`** (`src/setup.rs`). A bare `efference` on a machine
with no `~/.efference/config.toml` runs it; with one, it says what to run next.
Setup is a sequence of inline terminal prompts, not a full-screen view, and it
needs no host and no model: it is the one path that has to work before any
inference is configured. Per connection it asks the provider, a name, where the
credential comes from, the agent model, and the spotter model, then whether to
add another, and which connection sessions default to when there is a choice.
The credential source is an environment variable (prefilled with the provider's
conventional name), a key pasted now and kept in the OS credential store as
`inference:<name>` ([secrets.md](secrets.md); the file then records only
`kind = "keychain"`), or, for Codex, the CLI's cached login. What can be checked
is checked live: a variable that is set in setup's own shell is tried against
the provider, an unset one can be pasted for the check alone (sent to the
provider, never written), a key to be kept is checked before it is kept,
Anthropic offers its model listing as a menu, and a chosen model is described
from the same metadata boot reads. A rejected key or an unknown model
is said and can be kept. The file is written `0600` under the owner; an existing
file keeps its entries, comments, and standing `[defaults]`, and a name already
in it is refused at the prompt. Esc or Ctrl-C at any prompt cancels with nothing
written. `--config <path>` writes elsewhere.

```toml ~/.efference/config.toml
[defaults]
model = "work/anthropic/claude-sonnet-4-5"
spotter = "work/anthropic/claude-haiku-4-5"
# triage omitted: the peek shares the agent's client

[connections.work]
provider = "openrouter"
auth = { kind = "env", key = "OPENROUTER_WORK_API_KEY" }

[connections.personal]
provider = "anthropic"
auth = { kind = "keychain" }   # the store's inference:personal item

[connections.codex]
provider = "openai-codex"
auth = { kind = "codex-cache" }
```

A fourth read is offline, about spend rather than content, and a developer's instrument
rather than an operator's: **`efference dev cache`** folds a
session's `usage.jsonl` into a prompt-cache read rate per request origin, separating the
cold reads that explain themselves (session boot, a model change, an idle gap past the
cache TTL) from the ones that do not, and reporting the longest run of the latter. With no
argument it reads the most recently written session; `--all` sweeps the corpus, which is
where a rate is worth comparing against. It needs no daemon and no network.

**`efference check [path]`** is the other no-daemon verb, and it is about the template rather
than a session: it parses `efference.toml` (or the path given), applies the soundness
checks a session boot applies, resolves every `[context]` entry from the current
directory (`exec` entries included), and mounts every `[mcp.servers.*]` stanza to
enumerate each server's advertised tools against the file's `tools` grants — the computed
trust perimeter, so the grant is written with the tool table in front of it. Mounting
reuses stored credentials and never runs the authorization dance; a server that needs
authorization is reported, not authorized. It prints the grants and the preamble's byte
count, or the boot-time error with a non-zero exit. A required server that does not mount
fails the check, same as boot; an `optional = true` one is reported and tolerated
(docs/tools.md). It is the terminus the new-project guide names for a "set this project
up" task, so a session asked to do that has something to run that is not a session.

Two things worth knowing about what you see. Tool output on the transcript is the **raw**
result — the model's copy has been focused down by a triage pass, so what you read and what
it reads differ, deliberately. And the pace dial (below) is operator-view state that is
never logged and never projected, so slowing or holding a session leaves no trace the model
can perceive.

## The gate

When a call is not cleared by policy, the loop blocks and `state.gate` names it: the call
id, the tool, its input verbatim, and — when the spotter ran — its one-line reason. Four
decisions resolve it:

| decision | effect |
| --- | --- |
| **approve** | run this call, once. No policy changes. |
| **abort** | reject it; the turn unwinds. |
| **redirect** | reject it, and send text to the model as the next user turn. |
| **config_edit** | change the rule, then re-check *this* call against the new one. |

`efference control approve` sends the first; `--abort` and `--redirect <msg>` the next two. In the
TUI, an empty composer approves, `.abort` aborts, and any other text redirects.

**What approving scopes: this call, and nothing else.** There is no "allow this kind of
thing" in an approval. A call that comes back needing a correction — a `write` refused
because the file exists, retried with `overwrite: true` — is a *new* call and gates again.
Six approvals for two logical writes is what that looks like from the outside, and it is
working as designed: the mechanism has no way to know the second call is the first one
fixed. If you find yourself approving the same shape repeatedly, that is the signal to
change the rule instead (below), not to approve faster.

**Redirect is the underused one.** It keeps the turn alive: the model reads the rejection
and your words together, in the same message, and continues. Aborting throws away the
trajectory; redirecting steers it.

## Granting instead of approving

A `config_edit` decision carries one `ConfigChange`, writes it to the session's
`config.toml`, folds it into effective policy, and re-classifies the gated call. The call
runs only if the new rule actually clears it — the rule changes, and the mechanism
re-authorizes; nothing rides along.

The vocabulary is four shapes:

- **`command`** — a bash command prefix, matched token-wise, longest-prefix-wins.
  `cargo` grants the whole program; `git log` grants only that sub-verb. Granting broad and
  carving out (`git` = clear, `git push` = deny) is available in the config file, though a
  grant made at a gate is always a *clear*.
- **`path`** — a filesystem subtree with `read`/`write`. Deeper keys govern shallower ones,
  so a narrow grant inside a broad denial works, and a narrow denial inside a broad grant
  works too.
- **`tool`** — a structured tool name. Path-bearing tools still need their target under a
  path grant, so this is half of a grant, not all of it.
- **`scope`** — replaces the perimeter prose the spotter judges against. The frozen boot
  preamble is not re-rendered; the session learns about it as a notification.

`path` and `scope` are **session-visible**: the session is told its perimeter moved.
`command` and `tool` reach it only through the classifier — the next call simply does not
gate.

In the TUI, `.allow` at a gate commits a grant directly: bare, it derives one from the
gated call (the bash program, or the tool's name); `.allow git log` grants that prefix
instead. Over the API it is
`PUT /approvals/:call_id {"decision": "config_edit", "change": {...}}`.

**When a grant beats an approval.** Ask whether the *next* call of this shape should also
stop you. If the answer is no — you are about to approve `cargo test` for the fourth time,
or the session needs to read a directory it will keep needing — grant it. A grant is
specific, written to a file you can read, and prunable; it is not an "always allow" mode.
The two cases where reaching for a grant is clearly right:

- **A repeated gate on the same prefix.** Grant the prefix. The session stops asking and
  you stop deciding the same thing.
- **A read-only path outside the working tree** — a dependency source, a reference
  checkout. A `path` grant with `read = true` is one edit; the alternative the harness makes
  available otherwise is copying the material into the session's scratch directory, which
  works and costs a detour.

The perimeter also moves without you: the session may **edit its own `config.toml`** to
request a change, and that write always gates for you whatever else the config allows —
including under `--autonomous`. Reading the diff *is* reading the request, and approving it
is granting it. Separately, the spotter files **proposals** for command prefixes it has
judged safe across repeated sightings; they sit inert in `state.proposals` until
`PATCH /proposals/:seq` commits or cancels one. A session can file a proposal; it can never
resolve its own.

## The dials

- **`efference control review-level <level>`** — who reviews a call the fast path did not clear.
  `spotter_human` is the supervised default (fast path → spotter → you). `autonomous`
  removes the synchronous human: the spotter still runs and can clear, and what it will not
  clear is handed back to the model with the reason and a pointer at its own config file —
  so routine gates self-resolve and your attention is spent on genuine policy requests.
  `human_only` skips the spotter entirely and sends everything it did not clear to you.
- **`efference control pace [--speed-ms N] [--hold | --resume]`** — a between-turns wall-clock
  delay, and a hold overlay on top of it. Holding parks the loop at the next juncture; the
  speed underneath is retained, so resuming returns to it. Sending a turn also resumes.
  None of this is visible to the agent.
- **`efference control interrupt`** — signal a running tool: its process group is killed. Fired
  while the model is generating instead, the *next* call does not run. Fired where a
  result lands, the call's output is kept out of the model's context entirely while staying
  in the log for you — the answer when a cleared call is about to put several thousand
  tokens of the wrong thing into the transcript.
- **`efference control background [--note …]`** — hand the running foreground command to the
  background supervisor instead of killing it. It keeps running as `p#<n>`, its output
  surfaces at later steps, and the optional note is delivered to the model as a user
  message alongside the handoff.

## Speaking to a running session

- **`efference control turn <text>`** — a user turn. It never waits for the trajectory to end: it is
  picked up at the next juncture, after the current tool's results land, and merges into the
  same message as those results. So mid-trajectory steering arrives *with* the evidence the
  model is about to reason from, not after it. `--source <label>` names who is submitting
  when it is not the operator (`web`, `mcp[<name>]`, `agent[<id>]`); the label rides on the
  turn and prefixes it for the model (docs/control-api.md). Over the MCP mount it defaults
  to the mounting harness's name (below).
- **`efference control notify <source> <body>`** — an ambient notification into the inbox. Unlike a
  turn, this is **triageable**: the model may pin it, dismiss it, or leave it. Use it for
  things that can wait and might not matter. `--slot <name>` supersedes an earlier
  notification on the same channel rather than stacking.

- **Drop a file into `.efference/<id>/attachments/`** — an attachment. The bytes stay on disk;
  the session is told what arrived (type, size, shape) and reads it, or not, on the terms of
  whatever you asked for ([inbox.md](inbox.md)). Prefer this to pasting a large blob into a
  turn: pasted bytes ride in the cached prefix for the rest of the session, an attachment's
  do not.

The asymmetry is deliberate: a user turn is canonical and cannot be dismissed by the model;
a notification can. Say it as a turn when it must land.

**`efference control image <seq> [idx]`** writes a logged result image back out as a file — the same
bytes the model was sent, not a re-render of a source file that may since have changed.

`--wait` on `turn` and `approve`, and the standalone `efference control settle`, block until the
daemon reaches a decision point and print the resulting state — which is how a script or an
agent operator drives a session as *act, read, act* instead of polling.

## Silence, and what it means

A session that has said nothing for two minutes is in one of a few conditions, and
`efference control state` distinguishes them. Guessing is no longer necessary.

- **`upstream_retry` is non-null** — a request or a stream failed and the transport is
  retrying on a capped schedule. The topic carries the attempt, the budget, when the next
  attempt is due, and the upstream's own words. Nothing to do; it is working.
- **`stall` is non-null** — the retries were exhausted, the request failed past them, or
  the upstream refused it outright (an exhausted plan, a rejected request).
  The loop appended its record and returned to idle. Every other signal reads like a
  finished run (`running: false`, no gate, an empty inbox), which is why this topic exists.
  The recovery is a `POST /turns` — send a turn and the session picks up.
- **`config_repair` is non-null** — the config file on disk does not parse *and* could not
  be reverted. The loop is parked, deliberately: effective policy in memory is still the
  last good config, but the file no longer shows it, and the file is what says what the
  session is running under. Nothing you send clears this. Edit the file until it parses;
  fs-watch adopts it and the loop wakes.
- **A gate is pending** — `state.gate` is non-null and everyone is waiting on you.
- **Everything null and `running: true`** — it is thinking, or a tool is running. The event
  stream's `thinking_delta` and `tool_call_input` frames are what tell a live turn from a
  hung one; the TUI renders them.

Long-poll `GET /state?wait=settled` (or `efference control settle`) blocks until a gate opens or the
loop goes idle, capped server-side, and is the shape a supervising process should loop on.

The inverse condition is a session that looks entirely healthy and is quietly paying full
price for every turn: `usage.cold_streak` counts consecutive main-loop requests that read
nothing from the prompt cache with no reason to (docs/log-and-projection.md), and the TUI
sidebar states it as `⚠ cache cold ×N` beside the context meter. Nothing stalls, so nothing
else says so. `efference dev cache` is the detail view.

## What the session was already told

Operator guidance that contradicts the preamble costs the session a turn reconciling them.
The frozen system prompt tells it, in these words or close to them:

- **Explore the workspace directly.** The cwd is the workspace root; it should look rather
  than ask.
- **Tool output is focused for it.** Only the relevant parts of each result stay on its
  transcript, marked with `… [N lines elided] …`. It is told to treat what arrives as the
  output — and specifically **not** to pre-filter by piping to `head`/`grep`/`sed`, because
  trimming before the harness can focus risks discarding the part that mattered. Telling it
  to grep its own output undoes this.
- **Prefer the file tools over the shell** — `read`, `search`, `write`, `list` — because
  their structured results curate cleanly.
- **`$TMPDIR` is scratch space**: a private per-session directory, always granted
  read/write, exported into every tool child, expanded in the file tools' path arguments,
  and deleted at session end (`--keep-scratch` retains it). It is the sanctioned
  answer for temporary files, and it does not need a grant.
- **Every call carries an `intent`**: one line on why this call, now, written before the
  other arguments. It is stripped before the tool runs and is shown to you at the gate. It
  is deliberately *not* shown to the spotter, so a call cannot argue its way past that rung.

Two things the session is **not** told: the path to its own config file (it learns that from
a gate rejection when a move actually strays, so the perimeter stays out of its attention
until it is relevant), and the `watch-for` list (which the spotter holds instead).

## Reading the log for audit

Beyond turns, calls, and results, the log carries the decisions:

- **`Spot`** — every spotter verdict, its stance, its one-line reason, and the prefix it was
  asked to vouch for. Audit-only; it never reaches the wire. `efference dev spotter-log`
  summarizes these across sessions.
- **`ConfigEdit`** — every proposed, committed, or cancelled policy change, with its note.
- **`Triage`** — what each result's peek decided to keep, and whether the verdict came from
  the model or a fallback.
- **`Mark`** — today only a consolidation boundary, written by the `consolidate` tool. There
  is no operator verb that writes a mark; salience annotations are the agent's own.
- **`Preamble`** — the exact system prompt this session booted with, snapshotted, because it
  folds in file contents that drift on disk and is otherwise unreconstructable.

Fast-path clears are deliberately unlogged: the log records the decisions someone made, not
the ones nobody had to.
