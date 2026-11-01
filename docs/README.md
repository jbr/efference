# efference — architecture

> An experimental, API-backed agentic coding harness in Rust, built to make
> *context management* a first-class, iterable concern.
> The name is from the neuroscience term *efference copy*: the copy of a motor
> command an organism keeps to predict and reconcile the sensory consequences of its
> own action. A harness that deliberately curates what it will "perceive" next is
> doing the same thing.

This is the top of the architecture tree: what efference is, the core data model,
the invariants, and a map to the per-subsystem docs — each of which names the
module that implements it so you can go straight to the code instead of grepping.
The docs are present-tense and describe how the system works now. One of them is not
architecture: [operating.md](operating.md) is the *usage* document — how to drive a
running session from the outside. The usage documents served to sessions through the
guide overlay are in `assets/guide/` ([context.md](context.md)). The design
positions behind it are in [positions.md](positions.md); where a word in use differs from
the identifiers behind it, [glossary.md](glossary.md) pairs them; planned work lives in
[`../backlog/`](../backlog/); the terse record of shipped features is
[`../CHANGELOG.md`](../CHANGELOG.md); session narrative memory is
[`../journal/`](../journal/).

## What this is

efference is a long-lived host process (`efference --host`) that holds every session and
runs their agent turn loops, against two wire dialects: Anthropic Messages and OpenAI
Responses ([transport.md](transport.md)). It is driven entirely through an HTTP control
channel — over a Unix socket, ordinarily the user-wide `~/.efference/host.sock`, and
optionally over TCP as the web client's front door ([web-bind.md](web-bind.md)) — which
the TUI, the CLI verbs, scripts, and other agents share as equal clients. A session takes
the working directory and environment of the client that created it, and outlives that
client: detaching a TUI leaves the loop running in the host.

It is one take on the harness category — a small, deliberately curated featureset —
built on positions rather than a thesis: chiefly that autonomy is a
runtime dial rather than a product identity, and that friction experienced by the
agent is a defect even when the work completes anyway. [positions.md](positions.md)
records the positions and the observed failures behind them.

## The data model

Each session is an append-only log; the wire transcript sent to the API is
`project(log)`, recomputed each turn. Storing a log and rebuilding the request
from it is how most harnesses work; what efference commits to on top of that is:

- **Rendering decisions are stored as per-entry verdicts, not content edits.**
  Curation (triage, elision) flips a `WireState` (`Raw → Triaged → Elided`) carrying the
  *verdict* (`keep` ranges + an optional note); `project` re-renders from the
  immutable `raw` each turn. Because the raw result survives in the log, every
  verdict is reversible and auditable.
- **`project` is byte-deterministic, and tests pin it** (see the invariants
  below).
- **Decisions get first-class log entries** — summaries, triage verdicts,
  spotter verdicts, config edits — recorded at the moment they're known.

```rust
struct LogEntry { seq: u64, turn: u32, at: Timestamp, payload: Payload }

enum Payload {
    User(Vec<ContentBlock>),
    Assistant { text: Vec<ContentBlock>, calls: Vec<ToolCall> },
    ToolResult { call_id: CallId, raw: RawResult, wire: WireState },
    Summary(Summary),   // text replacing a range of the transcript (consolidation or compaction)
    Ambient(Notification),  // an out-of-band notification
    Triage(Triage),     // a triage verdict — the audit record
}

enum WireState {
    Raw,                                             // project the full raw result
    Triaged(Triaged),                            // a verdict (keep, note, intent_feedback), re-rendered by project
    Elided,                                          // project a placeholder; still recoverable
}

fn project(log: &[LogEntry]) -> Vec<ApiMessage>;     // deterministic, byte-stable
```

Because rendering decisions live in `project`, these behaviors are projection
changes rather than log rewrites:

| Behavior | Becomes |
| --- | --- |
| Intent-aware compaction (drop dead-end deliberation, keep the conclusion) | a projection that keeps `Summary` entries and elides the ranges they replace |
| Continuous scrubbing (drop the irrelevant lines of a result) | a projection that `Elide`s or `Triaged` those entries |
| Progressive doc/tool reveal | a projection where a subtree / tool-def hasn't been expanded yet |
| Reversibility of all of the above | raw output is never overwritten |

