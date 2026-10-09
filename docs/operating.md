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
  `notify`, `subscribe`, `unsubscribe`, `dismiss`, `proposal`, `image`, `state`,
  `transcript`, `entries`, `title`, each one control-API call
  over the session's control channel ([control-channel.md](control-channel.md)) —
  and the three that address the host: `sessions` (list, with titles), `create`
  (a session in a working directory, this process's environment as its base), and
  `archive` (stop one, keeping its log for a later resume).
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
`turn`, `approve`, `interrupt`, `archive`, `review-level`, `pace`, `title`, `background`,
`dismiss`, `proposal` — reach only sessions the caller created; `notify` and the reads
reach any session in the project. Reach is the project: `sessions__sessions` lists, and every
`sessions__*` target resolves, only among sessions sharing the caller's `.efference/`
base, so an id from another project reads as not found.

`create` from a session runs in the caller's cwd with the caller's child environment as
the new session's base, and records the caller as the child's creator — the field
`GET /sessions` reports as `created_by` ([control-api.md](control-api.md)). It also
subscribes the caller to the child: each time the child stops at a gate, finishes a
turn, stalls, or ends, a notification naming the condition and the call that answers
it lands in the caller's inbox and wakes it ([tools.md](tools.md)). `subscribe` hears
any session in the project the same way, without the wake; `unsubscribe` drops
either. `settle` is not mounted, and the `wait` flags are refused in-session: a
session has its own inbox to be woken through rather than a client that must block.
A child ends with its creator unless created `detached`.

## Where a session lives

Creating a session mints `.efference/<id>/` and never reuses one. The id is an
`adjective-noun` pair unless `--name` supplied one, and it never changes; the
one-line title beside it in the TUI header and the session list does
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
([control-api.md](control-api.md)). With no host running, it starts an island instead
(below). Without `--new`, `tui --name <id>` attaches to that
session whether it runs on the host or on its own socket. Without `--new`, a session flag is an error rather than
silently ignored.

A bare `efference tui` — no `--new`, no `--name`, no `--socket` — asks first
(`src/bin/efference/picker/`): an inline ratatui prompt drawn with the same list, theme and
keys as the new-session form ([tui.md](tui.md)). The top page is the entry that creates a
session here, then, when the live hosts hold sessions whose cwd is this directory, a
`running here` separator and a row for each, then, when this directory has resumable
archived sessions, `resume an archived session…` with their count. The cursor starts on
the create entry, so `efference` ⏎ ⏎ creates a default session: Enter here, then Enter on
the form's "start session". That entry opens the resume page: every resumable archived
session here, most recently active first, under a query line that filters on title and id,
scrolling within the viewport; Esc returns to the top page, and Esc there cancels. A row is
one line in one shape wherever it appears: the title (cut with `…` to fit; the id when the
session has none), the id dimmed, then the status (`idle`, `running`) of a live session or
the age (`16m ago`, `2d ago`) of an archived one, dimmed. A directory whose log cannot be
resumed — unreadable, or one that predates the boot preamble — is not offered. The archived
rows come from `GET /sessions?state=archived` on the first live host, or from the
host-free scan (`src/inventory.rs`) when none is running
([log-and-projection.md](log-and-projection.md)); if the host's listing fails, a warning
prints and the prompt continues without them. Picking a live row is the attach above;
picking an archived one prints a `resuming <id>…` line, revives it with
`PATCH /sessions/<id> {"state": "active", "cwd": …}` — which takes seconds, since the host
re-establishes the environment and mounts the session's MCP servers
([control-api.md](control-api.md)) — and attaches the same way; picking the create entry
opens the new-session form ([tui.md](tui.md)) as a second inline viewport where the
picker's was (the picker's lines are erased first), and attaches to what it creates. `tui --new` stays the non-interactive path: it creates from the flags and
the host's defaults without asking. Sessions the same hosts hold in other directories are
counted in a line above the menu rather than listed — attaching from here to a session
rooted elsewhere would put the TUI's directory and the session's out of step. With no live
or archived session here the prompt is skipped and the form opens directly — its one entry
would be "create", and the form has its own Esc. With no host listening and no session on
its own socket, the prompt lists only the archived sessions and the create entry; an
archived pick starts an island that resumes that session instead of creating one. With
nothing archived there is nothing to pick: the TUI starts an island and opens the same form
against it, so a session is configured the same way with or without a running host. The prompt's
lines are erased once answered. Esc leaves without attaching.

