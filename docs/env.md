# The `[env]` table — stripping at spawn, masking at append

One name-keyed table in `efference.toml`. Each entry answers two independent
questions, asked at two different points in the session:

```toml
[env]
"*_KEY" = { mask = true, strip = false }   # a secret the tools still need
"EFFERENCE_*" = { strip = true }           # env hygiene, no masking
DEPLOY_TOKEN = { mask = true, strip = true }
```

- **`strip`** — answered at **subprocess spawn**: does the child's environment
  include this variable? It also serves determinism — a stable child env makes
  tool output reproducible.
- **`mask`** — answered at **log append**: does the log ever store this
  variable's value?

The two are a **2×2, not a scale**. Strip-without-mask is the hygiene cell
(`COLORTERM`, the harness's own self-vars): a scale would force masking onto
hygiene values and shred output that has nothing secret in it. Mask-without-strip
is the ordinary case for a credential a tool genuinely needs.

An entry may also answer a third question — **where the value comes from** — in
which case it is set into every child environment as well
([Values and references](#values-and-references)).

Keys are exact variable names or `*` globs (`*` matches any run of characters;
that is the whole syntax). Two spellings parse: the inline table, and a bare
string, which names the value (`SOME_KEY = "op://Vault/Item/field"`). A bare
*boolean* — `"*_KEY" = false` — cannot say which question it is answering, so it
is a parse error rather than a guess.

Implemented in `src/env_table.rs` (the table, precedence, the child-env policy),
`src/env_table/child.rs` (spawn replacement),
`src/env_table/snapshot.rs` (the in-memory base),
`src/env_table/mask.rs` (needles, the marker, the scan), and
`src/env_table/resolve.rs` (the scheme registry, the runner, the cache).

## Session base environment

A local creation captures the caller's environment once, before reference resolution
or provider lookup, as an `EnvSnapshot`. It retains the already-applied shell and
direnv values; it does not load shell configuration or run direnv itself.
`EnvSnapshot::new` accepts a complete map for callers preparing sessions in-process.
The snapshot is not merged with the host environment later.

Snapshots are memory-only: no serialization implementation, no state/log/discovery
field, and Debug reports only the entry count. `ChildEnv` Debug omits injected
values too.
Names/values are UTF-8; local capture fails explicitly on non-UTF-8 rather than
changing or dropping bytes. Empty names, `=` in names, and NUL bytes are rejected.

`EnvPolicy::build_snapshot` retains this base for masking and constructs the fixed
child policy from it. At each spawn, `ChildEnv::apply` clears inheritance, installs
the base, removes stripped names, then injects resolved values. Bash's session
`TMPDIR` is applied afterward. A stdio MCP
stanza overlays its own env after the table.
Context exec uses the child policy too. Reference commands use the base directly,
before table values have been resolved, and never inherit additional host values.

Later config edits reevaluate masking against the retained base, not the host's
current environment; reference commands continue using the same base and cwd.
Offline convenience constructors still capture the local environment at
construction. `control create` and the new-session form capture the caller's snapshot and send its entries
in `CreateSession` over the host's Unix socket. The host constructs an `EnvSnapshot`
from those entries before preparation; the creation response does not echo them.
Every creation route establishes a base: a request that carries `env` is used
outright, and one that omits it is established here.

## Launch environment

The user configuration may name `env_hook`: an argv, not a shell command string.
It is invoked directly, with the chosen directory set on the subprocess, so
nothing interpolates a `cd` or changes the host's own working directory. Shell
syntax requires a shell in the array, and the harness neither discovers one nor
chooses login/interactive flags. `env_hook = []` is refused rather than read as
"no hook", which would make a typo look like a deliberate choice. The chosen
directory is the session's project, on creation and on resume alike, so a session
working in a git worktree gets the environment its project gets
([worktrees.md](worktrees.md)).

The hook inherits the host environment, so a host started as a service — with
whatever environment its service manager gave it — can still reach `mise`,
`direnv`, or the user's own binaries. Reconstructing a richer environment is the
configured command's own responsibility; an operator who wants a cleared baseline
can invoke `env -i`.

Its stdout is the complete environment: NUL-delimited `KEY=value` records split
at the first `=`, which is what `env -0` writes. That output **replaces** the base
rather than merging into it, so a variable the host had and the hook did not
report is absent, and one the hook reported is present even if the host never had
it. The session's `[env]` policy is applied afterward, to whatever this returns.
A value may contain newlines and `=`; empty records (the trailing delimiter) are
skipped; the records are validated by `EnvSnapshot::new` like any other base.

Execution is bounded and noninteractive: no stdin, one 30s deadline, and 1 MiB
of retained stdout. stderr goes to null — nothing keeps diagnostics from a startup
script that may print secrets, and a discarded stream needs no reader to keep up.

The deadline covers the whole launch, reading and the exit status together, because
a hook that closes its pipes has not thereby finished. Success is an *observed*
successful exit, never an EOF: a hook may close stdout early and then exit nonzero,
and reporting that as a successful environment is the failure this ordering exists
to prevent. A nonzero exit, malformed output, an overrun, or a cancelled request
fails the launch; nothing falls back to the host environment.

The hook leads its own process group, and the group is killed on every terminating
path except a hook that exited successfully with its pipes at EOF **and** an output
that parses. A zero-exit hook that wrote something unusable fails the launch, and
its descendants do not outlive it.

The kill lives in a guard's `Drop` rather than at the end of a happy path, so a
request that is cancelled — or a runtime shutting down — still has the group
signalled, which neither an awaited timeout nor `kill_on_drop` alone covers for the
whole tree. Signalling is separate from reaping: the direct child is collected on the
paths that are still awaited, and a dropped launch leaves that to the runtime. A
process group is not containment: a descendant that puts itself in a new group is
outside what one `killpg` reaches.

A dump must be NUL-terminated: a final record with no closing delimiter is what a
truncated write looks like, and accepting it would seed a session with a partial
environment. Zero bytes with a successful exit is a valid empty environment, which
is how a deliberately cleared baseline reports itself.

The base is established fresh per launch — no session's filtered environment is
copied, and none is persisted.

With no hook the base is the host environment read directly: no shell and no
`env` process is spawned. That is the environment the host process started with —
a login shell's exports, or whatever set a service manager hands a daemon — and it
is not a loaded project environment.

`InferenceRegistry` retains the configured argv, so the hook travels with the user
configuration that session creation already loads. The host retains its own
start-time environment alongside it, which is the base an omitted `env` inherits
and what the hook itself inherits; both are taken once, at host start, rather than
per request.

Creation consults this when a request omits `env`, on every listener: a supplied
environment is used outright and no hook runs. Establishment runs while the session
is *starting*, after its id is reserved ([control-api.md](control-api.md), "A
starting session"), and its failure — a nonzero exit, unusable or truncated output,
the deadline, an unresolvable reference — is that starting session's failure, with
the reason verbatim and nothing minted. Archiving the starting session or host
shutdown cancels it, killing the hook's process group. A client that disconnects
does **not** cancel it — the deadline bounds the hook.

## Threat model: accidental capture

This guards **accidental capture** — sticky-note opsec, not evil-maid. A
deliberately exfiltrating model defeats any string matcher (`cut -c1-4`, a
base-N re-encode, an arithmetic transform); the boundary that stops that is a
real sandbox, not the harness. The same error-not-deception model
[permissions.md](permissions.md) states applies here.

It complements the spotter rather than duplicating it: **approval gates guard
the action direction** of an accident (a session about to do the wrong thing),
**masking guards the disclosure direction** (a session about to record something
it should not). Neither subsumes the other, and neither is adversary-grade.

### Known gaps

- **Values the harness process does not have are invisible.** A secret that
  lives only in a `.env` file, or that `direnv` injects into a subprocess
  without ever entering the daemon's environment, produces no needle. Naming a
  value *source* (`{ file = ".env" }`) is the conditional extension if that gap
  bites; the trigger is someone finding an unmasked secret in a log.
- **Signed thinking blocks are not masked.** The provider mints a signature over
  a thinking block's exact text and rejects a modified block on replay, so
  masking one would end the session rather than protect it. The realistic path
  into thinking is a tool result — which *is* masked — so the model rarely has
  the bytes to think about.
- **Image bytes are not scanned.** `ResultImage.data` is base64 of an encoded
  image; a substitution there corrupts the image and protects nothing.
- **A Responses `function_call` argument string is masked as JSON, and rewritten
  only when a value changed.** The string is the replay authority
  ([transport.md](transport.md)): it goes back to the provider byte for byte. The
  masker parses it, masks the values as it masks a Messages `tool_use` input, and
  re-serializes only if masking changed something — so an ordinary call keeps the
  provider's exact bytes, and a masked one replays as the logged string, which still
  parses and pairs with its result. A string that does not parse is scanned as text:
  the turn is record-only and never dispatched, and the secret must not reach disk
  either way. Nothing signs the argument string (only reasoning carries
  `encrypted_content`), which is what makes this override safe where masking a
  signed thinking block is not.
- **The Codex strip gotcha.** A tool whose credential was stripped dies with an
  opaque auth error and the session has no way to connect the two. Today the
  mitigation is documentation: if a tool starts failing to authenticate, check
  whether `[env]` strips the variable it reads. Naming the stripped variable in
  the failure is the improvement this gap is waiting for.

## Masking

### The choke point is log append

Every string about to enter the log is scanned, at
`Log::append_submission` (`src/persist.rs`) — the one function both `append`
paths funnel through. That single point covers tool results, the boot
`Preamble` (including a `[context]` exec snapshot of the environment),
`Ambient` notifications, user paste, and harness-written error text, and because
every downstream surface reads the log, one choke point secures all of them: the
wire, the jsonl on disk, the TUI, and the `control transcript`/`entries` reads.
The log holds the mask as a `PayloadMask`, a one-method trait `src/persist.rs` defines
and `Masker` (`src/env_table/mask.rs`) implements, so the log imports nothing from the
`[env]` table; a log with none installed masks nothing.

The payload paths it filters, exhaustively: `User` text blocks; `Assistant` text
blocks, tool-call `input` strings, and an upstream `excluded` mark (its `why` and, when
the empty-stream path attached them, its captured header values, `message_start`
body, and frames); `ToolResult`'s
`raw.content`; `Mark.note`; `Triage`'s `raw_keep` / `surfaced` / `retries` /
`thinking`; `Ambient`'s `source` / `body` / `slot`; `Spot`'s `reason` and
`probed_prefix`; `ConfigEdit`'s `note` and its change payload; `Preamble`'s
`identity`, `body` and its resolved `[context]` listing.

One string is masked *before* the append as well as at it: the boot preamble.
It both rides the request's `system` field and is snapshotted into the log, and
the point of the `Preamble` entry is that those two are byte-identical
([context.md](context.md)) — so boot masks the resolved `[context]` listing
and the rendered system prompt, and the choke point's re-scan is then a no-op.

### It is not a `WireState`

`WireState` ([log-and-projection.md](log-and-projection.md)) is **reversible
curation over immutable raw**: a verdict is flipped, `raw` is never overwritten,
and any redaction can be undone. Masking is the inversion — **irreversible
filtering before the log**. The secret never becomes `raw`, because a raw copy
on disk is half of what masking exists to prevent. A future change that
"simplifies" masking into a `WireState` variant would reintroduce exactly the
artifact this avoids.

Byte-determinism of `project` is untouched: the filtering happens upstream, so
by the time projection reads an entry the stored text is already final.

### The marker

A masked value is replaced by a marker that names the variable and shows a
**fixed** four-character prefix of the value:

```
⟦ANTHROPIC_API_KEY: sk-a… elided by harness⟧
```

Fixed, never proportional — a proportional prefix leaks the value's length. The
same value always produces the same marker, which is what keeps the diagnostic
pattern working: the questions a session actually asks about a credential are
"is it set?" and "does this one match that one?", and both are answered by the
marker's existence and equality without any value bytes crossing. A bare
`[REDACTED]` answers neither, and a blocked diagnostic pushes a well-intentioned
session into working around it (`echo ${KEY:0:8}`, `md5sum`) — each workaround a
fresh leak. The marker should not regress to an opaque placeholder.

### The maskability floor

Values under **8 characters** are not masked, and the variable is named (name
only) in a startup warning. Without the floor, `SESSION_KEY=dev` shreds every
occurrence of "dev" in every tool result.

### Transforms

A secret rarely reaches a tool result verbatim, so one value contributes several
needles:

- **line-wise, for a multiline value** — the top real-world miss is a PEM key
  read back through `cat -n` / `grep` / `diff`, where each line carries a prefix
  and the whole blob never appears contiguously.
- **base64 at three phase offsets** — a value embedded in a larger base64 blob
  encodes differently depending on its byte offset mod 3, so all three
  alignments are needled (the characters that mix in neighbouring bytes' bits
  are trimmed off).
- **percent-encoded** and **JSON-escaped** — the two shapes a value takes when
  it rides a URL or a JSON body back out of a tool.

### Mechanics

Needles are scanned **longest first** (so nested secrets resolve to the longer
one) in a deterministic total order (length descending, then lexicographic), in
a **single left-to-right pass** over the assembled final string. The pass
advances past each emitted marker, which is what keeps a short needle from
matching inside one — a repeated `replace` could not promise that.

Values are captured at startup; config edits reevaluate the retained base and
resolve any newly named references. The needle set is **append-only for the session**: a variable an edit stops masking
keeps its needles, because a rotated-out secret is still a secret and still sits
in files the session may go on to read. There is no zeroize window before exit —
the values are needed for the entire runtime — so the guarantee is exactly
*never written to disk, always masked at append*, and stops there.

## Defaults and precedence

Built-in entries — `*_CREDENTIALS`, `*_KEY`, `*_PASSWORD`, `*_SECRET`,
`*_TOKEN` — carry `mask = true`. Default-on is right for a safety feature, and
being *table entries* rather than code keeps them visible and overridable.

An operator entry at a built-in's key **merges** with it: an omitted field
inherits. So `"*_KEY" = { strip = true }` composes with the inherited
`mask = true` rather than disabling it — the ladder (allowed → masked →
masked+stripped) is the natural path through the defaults, not a third axis.
Disabling is possible, but only by saying `mask = false`.

**Precedence** for one variable name:

1. An **exact-name** key beats every glob.
2. Among matching globs, **more literal (non-`*`) characters wins**.
3. An unresolvable tie — two globs with the same literal count — is a **startup
   config error** naming both keys and the variable. (A caller that cannot fail
   resolves the tie to the union of the two rules, so the fallback is closed
   rather than silently empty.)

Never file order: TOML tables are semantically unordered.

**The host's credentials are built-ins too.** Every variable a host connection reads its
key from (`auth = { kind = "env", key = … }` in `~/.efference/config.toml`, disabled
connections included) joins a hosted session's table as an exact-name entry that strips
and masks (`Table::merge_with_host`, fed from `SessionInference::credential_variables`).
Those values are the host's, not the project's: the host has already read them to reach
its models, and a session's commands have no need of them. The client's environment
snapshot carries them whenever the creating shell had them — an exported key, or any
launcher run under `op run` — so without this every tool a session ran inherited the
host's model credentials. Being an exact name, the entry outranks `*_KEY`; being a
built-in, a project entry at the same name inherits what it omits, so
`ANTHROPIC_API_KEY = { strip = false }` opts a project that genuinely calls the provider
back in and keeps the value masked. Boot names the variables it withheld, since a tool
that needed one fails obscurely (the Codex strip gotcha below). The names are kept on the
policy, so a mid-session `[env]` edit re-merges them.

## Values and references

An `[env]` entry can name the variable's **value**, in two forms:

```toml
[env]
RUST_LOG = { value = "debug" }                                # literal
SOME_KEY = "op://Private/Something/credential"                # reference
OTHER = { cmd = "security find-generic-password -s foo -w" }  # the primitive
```

A **literal** is a convenience value, not a secret store: it sits in the config
file, in `ConfigEdit` entries, and in any diff of either, so it answers the
table's ordinary `mask` rules like any other variable. A **reference** stores
only a pointer in all of those places; the value lives in harness memory alone.

`cmd` is the primitive — one exec-for-a-secret indirection (the shape restic's
`--password-command`, `BORG_PASSCOMMAND`, git askpass, and docker credential
helpers all take) — and a scheme is sugar over it. A registry maps a scheme to a
command template, `op://` → `op read {ref}`, with the reference shell-quoted into
the template. Adding a vault is one registry entry.

Sniffing applies to **bare strings and registered schemes only**, so
`DATABASE_URL = "postgres://localhost/db"` passes through as a literal: nothing
runs for a scheme no one registered. The escape hatch for a literal that *does*
look like a registered reference is `{ value = "op://…" }`, which is never
sniffed.

**Masked by construction.** A value that came out of a command joins the needle
set automatically — the harness knows it is a secret, with no `*_KEY` glob
involved — and `mask = false` on such an entry does not disable it: masking
stays on and the entry is named in a startup warning. Defaults are the opposite
corner from a plain variable: `strip = false`, because injecting it is the point.
A name that is somehow both stripped and valued gets the value (removals run
before sets).

`Resolver::with_environment` fixes reference commands to the session's working
directory and base environment. The CLI supplies both before resolving the origin
table, and later config edits reuse that resolver. The runner sets child cwd
directly and replaces inheritance; it does not load shell configuration or direnv.

**Resolution is eager and cached.** Every reference resolves at boot, before the
daemon serves, and again whenever a config edit is adopted; the cache is keyed by
command string and lives for the session. That makes a vault prompt (1Password's
Touch ID) one predictable interaction at session open rather than a block inside
a turn, and it means the config edit that re-adopts an unchanged reference does
not prompt again.

**Failure is loud.** At boot, an unresolvable reference fails creation with the
reference string in the error (a pointer is safe to print). At config-edit time
the failure is announced to the session on its own notification slot, with the
rest of the edit adopted. What it is never allowed to be is silent: a value that
quietly failed to arrive surfaces later as a tool's opaque auth error, with
nothing connecting the two.

Injection, like strip, is **fixed at boot** with the spawn sites that hold it, so
a reference added by a config edit is resolved (and masked) immediately but
reaches children on the next session.

A session's inference credential does **not** ride this resolver: the host holds
it ([transport.md](transport.md)). The selected roles' secrets are registered
with the session masker by `Masker::absorb_secret` before the first append, under a
marker that names the connection and source (`⟦work (WORK_KEY): elided by harness⟧`)
with no value prefix and no minimum length — a short credential is still a
credential. They are never injected: not into the snapshot, the child environment,
the log, `[context]` output, state, or the creation response. No `[env]` variable
selects a provider or key: an `ANTHROPIC_API_KEY` there is an ordinary tool
credential the project injects, masked by the `*_KEY` glob like any other. Host
ownership does not make the snapshot secret-free — a caller's shell can carry any
project secret — and it does not sandbox an agent with the same user's shell
access; it keeps the host's keys out of what the session emits.

If neither home variable is present, cache lookup fails rather than consulting the
host account's home directory. Explicit token/account values still bypass the cache.

### Deliberately not done: memory hardening

No zeroize, no `mlock`, no core-dump scrubbing. A resolved value is needed for
the session's whole runtime — every spawn injects it, every append scans for it,
and the needle set is append-only — so there is no window in which to wipe it,
and adversary-grade hardening is outside the posture this guards
([Threat model](#threat-model-accidental-capture)). The guarantee is exactly:
*never written to disk, always masked at append*.

## The spawn sites

The child-env policy — strip, then the values the table names — is fixed at boot
and applied at every place the harness starts a subprocess:

| Site | Module |
| --- | --- |
| the foreground `bash` tool | `src/bash.rs` |
| a backgrounded `bash` command | `src/daemon/background.rs` |
| a stdio MCP server | `src/mcp/stdio.rs` |
| a `[context]` `exec` entry at boot | `src/context.rs` |

At the MCP site the table is applied *before* the stanza's own `env` map: a
server explicitly handed a variable in its own config is being given it
deliberately, and neither a blanket strip nor a table-wide value should silently
undo that. The machine root is never in an environment to remove: it lives only in the
secret store ([secrets.md](secrets.md)).

## Deliberately not built

- **File sources** (`{ file = ".env" }`) — naming a value the harness process
  does not have. The conditional extension to the invisible-values gap above; a
  `cmd` entry already covers the case where a command can print it.
- **Shape-based detection of *unregistered* secrets** (Gitleaks/detect-secrets
  rules). If ever, it would be a flag raised to the human, never silent masking:
  a heuristic that shreds output the operator did not declare is worse than the
  leak it prevents.