The log projects to more than one view: the model's context
(`project(log) → messages`), the human's TUI transcript (a second projection of
the same log to a renderable tree, [tui.md](tui.md)), and a reconnecting
control-channel client's catch-up replay ([control-channel.md](control-channel.md)).
See [log-and-projection.md](log-and-projection.md) for the full type set and
[triage.md](triage.md) for how verdicts are produced.

## The invariants (do not break these)

Violating any one silently breaks caching or correctness:

1. **`project` is byte-deterministic.** Identical log → byte-identical wire, every time.
   Prompt caching is a prefix match: any byte change in the prefix invalidates everything
   after it. A non-stable projection silently kills the cache. (Determinism hazards:
   serialize tool-call inputs with sorted keys; never project timestamps.) Two tests pin
   this — re-projecting an identical log yields identical bytes, and the exact wire
   encoding is fixed so any reorder/rename of a wire field fails *on purpose*.
2. **Committed regions are immutable on the wire; all mutation happens in the uncommitted
   tail past the last cache breakpoint.** Triage is fixed *at append*, so no cached
   prefix entry is ever mutated. The one operation that rewrites prefix — a consolidation
   fold — is exactly one deliberate cache write.
3. **raw is never overwritten; only `wire` flips.** This is what makes every verdict
   reversible and the log a complete audit record.