**Islands** (`src/bin/efference/island.rs`, `src/daemon/island.rs`). A TUI that finds no host
runs its own: `efference host --island` as a child process, a session created in this
directory from the host's defaults (or an archived one resumed, from the picker), and the TUI attached to it. Nothing outlives the TUI.
The host is a separate process from birth: its own OS session (`setsid`), so the terminal's
Ctrl-C and hangup never reach it; stdout and stderr to `host.log`, so nothing it prints lands
over the full-screen UI; and stdin a pipe the TUI holds and never writes. That pipe is the
ownership connection. The kernel closes it however the TUI ends — quitting, a crash, SIGKILL —
and the host reads EOF and shuts down as a signal would, reaping every session's processes
([supervisor.md](supervisor.md)). Owner loss is watched from before the host initializes, so
a TUI that dies during startup leaves nothing running. No other client carries ownership: a
second TUI, the web client, or an agent attaching and leaving does not extend or end the
island. An island leaves the terminal as it found it: while the host starts the launcher
shows one status line, erased before the TUI takes the screen, and the boot report stays in
`host.log`. Echoed before the TUI, it was left on the normal screen when the TUI gave the
terminal back. The log is printed only when startup fails — the host refuses to start or
creation fails — where it is the explanation. A setup offer that launches the session erases
its own questions first (`setup::terminal::Terminal::erase`, which counts the rows its
prompts leave), keeping only the line naming a file the wizard wrote.

**The browser island** (`src/bin/efference/island/browser.rs`) is the same launch with
`--bind`: a plaintext loopback web bind, on a port the kernel picks unless the operator
named one, which the host records once listening so its links name it
([web-bind.md](web-bind.md)). After
creating the session, the launcher mints a link over the socket (`GET /auth/link`),
points it at the session's page, and opens the browser. It is always an island, even with
a host running: this choice never adds a web bind to a host. `--bind` on any invocation
reaches it — a bare `efference --bind 8080` in a directory with no host running is this
path, the same one the setup offer's browser choice takes, and without `--bind` the same
invocation starts the terminal island instead. The one flag picks the surface; there is
no verb to learn for it. The owner is the
foreground launcher, never the tab — reloads and stream reconnects cannot end it — and
the terminal holds one prompt line: `o` opens the session again, `c` copies a link, `p`
prints one, `q`, Esc, or Ctrl-C quits. Every link is minted when asked for, because a
code works once and only for about a minute; the cookie it trades for is what keeps a
tab signed in. Quitting erases the prompt and ends the island as the terminal island
does. Each island lives in
`~/.efference/islands/<launcher pid>/` (`host.sock`, `host.log`), which the host removes on a
clean exit and leaves in place when it fails. The strip's leaving verb reads `quit` rather
than `detach` in the owning TUI. The island ignores the `[host]` table's web bind and
takes its address from `--bind` alone ([web-bind.md](web-bind.md)). Islands are not in
host discovery: a second client reaches one by its socket path. Where
stdin or stdout is not a terminal there is nothing to draw in, and the invocation resolves
as it would with an explicit target.

The host owns inference: `~/.efference/config.toml` names its connections and
where each credential comes from, and `efference host` loads them once at launch —
from the secret store, or from its own environment, where `op run -- efference host`
unlocks the vault once for every session the host will create
([transport.md](transport.md)). A creating TUI
passes model selections (`--model <connection>/<model>`) and no inference
credentials; with no `--model`, past picks apply (the project mode, else the
system-wide aggregate), else creation refuses naming the choice and the
new-session form names one. A
host with no config file refuses to start, naming `efference setup`. So does one
whose configured connections include any that cannot authenticate: the host names
each and its reason rather than starting and failing every session that touches
that provider, which would hide the problem at the moment someone could fix it.
`disabled = true` is how a provider stays written down without blocking a launch,
so the rule costs one setup run for a stale connection nobody uses. The check is
the host's, not the registry's — a registry describing a partly-broken world is
still a legitimate thing to build, and an offline verb naming a specific model
already fails on that model alone. A credential that expires *after* launch is a
different matter: sessions exist by then, and it is re-read in place
([transport.md](transport.md)).
The boot report and the TUI session drawer's Session section name each role by connection.

**First run: `efference setup`** (`src/setup.rs`). A bare `efference` on a machine
with no `~/.efference/config.toml` runs it. With one, a directory holding neither an
`efference.toml` nor any session gets the hand-off question below; any other is
`efference tui` — the picker, or an island when no host is running.
Setup is a sequence of inline terminal prompts, not a full-screen view, and it
needs no host and no model: it is the one path that has to work before any
inference is configured. Per connection it asks the provider, a name, and where
the credential comes from — and stops there. **Adding a connection asks nothing
about models.** Which endpoints are configured and which model each role runs are
separate questions, and folding the second into the first asked it once per
connection while answering it at most once. A first run then asks one more
thing: the model sessions use by default, on a connection that can take a turn —
not skippable, since a file with no main model starts no session. The spotter and
triage roles are not asked: each has a default that runs, and a new user cannot
yet know what either is. The defaults editor reaches all three from the
management list. `TypeSafe` is on the provider menu as a decision-model provider
backing the spotter; since it takes no turns, a first run that adds only it is
asked for another provider before the model question.
The credential source is an environment variable (prefilled with the provider's
conventional name), a key pasted now and kept in the OS credential store as
`inference:<name>` ([secrets.md](secrets.md); the file then records only
`kind = "keychain"`), or, for a ChatGPT subscription, one of two ways to hold
that plan. What can be checked
is checked live: a variable that is set in setup's own shell is tried against
the provider, an unset one can be pasted for the check alone (sent to the
provider, never written), a key to be kept is checked before it is kept,
Anthropic offers its model listing as a menu, and a chosen model is described
from the same metadata boot reads. A rejected key or an unknown model
is said and can be kept.

**The hand-off** (`src/setup/handoff.rs`). After writing the file, setup asks
whether to set up the current directory now — in this terminal (the default), in the
browser, or not now — and says what that entails first: an agent session that looks over the project, asks a few questions,
and writes `efference.toml`, billed as ordinary use of the history default when
past picks apply, else as ordinary model use with the model picked in the form.
The wizard is free up to that point, and the question keeps the change to billed
use from happening unannounced. The terminal choice creates a session here — on a
running host, or on an island — and with no project config it is the setup profile;
the browser choice always starts a browser island (below). Not now exits,
saying to run `efference` in the project to set up; the same question is asked on
every later bare `efference` in a directory with no project config and no sessions,
so setup run in the wrong directory is recovered by changing directory. A
connection whose variable is not set in setup's own shell skips the question: the
host would refuse to launch, so setup says to export it and run `efference` in the
project. The prompts draw on `/dev/tty` when stdout is a pipe, which is what
`op run` makes it.