4. **A completion may make several tool calls.** A turn under `auto` tool choice is
   free to call tools in parallel: the request does not set `disable_parallel_tool_use`
   (Messages) and sends `parallel_tool_calls: true` (Responses). The loop runs every call
   a completion made, holding the results, then triages them together in one triage request
   ([triage.md](triage.md)) — a triage fired between calls would carry
   unanswered `tool_use` blocks, which the API rejects. What *is* invariant is the
   pairing — every `tool_use` gets exactly one `tool_result`, or projection orphans it
   and the API rejects the request. A forced tool choice (the spotter's `spot`) still
   disables parallel calls, because its answer is one verdict. Requests once asked for
   one call per turn; models read the restriction (and a matching preamble sentence) as
   a pace to keep, making one read per turn and ending turns on announcements, so it
   was dropped (journal/2026-09-28-upstream-failures-and-parallel-calls.md).
5. **No privileged client.** The loop's only input is the HTTP control channel; the human
   TUI, the driver, and subagents are all equal clients of the same socket. See
   [control-channel.md](control-channel.md).

## Shared vocabulary

To describe wire/log states compactly, two aligned rows — the **LOG** (source of truth;
everything, raw) and the **WIRE** (`project(LOG)`; what is sent):

| token | meaning |
| --- | --- |
| `U` | user turn |
| `A` | assistant prose |
| `t(cmd)` | `tool_use` — a tool call |
| `R(…)` | a **raw** tool result (full content) — lives in the LOG only |
| `r(…)` | a **triaged** tool result — what the WIRE carries, e.g. `r(L40-52)` |
| `┃` | a cache breakpoint |
| `_` | an elided entry (projects to a placeholder) |

Example — a big `cat` triaged to two line ranges, a small `grep` kept whole, breakpoint
at the tail:

```
LOG :  U  A t(cat file)  R(486 lines)       A t(grep fork)  R(7 lines)
WIRE:  U  A t(cat file)  r(L40-52,310-318)   A t(grep fork)  r(L1-7)     ┃
```

## The map

Read top-down; open the subsystem doc you need.

| Subsystem | Doc | Governing modules |
| --- | --- | --- |
| The log, the projection, the caching constraint | [log-and-projection.md](log-and-projection.md) | `src/record/log.rs` (+ `src/record/log/`), `src/record/project.rs`, `src/record/persist.rs` |
| The names a recorded entry carries — tool and namespace names, and the labels a turn's `source` is written in | [tools.md](tools.md), [log-and-projection.md](log-and-projection.md) | `src/record/names.rs`, `src/record/names/tools.rs`, `src/record/names/sources.rs` |
| Per-result triage — how every tool result is triaged, and the text view a separate triage model reads | [triage.md](triage.md) | `src/curation/triage.rs`, `src/curation/triage/text_view.rs`, `src/record/project.rs` |
| Tools (in-process `Tool` trait; in-process MCP; per-call telemetry; which tools a session mounts) | [tools.md](tools.md) | `src/tools.rs`, `src/tools/lsp.rs`, `src/tools/bash.rs`, `src/agent/telemetry.rs`, `src/config/tools.rs` |
| Trajectory consolidation (completed working spans and juncture checks) | [consolidate.md](consolidate.md) | `src/curation/consolidate.rs`, `src/agent.rs` |
| Trajectory digest (one line per call, the summarization substrate) | [digest.md](digest.md) | `src/curation/digest.rs` |
| Summaries (the record that replaces a range of the transcript) and compaction (the harness writing one from the digest) | [summary.md](summary.md) | `src/record/log/summary.rs`, `src/curation/compact.rs`, `src/daemon/compact.rs`, `src/record/project.rs`, `assets/prompts/` |
| Ambient notifications — the inbox (enqueue then triage), and the attachment store | [inbox.md](inbox.md) | `src/curation/inbox.rs`, `src/daemon/notifications.rs`, `src/attachments.rs`, `src/daemon/attachments.rs` |
| Session status — the one view of where a session stands (the `status` topic), its plaintext rendering, and the subscriptions that post it | [tools.md](tools.md), [control-api.md](control-api.md) | `src/protocol/status.rs`, `src/daemon/status.rs` (+ `src/daemon/status/`), `src/daemon/subscriptions.rs` |
| Session id and title — the minted word-pair address, and the triage-kept one-line title | [session-title.md](session-title.md) | `src/daemon/slug.rs`, `src/agent/title.rs`, `src/curation/triage/title.rs` |
| Starting sessions — the host's entry for a session while its environment and MCP servers are prepared, and how a failure or an archive ends it | [control-api.md](control-api.md) | `src/daemon/starting.rs`, `src/daemon/host/entries.rs`, `src/daemon/host/preparation.rs` |
| Session resumption — reopening a persisted session under its id, and listing the archived ones | [resumption.md](resumption.md) | `src/boot/resume.rs`, `src/daemon/host/resume.rs`, `src/daemon/host/archived.rs`, `src/daemon/host_http/patch.rs`, `src/daemon/host_http/archived.rs`, `src/inventory.rs`, `src/tui/resume_list.rs` |
| Worktree sessions — a session's own git checkout, and the project/working directory pair | [worktrees.md](worktrees.md) | `src/worktree.rs`, `src/record/session_dir/worktree.rs`, `src/config/policy/directories.rs`, `src/boot.rs`, `src/daemon/host.rs` |
| Background processes — the process monitor | [process-monitor.md](process-monitor.md) | `src/tools/process_monitor.rs`, `src/tools/bash.rs`, `src/platform/spawn.rs`, `src/platform/terminal_text.rs`, `src/daemon/background.rs` |
| Control channel & the `Frontend` trait (HTTP over a Unix socket) | [control-channel.md](control-channel.md) | `src/daemon.rs` (+ `src/daemon/`), `src/client.rs`, `src/frontend.rs`, `src/agent.rs` |
| The control channel's contract — the session collection's bodies, rows and answers, a turn's submission id and where it landed, `GET /state` (`StateTopics`) and a type per state topic, the status view, the `output` class's deltas, transcript reads and the read cursor, the attention report, and the small bodies routes answer with: the `{status}` word, the remembered locations, a directory listing, the sign-in and enrollment answers | [control-api.md](control-api.md) | `src/protocol.rs`, `src/protocol/sessions.rs`, `src/protocol/turns.rs`, `src/protocol/state.rs` (+ `src/protocol/state/`), `src/protocol/status.rs`, `src/protocol/stream.rs`, `src/protocol/transcript.rs`, `src/protocol/attention.rs`, `src/protocol/answers.rs`, `src/protocol/projects.rs`, `src/protocol/files.rs`, `src/protocol/inventory.rs`, `src/protocol/sign_in.rs` |
| The session directory — the base session dirs live under, the log's file name, which ids may name a session, and the owner-only modes it and its files are created with | [control-channel.md](control-channel.md), [state-store.md](state-store.md) | `src/record/session_dir.rs`, `src/daemon/permissions.rs` |
| The `agents__*` tool contract — the namespace, the wire names the loop treats specially, the `decide` argument, and the `BoundAction` a decision binds | [tools.md](tools.md), [permissions.md](permissions.md) | `src/protocol.rs`, `src/protocol/agents.rs`, `src/control_tools.rs` |
| Permissions — role config & the approval fast-path | [permissions.md](permissions.md) | `src/config.rs`, `src/config/policy.rs`, `src/agent.rs`, `src/daemon/config_edits.rs` |
| The `[context]` preamble builder — what the session boots knowing | [context.md](context.md) | `src/config/context.rs`, `src/config.rs`, `src/config/env.rs`, `src/config/policy.rs`, `src/config/guide.rs` |
| Operating a session — what an operator can see, change, and say (usage, not architecture) | [operating.md](operating.md) | `src/client.rs`, `src/control_tools.rs`, `src/bin/efference/control.rs`, `src/bin/efference/verbs.rs` |
| The `[env]` table — stripping and injecting env vars at spawn, masking them at append, resolving vault references | [env.md](env.md) | `src/config/env_table.rs`, `src/config/env_table/resolve.rs`, `src/record/persist.rs`, `src/tools/bash.rs` |
| Transport & streaming (upstream SSE, deltas, keepalive, the shared HTTP client) | [transport.md](transport.md) | `src/inference/api.rs`, `src/platform/http.rs` |
| Host-owned inference — named connections, credential loading, per-role model selection | [transport.md](transport.md) | `src/config/user_config.rs`, `src/inference.rs`, `src/boot.rs`, `src/daemon/host.rs` |
| Config schemas — the published JSON schemas for both config files, and the `#:schema` line that names them | [config-schemas.md](config-schemas.md) | `src/config/config_schemas.rs`, `src/config/raw.rs`, `src/config/user_config.rs`, `assets/schemas/` |
| The user state database — `~/.efference/state.db`, and the table per module that owns it | [state-store.md](state-store.md) | `src/platform/state.rs` (+ each table's module) |
| Model catalog — each connection's normalized model listing, stored in the state database | [model-catalog.md](model-catalog.md) | `src/inference/catalog.rs`, `src/daemon/host_http/catalog.rs`, `src/inference/api/provider/row.rs` |
| MCP authorization — the OAuth dance for connected HTTP providers | [mcp-auth.md](mcp-auth.md) | `src/platform/oauth.rs` (+ `src/platform/oauth/`), `src/platform/www_authenticate.rs`, `src/tools/mcp/auth.rs` (+ `src/tools/mcp/auth/`), `src/tools/mcp/http.rs` |
| The secret store — API keys and OAuth grants in the OS credential store, or through `[secrets]` commands | [secrets.md](secrets.md) | `src/platform/secrets.rs`, `src/platform/secrets/commands.rs`, `src/tools/mcp/auth/store.rs`, `src/config/user_config.rs`, `src/inference.rs` |
| The TCP bind — the web client's front door (root-derived credentials, enrolled devices, signing a browser in by approval, the gate) | [web-bind.md](web-bind.md) | `src/platform/auth.rs`, `src/platform/devices.rs`, `src/platform/sign_ins.rs`, `src/daemon/handler.rs`, `src/daemon/serve.rs`, `src/daemon/host_routes.rs`, `src/daemon/tcp.rs` (+ `src/daemon/tcp/`), `src/bin/efference/devices.rs` |
| The web client, component gallery, and stream tape/replay API | [web-client.md](web-client.md) | `web/`, `src/daemon/tcp/gate.rs`, `src/daemon/tcp/headers.rs`, `src/daemon/tcp/routes.rs` |
| Fenced HTML artifacts — signed links, sandbox, runtime, theme, built-in libraries and files | [artifacts.md](artifacts.md) | `src/record/artifact.rs`, `src/daemon/artifacts.rs` (+ `src/daemon/artifacts/`), `src/tui/artifacts.rs`, `web/src/ui/Artifact.tsx`, `web/public/artifacts/`, `web/src/theme/`, `assets/artifact-libs/`, `assets/guide/artifacts.md` |
| The update check — the signed `latest.json`, `efference update --check`, the host's daily check, and the `newer_version` topic | [update-check.md](update-check.md) | `src/platform/update_check.rs`, `src/config/user_config/updates.rs`, `src/daemon/host/update_check.rs`, `src/daemon/newer_version.rs`, `src/bin/efference/update.rs`, `src/setup/updates.rs` |
| Web push — the daemon notifies a pocketed phone | [web-push.md](web-push.md) | `src/platform/push.rs` (+ `src/platform/push/`), `src/daemon/push.rs`, `src/daemon/attention.rs`, `src/daemon/host/attention_report.rs`, `src/daemon/delayed_push.rs` |
| The TUI client | [tui.md](tui.md) | `src/tui/`, `src/config/user_config/tui.rs`, `src/platform/terminal_title.rs` |

Where a module has grown a directory beside it (`src/inference/api/`, `src/config/`, `src/config/policy/`,
`src/record/project/`, `src/curation/triage/`, `src/config/context/`, `src/daemon/`, `src/tui/*/`), the `.rs` file
holds the subsystem's vocabulary and public surface and the directory holds one file per
concern; each module's tests live in its own `test.rs`.

The turn engine that ties it together is `src/agent.rs`, its idle and trajectory phases in
`src/agent/trajectory.rs` and the model request in `src/agent/request.rs`; the CLI binary is
`src/bin/efference/`, whose `--host` flag boots the daemon. Its command tree is grouped by
who each command is for ([operating.md](operating.md)): bare `efference` (the TUI) and the human verbs at the top level, `efference
control <verb>` for the calls that drive a running session — its agent verbs the
ones `efference mcp` serves over MCP and a hosted session mounts as `agents__<verb>`, each
written once in `src/control_tools.rs` and `src/control_tools/` — and
`efference dev <verb>` for the instruments that develop the harness itself, behind the
`dev` cargo feature.

`src/daemon.rs` holds the shared-state vocabulary and the loop wiring; each concern it
serves lives in a `src/daemon/` submodule named for its governing doc — `handler.rs`
(the one handler stack every listener mounts), `serve.rs` (`serve_host`, which binds the
host's listeners), `http.rs` with one file per route family under `http/`, `host_routes.rs`
(the host's route table) with its handlers in `host_http.rs` and `host_http/`, and
`stream.rs` (the control channel), `config_edits.rs` (permissions), `notifications.rs` (the
inbox), `background.rs` (the process monitor), `phase.rs` (where the loop is) and `verbs.rs`
(what each loop route would answer now), plus `pace.rs`, `approvals.rs`, `metrics.rs`,
`keepalive.rs`, `watch.rs`, `topic_values.rs` (the `/state` readouts), and `views.rs` (the `/log` readouts).

## Dependency direction

The crate's modules import in one direction, and `src/test/layering.rs` fails on an
import that runs the other way. A group is a directory under `src/` whose file of the same
name declares its members (`src/platform.rs`, `src/record.rs`, `src/curation.rs`). Three
groups are named for one of their modules, whose own file is the group's root and whose
directory also holds the others as submodules: `config`, `inference` and `tools`. The
modules not yet in a group are at the top of `src/`. These rules hold:

- **`record::names` imports no other module.** It holds the names a recorded entry
  carries and its readers match on: tool and namespace names with the helpers that take
  them apart (`src/record/names/tools.rs`), and the labels a turn's `source` is written in
  (`src/record/names/sources.rs`).
- **`record::log` imports no other module but `record::names`.** It holds the vocabulary a
  record is written in, which includes the shapes other subsystems fill in or read: a provider and
  its wire dialect, a gate's `Approval`, the resolved `[context]`, an attachment's
  `Arrival`, the spotter's `Axes`, and the triage model a preamble records. Behavior that
  needs another subsystem stays with it, as `impl` blocks on the type the log defines: `api`
  implements `Provider`, and `context` resolves and renders `Resolved`.
- **`platform` imports no other group.** It holds the machine and user services that know
  nothing of a session: the user state database, the secret store, the shared HTTP client,
  OAuth and `WWW-Authenticate` parsing, push sending, the update check, the TCP bind's
  credentials, enrolled devices and sign-ins, and the helpers for spawning processes,
  watching files, terminal text and titles, home-relative paths and pretty-printing.
- **`record` imports only `platform`.** It holds the log and its deterministic views:
  `names`, `log`, `persist`, `recovery`, `project`, `artifact` and `session_dir`.
- **`config` imports only `platform` and `record`.** It holds configuration and what is
  built from it: the project config in `config` itself, the permission policy (`policy`),
  the system preamble and its `[context]` entries (`env`, `context`, `guide`), the `[env]`
  table (`env_table`), the recorded model picks (`history`), the user configuration
  (`user_config`) and the published schemas (`config_schemas`).
- **`inference` imports only `platform`, `record` and `config`.** It holds the connection
  registry in `inference` itself, the upstream wire (`api`), the model catalog (`catalog`)
  and, under test, the scripted upstream and scratch helpers (`testing`).
- **`protocol` imports only `platform`, `record`, `config` and `inference`.** It holds the
  names and shapes that cross the control channel or the tool wire.
- **`tools` imports only those and `protocol`.** It holds the toolbox in `tools` itself and
  the tool backends: `bash`, `mcp`, `vision`, `process_monitor` and `expand`.
- **`curation` imports only those and `tools`.** It holds `triage`, `spotter`, `inbox`,
  `compact`, `consolidate` and `digest`.
- **A module outside the outer set imports nothing from it.** The outer set is `daemon`
  and `boot`, which host sessions; `control_tools`, the session-control verbs; and the
  clients `client`, `tui`, `setup` and the `bin` crate. What the inner modules and the
  outer ones both need sits in modules that import nothing above them: `src/record/session_dir.rs`
  (where a session lives on disk) and `src/protocol.rs` (the names and shapes that cross
  the control channel or the tool wire).

A rule names a group, a module or a module inside a group, and covers the modules below
it: the test keys a file by its module path (`record/log/summary.rs` is
`record::log::summary`) and an import by the path it spells. A `super::` chain that leaves
its file's module counts as the `crate::` path it resolves to.

The imports that still run against the last rule are named in the test, each by the
file that makes it and the path it imports: the turn loop (`agent`) holds the `Session`
and refreshes the config through `daemon`. A new import of that kind fails the test, and
so does a named one that has gone, until its line is deleted. The toolbox (`tools`)
reaches the session-control tools through a trait it defines, `SessionControl`, which
`control_tools` implements. The host builds the server a session mounts through a function
the serving process installs ([tools.md](tools.md)).

## Repository layout

Everything is one crate in one repository — daemon, clients, CLI, and the written records.
A workspace split waits for a build target that cannot link the full dependency set; none
currently exists, because rendering for thin clients is daemon-side (the human-wire render
document) rather than a shared client library. Two roads considered and not taken: a
browser-side wasm core (superseded by daemon-side rendering) and multiple repositories
(the control API is unversioned and co-evolves with its clients, wire-format changes want
atomic commits alongside the byte-stability pin, and the records in `docs/`, `backlog/`,
`CHANGELOG.md`, and `journal/` are repo-scoped). Client code in other languages lives
in-tree and is embedded in the serving binary.

## Conventions for this tree

- **Present tense.** Describe what *is*, not what landed when. The dated narrative lives
  in [`../journal/`](../journal/); the shipped-feature list in [`../CHANGELOG.md`](../CHANGELOG.md).
- **No file:line citations** — they rot. Name the governing module inline (e.g.
  "implemented in `src/record/project.rs`") so a reader knows where to look, and let the code
  carry the line-level detail.
- **Depth bar:** a sufficiently sophisticated peer should be able to write a rough
  approximation of the project from these docs alone. The key types, the
  control/projection flow, the invariants, and the why that protects them belong here;
  exact details are the code's job.
- **Keep rationale only where it earns its place** — when it protects an invariant a
  future reader could unknowingly break, or records a road already taken and rejected.
  No decisions log.
- **Describe, don't evaluate.** State mechanisms and what breaks if they change — no
  praise words, no banner theses, no comparative claims about other harnesses. Assume
  any sentence here may be quoted out of context by a future summarizer; keep only
  sentences that stay true and modest when repeated. These docs are institutional
  knowledge whose primary readers are models, and framing is memetic in both
  directions — rigor propagates as readily as slogans ([positions.md](positions.md)).
- A doc graduates from `topic.md` to `topic/README.md` + child files only when it grows too
  big to scan in one sitting.