**An endpoint with no name in this build.** The provider menu's last entry is
"Something else", for an endpoint speaking a dialect efference already has an
encoder for at a URL the provider enum does not know
([transport.md](transport.md)). It asks the dialect — Anthropic Messages, OpenAI
Responses, Chat Completions, or System One decisions — then the base URL (for System One, the whole
request URL, since providers spell that route differently), and refuses a URL that
is not one at the prompt rather than at host launch. Both are asked *before* the name, because the
endpoint's host is a better name suggestion than anything the flow could derive
otherwise: `https://api.meta.ai/v1` suggests the connection `meta`, which in turn
suggests `META_API_KEY` for the environment variable, so the two questions with no
conventional answer arrive prefilled anyway. The connection is written as the two
keys it is — `dialect` and `base-url` — never as a `provider` line naming
something that is not a provider name.

A described connection may leave out `auth` entirely: it then sends no credential,
which is right for a local server (`base-url = "http://localhost:11434/v1"` for
Ollama). Setup lists it as "no credential". A named provider still needs its line.

A `completions` connection takes two more keys, written by hand rather than by setup,
for the fields vendors spell differently ([transport.md](transport.md)):
`output-cap-field` (`max_tokens`, the default, or `max_completion_tokens`) and
`body`, a table merged into every request for a vendor parameter whose value is fixed
(`body = { parallel_tool_calls = false }`). `body` may not set a key the encoder
writes, the effort included: that is `--effort`'s. Either key on another dialect fails
the load rather than doing nothing.

**Defaults: which model the spotter runs** (`src/setup/defaults.rs`). `[defaults]`
has one role — `spotter` — and the editor says what it is for rather than
leaving the key name to explain itself. The main model has no default: past
picks answer the omitted branch, else the new-session form names the current
one — and an omitted triage shares the main client, so neither is anything to
configure. The role is set by picking a connection and then a model on it,
which is where a listing belongs: the connection selects the endpoint to ask,
and the credential is read from the connection's own source (an environment
variable set in this shell, or an item the store already holds) rather than
prompted for again. Where setup cannot read one, the model is typed and taken
as written.

A connection may also answer the role for itself: `spotter-model` under
`[connections.<name>]` names the cheap model that connection's sessions use when
nothing else does. It resolves below an explicit `--spotter-model` and
`[defaults].spotter`, and above the arm's built-in default — the order is in
[permissions.md](permissions.md), and the reason a per-connection key exists at
all is that a cheap band is an account's property, not a provider's.

The role leads with what it already is, so the cheapest answer is Enter. An
unset spotter falls back to jev where a typesafe or OpenRouter connection
exists, else to the provider's cheap band, so that is a working configuration
rather than a gap. The first run asks no role at all: a new user cannot yet
know what a spotter is, and the new-session form names the current main model.
Declining the whole visit is a complete answer and writes no `[defaults]` at
all: `src/inference.rs` treats the table as optional, and an omitted model
with no past pick is a refusal naming the choice.

**Managing what is already there** opens on a top level of two entries —
Connections and Default spotter — each summarizing itself, because they are two
concerns and a list that mixed them is what made the model questions land on every
connection. Picking Connections gives the row list below.

The subscription's two ways are asked as one question, since both spend the same
Plus or Pro plan: piggyback on Codex's cached login (`kind = "codex-cache"`,
which needs Codex run often enough to keep the token fresh), or log in here
(`kind = "oauth"`: the browser opens, the grant is kept as `oauth:<name>`
([secrets.md](secrets.md)) and refreshed from there, and Codex need not be
installed). An unavailable way is listed with its reason rather than hidden — no
Codex cache to read, or no store to keep a grant in — and when neither is open
setup says both reasons instead of drawing an empty menu. A login that does not
finish is offered again and records nothing when declined, since a connection
naming a grant that is not there fails at host launch.

The file is written `0600` under the owner; an existing
file keeps its entries, comments, and standing `[defaults]`, and a name already
in it is refused at the prompt. Esc or Ctrl-C at any prompt cancels with nothing
written. `--config <path>` writes elsewhere.

**The connection rows** (`src/setup/manage.rs`). With a config file,
setup opens on the top level rather than on the provider menu, and its
Connections entry lists what the file has — one row
per connection with its name, provider, and a readiness note read locally
(the variable is set, the stored item or login is there, the Codex token's `exp`
has not passed; nothing is asked of a provider, since the list is drawn on every
run) —
plus adding another and finishing. A row offers:

- **Edit**, per kind. An API key can move between the environment and the store,
  or be replaced by rerunning the same credential prompts the add path uses; a
  `codex-cache` connection has no storage to move, so it re-reads instead, and an
  `oauth` connection offers re-authorize where the key kinds offer move: a grant
  is not a value that can be exported to a variable. Moving
  a variable *into* the store needs no paste, since setup already read the value
  for the readiness note — unless the variable holds a `<scheme>://…` reference
  (an `op://…` that a resolver expands at read time), which is refused, because
  storing what the variable literally says would put the unexpanded reference in
  the keychain. The reverse is never spelled as a move: the stored value is
  revealed, the config is switched to a variable, and the stored copy stays
  unless separately cleared — a provider key is shown once at creation, so the
  stored copy may be the only one.
- **Rename**, which also moves the stored item when its name was implicit
  (`inference:<name>` or `oauth:<name>` follows the connection): written under
  the new name, read
  back, and only then removed, so an interruption strands a copy rather than
  destroying the only one. An explicitly pinned `name` is left alone.
- **Disable**, writing `disabled = true`: the connection keeps its models and
  credential source and is never a candidate. `load_credential` is not called
  for it, so it is unavailable by the same mechanism as a credential that failed
  to load.
- **Delete**, which also offers to clear the stored item, never silently.

Disable and delete share one prompt about `[defaults]`, because `[defaults]`
treats them identically: naming a disabled connection is an error at load, as
naming a deleted one is, and both stop the host rather than the one role that
named it. The check reads the declared flag and never a credential's load
result — a config describes itself, and a credential that failed to load for an
outside reason is not a config error. The operator is told which role is
affected and offered a new target; leaving it dangling is allowed and says what
it costs.

A connection setup cannot read whole — an unrecognized `auth.kind`, a missing
`provider`, a non-boolean `disabled` — is an error naming it, not a row left
out. The connection it cannot parse is the one most likely to need repairing,
and a shorter list looks complete; the kinds also grow, so a newer file read by
an older build says it is not understood rather than hiding it.

```toml ~/.efference/config.toml
[defaults]
spotter = "work/anthropic/claude-haiku-4-5"

[connections.work]
provider = "openrouter"
auth = { kind = "env", key = "OPENROUTER_WORK_API_KEY" }

[connections.personal]
provider = "anthropic"
auth = { kind = "keychain" }   # the store's inference:personal item

[connections.codex]
provider = "openai-codex"
auth = { kind = "codex-cache" }   # the login Codex CLI keeps
spotter-model = "gpt-6-luna"      # this account's cheap band, when it is not the arm's default

[connections.chatgpt]
provider = "openai-codex"
auth = { kind = "oauth" }         # the store's oauth:chatgpt grant
```

`env_hook` is the same file's other top-level key, and it decides the environment
a session launched **without one of its own** starts with — a `POST /sessions`
that omits `env`. The terminal form, `sessions__create` and `hand_off` all send
their own and are unaffected; the web form and any script that leaves it out get
the hook. It
is an argv, invoked directly with the session's directory set on the subprocess,
whose stdout is the complete environment as NUL-delimited `KEY=value` records; an
empty array is refused rather than read as "no hook" ([env.md](env.md)):

```toml ~/.efference/config.toml
env_hook = ["mise", "exec", "--", "/usr/bin/env", "-0"]
```

Two things it deliberately does not do: it does not guess a shell, so shell syntax
needs a shell in the array (a Nushell or direnv recipe spells one explicitly), and
it does not merge — a variable the host had and the hook did not report is absent.
With no `env_hook` a session inherits the environment the host process started
with. Nothing here loads your shell configuration for you: if you want that, name
the command that does it.

A fourth read is offline, about spend rather than content, and a developer's instrument
rather than an operator's: **`efference dev cache`** folds a
session's `usage.jsonl` into a prompt-cache read rate per request origin, separating the
cold reads that explain themselves (session boot, a cross-model replay, an idle gap past the
cache TTL) from the ones that do not, and reporting the longest run of the latter. With no
argument it reads the most recently written session; `--all` sweeps the corpus, which is
where a rate is worth comparing against. It needs no daemon and no network.

**`efference dev stops`** is its sibling over `log.jsonl`, about turn endings rather
than spend. A turn ends when the model emits no tool call, which is the harness's only
way to put a question *and* its only way to say the work is done, so the ending alone
does not say which was meant. What the *next* turn carried does: an ending is
`question` when the parting prose asked one, `answered` when the reply supplied
something, `final` when nothing followed, and **`barren`** when neither — the model
asked nothing and was told nothing, so the round trip bought a request and a cache
write and no information. It takes the same session argument and `--all` sweep, and
reports the longest run of barren endings, since a run is the shape a stalled session
makes where a scattered few are ordinary. An assistant entry that records a failed
request (`upstream_failure`) is not an ending — the model said nothing — so a reply to it
is not counted. `ask` calls get a second table: each reply graded by the same test —
answered or barren — with interrupted and unanswered asks counted but not graded, so a
stall moved into a tool call ("Proceeding with X, OK?" → "sounds good") stays visible.
The classification is `log/stops.rs`.

**`efference dev models {connection}`** reads a provider's answer to *which models
do you serve*, over a connection named in `~/.efference/config.toml` and resolved
the way a session's would be — the same file, the same credential, no daemon and
no session. It prints each listing route's status, size, and `ETag`, and the
entries where it recognizes the shape (`data[].id`, or the Codex backend's
`models[].slug`). For the two OpenAI arms it reads both routes, so one run says
whether each catalog answers the other's credential; `--url` adds any route, which
is how the subscription backend's `client_version` gate is visible, and `--raw`
prints bodies verbatim for piping. It reads the routes directly, never through the
Codex arm's five-minute catalog cache, so its output is the provider's answer at
the time of the run (docs/transport.md).

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
`efference control proposal <seq>` (`--cancel` to drop it instead) commits or cancels
one, over `PATCH /proposals/:seq`. A session can file a proposal; it can never resolve
its own.

## The dials

- **`efference control review-level <level>`** — who reviews a call the fast path did not clear.
  `spotter_human` is the supervised default (fast path → spotter → you). `upstream` sends
  what the spotter will not clear to the session that created this one, which answers it with
  `approve`; it is the default for a session another session created, and setting a child back
  to `spotter_human` is how a creating agent hands it to you. A session with no live creator
  reviews as `spotter_human` whatever the dial says. `autonomous`
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
header states it as `⚠ cache misses ×N` beside the context meter. Nothing stalls, so nothing
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

- **`Spot`** — every spotter verdict, its stance, its one-line reason, the prefix it was
  asked to vouch for, and — from the decision rung — every axis probability it answered. Audit-only; it never reaches the wire. `efference dev spotter-log`
  summarizes these across sessions.
- **`ConfigEdit`** — every proposed, committed, or cancelled policy change, with its note.
- **`Triage`** — what each result's peek decided to keep, and whether the verdict came from
  the model or a fallback.
- **`Mark`** — today only a consolidation boundary, written by the `consolidate` tool. There
  is no operator verb that writes a mark; salience annotations are the agent's own.
- **`Preamble`** — the exact system prompt this session booted with, snapshotted, because it
  folds in file contents that drift on disk and is otherwise unreconstructable, plus the
  fixed main connection/provider/native-model identity as unprojected metadata for offline
  readers.

Fast-path clears are deliberately unlogged: the log records the decisions someone made, not
the ones nobody had to.
