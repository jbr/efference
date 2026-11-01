# Permissions — config, the fast-path & the spotter

A session is parameterized by `(config, cwd, orchestrator)`. The **config** is the
portable-policy half: what a session is *for* and what it may do, kept separate from cwd
(the per-invocation spatial scope, see [env](../src/config/env.rs) / the system preamble) so the
config stays portable. Policy is **data the classifier interprets**, never special-cased in
code — there are no privileged program names or directories in the mechanism, only config
the mechanism reads.

This doc covers what is built now: the config (`src/config.rs`), the deterministic
approval fast-path (`src/config/policy.rs`), and the independent spotter (`src/curation/spotter.rs`),
wired into the turn loop's gate (`src/agent.rs`). The watch-window and the rest of the
config-edit loop are planned; see [`../backlog/inbox/permissions/`](../backlog/inbox/permissions/).

## Threat model: error, not deception

The target is the **well-intentioned mistake** — a session that wants to succeed and
reaches for the wrong tool, directory, or egress — *not* a subverted, deceiving session.
Obfuscated bash defeats any in-harness check, so real adversary-resistance is
virtualization's job (run the daemon in a container). This boundary is
*why* the fast-path can be incomplete-but-sound. Mistakes are transparent; only malice
obfuscates, and obfuscation is out of scope.

### The axis: scope first, reversibility as the silent default

"Is this command dangerous?" is undecidable from a bash string and is the wrong question.
The question the policy decides on is **"is this move inside the mission?"** — the **scope**
is the authority, *including over how much permanence is sanctioned* (a teardown scope
clears `terraform destroy`). **Reversibility/containment** ("can the harness undo this; is
it confined?") is the **default heuristic when the scope is silent**, not the law. The
fast-path's curation rule (below) applies the reversibility default because a
`[commands]` allowlist is scope-silent by construction; the spotter, which *is* handed the
scope, judges against it directly.

### Scope vs. brief — different altitudes, kept distinct

**Scope** is the config's broad, set-and-forget **perimeter**: human-authored,
cross-session, rendered into the cached preamble, and the authority the spotter judges
against. **Brief** is the **specific session assignment** — the first user message —
ephemeral, living in the conversation, and **never in the config**. The spotter
deliberately sees *scope, not brief*: its value is judging against a fixed external
authority, not the session's own narration of what it is doing. The two words are not
interchangeable.

## The config (`src/config.rs`)

A TOML document, decoded through [`toml_edit`]'s serde layer into a raw shape and then
**validated** into the materialized [`Config`] the cascade consults. A malformed file
surfaces a `toml_edit::de::Error` whose `Display` renders the offending line — the
load-time diagnostic that stands in for KDL's old editor tooling. The same crate edits the
file in place ([`DocumentMut`]) for the auto-persist write-back (below), so one dependency
both reads the config and writes the harness's edits back with comments intact.

**Per-session snapshot (the config is a reviewed file).** At boot the origin (`--config <path>`,
which must exist; else the `efference.toml` probe; else a scaffolded empty file) is **snapshotted once**
into the session's own `.efference/<id>/config.toml`. That snapshot — not the origin — is the live,
watched, editable policy: two sessions sharing an origin diverge freely (the origin is an inert
template). Editing that file is how the session **requests a permission change** — but a
`write`/`edit` to the config file is forced to the operator gate (`policy::is_config_write`),
bypassing both the fast-path and the spotter, so a permission change can never auto-apply; on
approval the write lands and fs-watch folds the diff into effective policy. The config path is
**not** named in the preamble (that up-front mention drew sessions into probing their own perimeter
as a first move — a dogfood regression); the session learns the path from the gate-rejection
message when a move actually strays, keeping the perimeter out of attention until it's relevant. A
malformed edit fail-closes *and* is reverted, so the file is never left unparseable (below).
"Session-only" is thus a matter of geography, not a flag — the file is per-session, and every
committed grant is written to it.

`GET /state` reports the resolved source *and the full effective policy in force* (scope, watch-for,
commands, tools, paths) — effective, not boot, so a live config edit (a session grant, an fs-watch
file change) is visible there; an orchestrator reads a subordinate's live perimeter from it.

The schema is **string-keyed maps**, not lists — because the policy is a *table* the
config-edit loop mutates by key (below). The config is a materialized projection of the
file plus the log's config-edits:

```toml
scope = "Work only inside this repo."
watch-for = ["network egress to non-registry hosts"]

[commands]
cargo = true            # blanket — key lexes to ["cargo"]
"git diff" = true       # sub-verb — key lexes to ["git", "diff"]
git = "clear"           # the stance vocabulary; `true` is sugar for this
"git push" = "deny"     # a carve-out: the longest matching key governs

[paths]
"." = true            # sugar for { read = true, write = true }
"/refs" = { read = true }

[tools]
efference = { enabled = true }
"efference.read" = { granted = true }
"efference.write" = { granted = true }
```

For now there is **exactly one config file per session** — no built-in base, no merge.
"Merge by copy-and-edit" holds until usage patterns settle the real merge semantics; the
`extends` ancestor pointer is parsed but the merge engine is deliberately unbuilt. What
the config holds:

- **`scope`** — the positive perimeter, *shared with the session*. It projects into the
  cached preamble (`Config::preamble_section` → `env::system_prompt`). The projection must
  be **byte-stable for the session** (the same caching invariant as `project(log)`, see
  [substrate](log-and-projection.md)): it is rendered once at startup from the scope text and
  frozen. A live, mid-session scope edit must **not** re-render it (that would mutate behind
  the cache breakpoint); instead it surfaces as a notification (see the config-edit loop).
- **`watch-for`** — the "things to watch for" list, *spotter-only*. Kept off the session's
  preamble for **attention-division** (not anti-gaming — the threat model is error, not
  deception, so there's no test to game): the focused session shouldn't carry low-probability
  peripheral hazards in working memory; the spotter holds them. Consumed by the spotter rung.
- **`[commands]`** — the bash prefix policy, a **map** from command-prefix key to stance (below).
- **`[tools]`** and **`[paths]`** — which tools the session mounts and which it may call
  without review, and the filesystem-grant map the fast-path reads (below).
- **`[env]`** — per-variable `strip` (does a spawned child see it?) and `mask` (does the
  log ever store its value?). Its own subsystem: [env.md](env.md). It sits beside the
  cascade rather than inside it — the gates guard the *action* direction of an accident,
  masking the *disclosure* direction.
- **`trust-threshold`** — where the watch-window/gate cutoffs will sit. Parsed, unused.

**Two-layer decode, fail-closed.** Serde reads the file into a raw struct
(`deny_unknown_fields`, so a typo'd key is a loud error), then `validate` folds it into the
`Config`: every `[commands]` key is **lexed with the fast-path's own lexer**
(`policy::simple_command_tokens`) so the stored key and the command it's matched against
agree token-for-token, and a key that isn't a single simple command (`"git | rm"`) **fails
the whole file** — no silent skip. A carve-out key is a multi-token prefix (`"git push"` lexes
to `["git", "push"]`), so the same check that admits a sub-verb grant admits a sub-verb deny.
The value vocabulary is closed — the four stance words plus the `bool` sugar — so a misspelled
stance is a parse error rather than a silently-ignored entry.

## Ahead of the cascade: the call has to name something (`src/agent/step.rs`)

Two calls never enter the cascade at all, because neither poses a permission question.
A triage tool (`retain`/`triage`) called when nothing was asked of it is paired with the
nudge result ([triage.md](triage.md)). And a call naming **no tool this
turn advertised** is answered with `tools::unknown_tool_message` — the mounted names, plus
a pointer at `bash` for a name that is a program — as a `Direct` payload, so the correction
reaches the wire verbatim rather than through the triage.

The membership test reads the names off `agent::tool_definitions` itself, minus the triage
tools, so what the check accepts and what the request advertised cannot drift.

The check is ahead of the rungs rather than inside one because the call cannot run whatever
any rung answers. Asked anyway, the spotter answered: it returned `clear` for a call naming
a nonexistent tool, with a rationale invented for the call it imagined ("reversible query
that doesn't modify state or escape the repo"), and `spotter-log` counted that as a clean
clear. A rung that judges intent against `scope` and `watch-for` has nothing in its inputs
that says the target does not exist.

## The fast-path (`src/config/policy.rs`)

`classify(&Path, &Config, &ToolCall) -> Verdict` is a **pure function** — no I/O, no model —
returning `Clear` (auto-approve: no human gate, no model round-trip), `Gate` (fall
through to the spotter rung, then the human gate), or `Deny` (skip the spotter and go
straight to whoever gates). The loop consults it just before the gate: `Clear` becomes an
immediate approve; `Gate` hands the call to the spotter (below); `Deny` routes to the human
gate, or, autonomous, to the same redirect a spotter `Gate` takes there — `agent::denied_gate_message`
names the config rather than a spotter verdict nobody gave. `Deny` is reachable only from a
`[commands]` stance (below); everything else the fast-path is unsure of is a `Gate`, and it
only ever *accepts what it is certain about*.

The first argument is the session's immutable cwd. The loop passes the same root
to classification, config-write detection, missing-grant diagnosis, and bash
tokenization; probe-ledger replay also uses it. `config::load_in` resolves a
relative template path against an explicit root while preserving the distinction
between an optional missing default and a required explicit config file.

### bash: a two-stage check; the order matters

Stage 1 (structural simplicity) runs **before** stage 2 (prefix match), so a trusted
`git diff` prefix can never clear `git diff > /etc/hosts` or `git diff $(rm x)` — those die
in stage 1, before any prefix is consulted. This ordering is what makes a blanket
`git = true` survivable in principle and a sub-verb `"git diff" = true` sound.

- **Stage 1** parses the command with `brush-parser` — a real bash parser, not a regex —
  and accepts only a *single simple command*: no operators (`| & ; && ||`), no *file*
  redirection, no command/arithmetic substitution, no parameter/tilde expansion, no
  env-assignment prefix, no unquoted glob/brace. The one redirection it tolerates is a pure
  file-descriptor duplication/closure (`2>&1`, `>&2`, `2>&-`) — those rewire the process's
  own fds and touch no filesystem, so they're sound, and admitting them keeps the habitual
  `cmd 2>&1` on the fast path (without it a trailing `2>&1` knocks blanket `cargo = true` off,
  as a dogfood session found). A `>&` whose operand is a *filename* or an expansion is not a
  pure dup and still gates. The parser is needed (not a regex)
  because safety is quote-sensitive: the `|` in `rg 'a|b' src/` is one literal argument,
  not a pipe. brush *parses* without *expanding*, so we inspect word pieces ourselves for
  the expansion metacharacters bash would act on at runtime (`* ? [ {` anywhere; a leading
  `~`) — which is why `git diff HEAD~1` clears but `~/x` and `cp {a,b}` gate.
  The one operator it tolerates is a single leading **`cd <dir> &&`**, whose remainder becomes
  the command stage 2 matches. The `cd` segment must be exactly two words with a *literal*
  target (the same word-unit inspection), and that target — resolved against the cwd by the
  same lexical rule the `[paths]` check uses — must sit under a `[paths]` grant carrying
  **read**: `cd` mutates nothing, so read is the permission the move needs, and a write grant
  satisfies it. Anything else fails closed: an ungranted or expanded target, a second `cd`, a
  different operator (`;`, `||`, `|`), more than one operator, or a remainder that is not
  itself a simple command. Same reason as the fd-dup tolerance — models emit `cd <abs> &&` on
  nearly every bash call (45.6% of a 742-call local corpus), and without this each one pays a
  spotter round-trip whatever `[commands]` granted. The tolerance is in the bash-call lexer
  only; the `[commands]`-key lexer stays single-simple-command, so a `cd …` compound can never
  be stored as a grant.
- **The redirection carve-out.** A stage 1 rejection is not always final. One shape gets a
  second look: a **write to a single literal path** — `cmd > path`, `cmd >> path`,
  `cmd >| path`, optionally fed by a here-doc (`cat > path <<EOF`, `<<'EOF'`, `<<-EOF`). That
  is a `write`, so it is governed as one, by the `[paths]` write grants. A granted target
  clears on the fast path with no spotter round-trip; an ungranted one gates exactly the way
  the structured `write` gates.

  This exists because the heredoc is what a model reaches for when `edit`/`write` doesn't go
  through: the harness then loses the reviewable diff at the moment review matters most
  Governing the bash form as the same write means
  routing around the tool changes nothing about the answer.

  **Two authorities govern and the more restrictive one wins.** The command must still clear
  stage 2 on its own — `cat` needs a `clear` stance — *and* the target must be write-granted.
  A `[commands]` `deny` refuses whatever the path says; a `clear` command writing outside a
  write grant gates. The `[tools]` table is not consulted: it names structured tools, and the tool
  being called is `bash`, whose name grant is its `[commands]` stance. Composed with the `cd`
  tolerance, the target resolves against the **`cd`'s** directory, since that is where the
  command runs. The self-surface stack needs no special case — it already denies writes under
  `.efference/`, so a here-doc aimed at a session's own `config.toml` or log fails the path
  check like any other ungranted target.

  The admitted shapes are exhaustive and everything else fails closed to the spotter as
  before: an expansion or glob in the target, more than one file redirection, an input
  redirection or here-string, a non-stdout fd (`2> file`), a here-doc whose content contains
  any expansion or substitution (an unquoted here-doc *runs* `$(…)`, which brush surfaces as
  a substitution piece), pipes, process substitution, lists, backgrounding, negation, assignment
  prefixes, and anything that does not parse. It is a governed exception, not a general
  loosening of stage 1's rejection of file redirections.
- **Stage 2** matches the lexed `[program, args…]` against the `[commands]` map: each key
  is **re-lexed with the same lexer** and compared token-wise (so `"git diff"` matches as
  `["git", "diff"]`, not by string), an empty-args key (`cargo`) addressing the whole
  program. Re-lexing a handful of keys is free next to parsing the command itself. The
  stance of the **longest** matching key is the answer (below).

**The hand-off's completion.** An `agents__archive` call that names the caller's own
predecessor clears on the fast path, ahead of the rungs below it, with no spotter round-trip
and no `Spot` entry (`judge` in `src/agent/step/judge.rs`, `Toolbox::completes_hand_off`). A hand-off
is a conversation, and only the successor knows when no clarification remains, so its
archiving the predecessor is the completion the successor's brief tells it to make
([tools.md](tools.md), "Hand-off"). The rule reads the host's succession record, written
at `hand_off` and never taken from the call, so it cannot be pointed at a sibling, a
descendant, or the predecessor from any session but its successor; those go through the
ladder as any `archive` does. The record is host state and is not restored on a resume
or host restart, so across one the archive goes to the spotter as before. The archived log
stays on disk and resumable.

### `[commands]`: the stance vocabulary, resolved longest-prefix-wins

A `[commands]` value is a **stance**, spelled with the same four words the spotter's verdict
uses, so one word means one thing at both rungs. `true` and `false` are sugar (`true` = `clear`,
`false` = `deny`), which is why every config written before the vocabulary keeps parsing and
keeps meaning what it meant.

- **`clear`** — auto-approve at the fast path: no human, no spotter round-trip.
- **`watch`** and **`gate`** — send the move to the rung that gates it. They are distinct
  parsed values with identical behavior today: `Stance::Watch` collapses to the human gate
  until the watch-window lands (see below), and the config words inherit that collapse. When
  `watch` gets its own intervention latency, these diverge without a config migration.
- **`deny`** — refuse *without consulting the spotter*: the move goes to whoever gates (the
  human, or the autonomous redirect) with no model call. A redirect the loop writes itself
  is recorded as the permission review's decision (`review[autonomous]`), so neither the
  model nor a supervisor reads it as the operator's (docs/log-and-projection.md). Nothing the spotter could answer
  would resolve below the human gate, so the round-trip would be spent for nothing.

**Resolution is longest-prefix-wins**, the way `[paths]` already resolves a filesystem grant:
among the keys whose tokens are a prefix of the command's, the *most specific* (longest) one
governs. So `git = true` plus `"git push" = "deny"` clears `git log` and stops `git push
origin main`, and the inverse (`git = "deny"`, `"git log" = true`) works the same way —
trust-broad-then-carve-exceptions, without deleting the broad grant and re-listing every safe
sub-verb. Nesting is unbounded: an allow can sit inside a deny inside an allow. Two keys at
equal token depth (only reachable through two spellings of one prefix) resolve to the more
restrictive stance.

The resolution reads the token vector stage 1 produced, so a carve-out governs whether or not
the move arrived with a tolerated `cd <granted dir> &&` in front of it: the `cd` is stripped
before stage 2, and `cd /work && git push` hits the same deny `git push` does. An *ungranted*
`cd` target still fails stage 1, which gates rather than denies — stage 1 has no opinion about
the remainder.

Prefix-promotion (below) files **clear** grants only: `ConfigChange::Command` spells the grant
an approval hands out, and no initiator hands out a restriction. Two consequences. The
write-back writes the `true` sugar, so a promoted grant round-trips through the file as `clear`
and leaves neighbouring stances untouched. And a promotion is dropped when *any* key already
governs its prefix — under a governing `clear` it would be a no-op, and under a `watch`/`gate`/
`deny` it would punch a hole in a carve-out the operator wrote. A non-clear stance is written
in the file and read at load; because `ConfigChange::Command` cannot carry one, a carve-out
*added* by an external file edit takes effect on the next boot (which reloads the file as the
base) rather than live — the same frontier of the additive fold that a deleted key sits behind.

**Why a bash parser, and which one.** The gate's soundness never rests on parser
*coverage*: any shape outside the admitted list — including anything that fails to parse —
gates, so an incomplete parser costs spotter round-trips, not safety. What it does rest on
is *fidelity where the gate says yes*: the parser must not read something bash would expand
or execute as an inert literal word. A parser of the same dialect bash speaks (`[[ ]]`,
`<(…)`, `&>`, `$'…'` all surface as typed nodes to reject rather than as words) keeps that
gap small. `brush-parser` is the parser of the actively-maintained brush shell, so it
tracks bash and keeps receiving updates. It replaced `yash-syntax`, which was POSIX-strict
and had to be pinned to the last standalone version (later versions pull in the whole yash
runtime, `yash-env`) — a pinned 0.x parser on a security-critical path gets no security
updates, the worse trade. Only the parser is used — we parse, bash executes.

### the curation rule (config, not code)

The allowlist is curated along the reversibility axis: blanket `cargo = true` only for
programs whose *whole* arg-space is recoverable; sub-verb prefixes (`"git diff" = true`,
`"git status" = true`) for programs with an irreversible sub-space, letting the dangerous
verbs (`git push`, `git reset`) fall through. Longest-prefix resolution gives the same
curation a second shape — `git = true` with `"git push" = "deny"` names the exceptions
instead of the exceptions' complement — and which shape is honest depends on whether the
irreversible sub-space is enumerable. Memoization (the prefix-promotion growth of this
list, below) keeps the list sound through **exact token-prefix matching (never fuzzy) +
recurrence + judge-consensus**, not a granularity cap. Earlier this doc said "program-prefix
granularity, never command granularity"; prefix-promotion revises that deliberately. The two
original fears both dissolve: *dead entries* (exact commands rarely recur) — the frequency gate
only promotes shapes that actually recur; *discontinuity* (`./build` vs `/build`) — that only bites
*fuzzy* matching, and promotion is exact token-prefix (promoting `./build` clears `./build …`,
never `/build`). So depth is unbounded; the descent self-terminates at the shortest safe prefix.
See the prefix-promotion section below.

### structured tools: name grant + path permission

A structured (non-bash) tool clears only if the config's `[tools]` table **grants** it
**and**, for the path-bearing ones, its target resolves under a `[paths]` grant with the
needed permission — reads (`read`/`list`/`search`/`find_anchor`) need `read`, writes
(`write`/`edit`) need `write` (`write` implies `read`). Nothing clears in code; a config with
no `[tools]` grant gates every tool.

#### The `[tools]` table: enabled and granted

```toml
[tools]
efference = { enabled = true, granted = true }
agents = { enabled = true }
"agents.message" = { granted = true }
"efference.bash" = { enabled = false }
ferritin = { granted = true }     # a server declared under [mcp.servers]
```

A bare key names a **namespace** and a quoted dotted key names **one tool**, split at the
first dot (a server's own tool name may contain dots; server names may not). The namespace is
the `<server>` half of the wire name, resolved at the **first** `__`; the harness's own
embedded tools have no prefix, so they sit in the `efference` namespace and are written
`efference.<tool>` here (the key `bash` alone would name a namespace, and fails the file with
a message naming `"efference.bash"`). The session-control verbs sit in `agents`, and the language-server tools in `lsp`
([tools.md](tools.md)). A key is not
a pattern: a `*` fails the file. `config::tools::validate_tools` resolves each key to a
wire name, so the table the fast path reads holds one key per tool however the file spelled
it, and the write-back writes it back as the dotted key the session drawer, the transcript,
and the file all show.

Each entry sets either field, and a tool's entry overrides its namespace's **field by
field** (`ToolsTable::granted`, `Config::tool_enabled`):

- **`enabled`** — whether the session mounts the tool (docs/tools.md). efference's own
  namespaces default to **off**, so a built-in reaches only sessions whose config names it;
  a declared server's tools default to on, the declaration being the request. Read once at
  boot, when the toolbox is mounted: `tools[]` is fixed for the session
  (docs/log-and-projection.md), so a live edit to `enabled` takes effect at the next boot. The
  loop's own tools (`consolidate`, `ask`, `wait`, `check_inbox`, the process tools) are always
  mounted and refuse `enabled`, since the loop asks for them.
- **`granted`** — the fast path's name grant. Live: a gate grant, a committed proposal, or a
  file edit grants a tool or namespace mid-session ([`ConfigChange::Tool`], whose `name` is a
  wire name or a namespace as `<namespace>__*`). A grant on a tool that resolves to not
  enabled fails the file: it would clear nothing and read as if it did.

**What a namespace grant claims is trust in the *party*, not in an enumerated set.** A
blanket `cargo = true` is survivable because of stage 1: the structural check runs before any
prefix match, so a trusted prefix can never clear `cargo … > /etc/hosts`. A structured call
has no stage 1 — there is nothing between the grant and whatever JSON the server interprets —
and the granted set is defined by the other party and can grow: a server's table is frozen at
mount and may differ at the next one. So `ferritin = { granted = true }` means "anything you
can do with this server is ok — if it adds a new tool, that is allowed too." A server not
trusted to that degree should not hold a namespace grant; the harness does not constrain it
to the mount-time table, because a grant that needed that constraint is a grant that
shouldn't have been written. It is never the default on mount — a grant is an explicit config
act, and defaulting it would move the perimeter every time a server is added. The same
reasoning is why `enabled` defaults off for efference's own namespaces: a built-in added
later in a namespace of its own reaches no session that did not ask for it.

**Where a namespace grant sits relative to `[paths]`.** It is a *name* grant, so the second
check is untouched: a path-bearing built-in under a granted `efference` still clears only
inside a `[paths]` grant, which is why a whole-namespace grant over the built-ins has a
bounded blast radius. A connected tool's does not — the fast path knows nothing about what a
remote tool touches, so `ferritin = { granted = true }` is bounded by the grant alone.

**Multi-path clearance is per-path, unioned across sources.** A tool naming several paths
(`read`'s `paths` array) clears only when *every* path is granted — but each path may be
granted by a *different* source: a `[paths]` grant (which requires the name grant) **or** a
`[context]` listing (standalone, below). So a batched `read` mixing a repo file (under a
`[paths]` grant) and a memory file (under a `[context]` entry) clears, even though neither
source covers the whole batch alone — the check is over the union, decided path by path. One
ungranted path still poisons the batch. (An earlier version evaluated each source
all-or-nothing over the whole batch, so a cross-source read gated; a dogfood session caught
it.)

Two properties matter here. **Resolution is longest-prefix-wins, so a nested grant
carves an exclusion out of a broader parent.** Among the `[paths]` grants whose subtree
contains the target, the *most specific* (deepest) one governs; a no-access grant (`false`
sugar / `Perms::default()`) is a real **deny** once it is the governing prefix. This is what
lets the standard three-layer stack work: allow `.`, deny `.efference/`, re-allow
`.efference/<id>/config.toml` and `.efference/<id>/attachments/` read-only. The harness
**injects that self-surface stack in
boot** (`src/boot.rs`), layered over the operator's grants: the session dir holds this
session's control socket, its append-only log, and every *other* session's dir, all under the
operator's `.` grant — so without the deny nothing would stop a session from writing its own
log (corrupting the session record) or POSTing to its own control socket (approving its own gates,
injecting a user turn). The attachment re-allow is what lets the model pull a file the operator
attached ([inbox.md](inbox.md)) without a gate. The `config.toml` re-allow is the sanctioned scope-expansion path: the
session reads its own perimeter and *edits* the file, a write the governing read-only grant
refuses at the fast-path, so it gates for the operator. Injected keys are inserted *after* the
operator config, so a safety deny overrides an equal-specificity operator grant; an operator
can still override by naming a *strictly deeper* path. Because bash never touches the fast-path,
the deny layers are also rendered into the **spotter context** (`spotter.rs`), so a `curl
--unix-socket .efference/host.sock` or `echo >> …/log.jsonl` gates on scope grounds there
too. Path matching is lexical (`..`/`.` collapsed without touching the filesystem, so it works
for not-yet-existing write targets), which means a symlink inside a grant pointing outside is
treated as inside — acceptable under the error-not-deception model. For the same reason a path
carve-out is a **mistake-guardrail, not access control**: a symlink or a `cat ./sec*` through
bash reaches around it, so a deny here keeps a well-intentioned session out of a directory and
claims nothing more.

**Two anchors.** Grant keys are written in the project's `efference.toml` and resolve
against the project; the paths a call names resolve against the session's working
directory (`policy::Directories`). The two differ only for a session in a git
worktree, and a target inside any worktree is judged as the same path in the project,
so every worktree carries exactly the project's grants ([worktrees.md](worktrees.md)).

**No privileged client, enforced by reachability.** A session must never hand-roll
control-channel HTTP from bash; every legitimate control interaction is a tool. The deny
layers above are what enforce that, and the rule is written to cover a session's *own*
socket, a *peer's*, and a *parent's* uniformly — so the property survives into multiagent
topologies without a second mechanism (see
[`../backlog/inbox/multiagent/`](../backlog/inbox/multiagent/README.md)).

Boot injects one more grant alongside the self-surface stack: the per-session **scratch
dir** ([tools.md](tools.md)) is always granted rw. It lives outside `.` and outside the
session dir, so it interacts with neither the operator's grants nor the deny; structured-tool
paths reach it via `$TMPDIR` expansion (resolved *before* classify/spot, so the grant check
runs on the expanded literal), bash via the child env, and the spotter's scope block renders
the `$TMPDIR` → path alias.

**Mapping a tool to its path field is a deliberate wart.** The classifier reaches into
each built-in's JSON input by known key (`read`→`paths`, `write`→`path`,
`edit`→`file_path`, …); the string literals are centralized in `policy.rs`. The
sharp case: `undo_edit` writes to disk but carries *no* path (the target was named by an
earlier `edit`), so write permission is enforced at `edit` — the only
edit-pipeline call that names the file — and `undo_edit` clears on the name grant. The
cohesive fix (an MCP-superset that carries permission/notification metadata alongside the
schema) is later architectural work, and the coupling is marked rather than hidden.

### the `[context]` rung: a listing is a grant

A `[context]` entry ([context.md](context.md)) is an independent clearance source, unioned
per-path with the name-grant rule (above): listing a path **is** a read grant — the preamble
stub invites the model to `Read {path}`, so that read fast-path clears with no `[tools]` or
`[paths]` config at all. The same lexical subtree rule applies, and the caveat is stated loudly: a
location entry (dir, and later glob) read-grants everything under it, so operators must
list deliberately. The entry's write grants ride the same check: `update` clears a write
to an **existing** file under the entry, `create` a write to a **new** one — `create`
without `update` is an append-only store. Telling create from update is the classifier's
one filesystem touch (an existence probe) — sound here because a verdict is derived state,
never on the wire. The context rung covers only the structured path-bearing tools; bash
never clears through it. A write to the session's own config file still bypasses
everything to the human gate.

The guide overlay's reserved prefix ([context.md](context.md)) rides this rung as a
standing grant: an `efference://` target is **read**-granted with no config entry at all —
the documents are the recovery path for a session whose config knows nothing else — and is
never write-granted, since there is nothing on disk to write and a harness authorizing
itself over its own documents would be the self-authorization the config design forbids. It
is also excluded from `[paths]` resolution outright, because joined onto the cwd it would
look like an ordinary relative path and a broad grant would then appear to cover it.

## An allowing `agents__decide` is classified as the action it authorizes (`src/agent/step.rs`, `src/protocol/agents.rs`, `src/control_tools.rs`)

A session's `agents__decide` carries only a target, the held call's transcript handle
(`seq`), and the decision, so classifying an allow as written would ask every rung to
judge a handle. Before the cascade runs, the loop asks the mounted sessions provider to
bind an allowing call to the target's held move (docs/tools.md): the host reads the
target's pending gate, and while the held call is itself an allowing `agents__decide` it
follows that call's `agent`+`seq` to the next session, refusing on a cycle. The result is
a `BoundAction` (`src/protocol/agents.rs`) — the leaf tool, the child's input, its intent, its cwd, and the chain of
session ids from the directly-targeted child down to the session holding the leaf. A seq
that is not the target's pending gate is refused before classification with a "gate
moved" error result, and the target rules the Host executor already enforces (self never,
supervised sessions only) apply to the binding, each hop to the previous
session: a held decision is followed only into a session its holder supervises, or whose
held call was escalated to its holder (below).

**A deny or an escalate authorizes nothing.** A denying `agents__decide` — a redirect or
an abort of the child's held call — and an escalating one, which leaves the call held, are
not bound, and clear on the fast path with no spotter request and no `Spot` entry, the way
the hand-off's completion does (`Toolbox::authorizes_nothing` beside
`Toolbox::completes_hand_off` in `judge`). The verb still enforces its target rules when
it runs.

The parent's ladder then decides an allow on the leaf:

- If the parent's `[tools]` table grants `agents__decide`, the call clears blind. That
  grant is the config's statement of trust in the coordinator, and the child's move is
  not re-litigated.
- Otherwise `policy::classify` runs on the leaf call, under the parent's effective
  config, with the leaf's cwd. A `[commands]` grant the parent holds clears a matching
  child command; an ungranted leaf gates.
- The spotter is handed the leaf — its tool brief and the approval chain — and answers
  the same scope question, not one about the approval call.
- The human gate shows the child and its held move (`acting_on`, docs/control-api.md)
  instead of the id.

The prefix-promotion probe does not ride a bound approval: the leaf's command belongs to
the child's own probe ledger. The logged `ToolCall` is untouched throughout — the wire
carries what the model wrote, and the binding lives only in flight and in the ephemeral
gate view.

## The spotter (`src/curation/spotter.rs`)

What the fast-path can't `Clear` falls to the **spotter** before it reaches the human — a
one-shot, **fresh-context** model call (the provider's cheap band by default, on its own
`Client`, model-independent of the acting session) that re-decides the residual. Independence is the point: the
acting session can't check its own folly because it shares its own trajectory; the spotter
can, precisely because its context is built from scratch — *not* `project(log)`.

Which model that is resolves in order: an explicit `--spotter-model`, then
`[defaults].spotter`, then the main connection's own `spotter-model`, then **jev on any
connection that reaches it** — the first `typesafe` connection by name (`jev-latest`), else
the first OpenRouter one (`~typesafe/jev-latest`) — then the main arm's built-in default
(`Provider::default_spotter_model`). The per-connection key sits below `[defaults]`
because it answers about *one* account's cheap band, which an account-wide default cannot;
it sits above jev because a key someone wrote outranks one inferred from which providers
are configured. Typesafe comes ahead of OpenRouter because OpenRouter's decisions route is
an alpha. The arm's fallback is last because it is a guess about a catalog the harness no
longer compiles — one id, and a stale one now costs a pricier spotter rather than a refusal
(docs/operating.md). `preview` reports a jev default's source as `decision_connection`.

Boot checks the spotter's id before any gate needs it. An inference model is looked up in
its provider's capabilities; a decision model has none, so it is checked for existence
instead — the first-party listing, or OpenRouter's per-model endpoints route, which answers
404 for an id it does not serve (`Endpoint::check_decision_model`).

### Two rungs behind one trait

`Spotter::assess` has two implementations, and which one a session mounts is decided once
at boot by `MountedSpotter::mount` (`src/curation/spotter/mount.rs`) from the `ModelKind` that
`Endpoint::spotter_kind` reports for the spotter selection:

- **`ForkSpot`** — the prose rung, described in the rest of this section. A generative
  model reads a rendered persona + scope + move and answers with a stance and a sentence.
- **`JevSpot`** (`src/curation/spotter/jev.rs`) — the **decision** rung. A decision model reads a
  JSON state and answers typed questions — yes/no, a choice among options, a score on
  described levels — with a probability for each answer
  ([transport.md](transport.md): the decisions route). Selected when the spotter model is a
  decision model.

The kind comes from what the provider says about the model, not from its id. A provider that
serves no turns (`typesafe`, a connection declared `dialect = "systemone"`) lists only decision
models. OpenRouter's per-model metadata route (`GET /api/v1/models/{author}/{slug}/endpoints`)
declares `architecture.output_modalities` — `decisions` for a decision model, `text` otherwise —
and that one GET is also the existence check, so any decision model OpenRouter lists mounts
this rung, `typesafe/jev-1.13` and `upstage/solar-decide` alike, and a model released after
this build needs no new code. Every other provider serves text models only. The same fact keeps
a decision model out of the turn roles: main and triage refuse it at boot with a message naming
the spotter as the role it can back.

Three things change with the rung, and they are why it is a second implementation rather
than a model swap:

**The judgement is decomposed, and part of it is precedent.** A decision model answers
fixed questions with probabilities, so the stance is a fold in code
(`src/curation/spotter/jev/questions.rs`) over two requests sent concurrently:

- **Cold** — the state above, asked `sketch` (a Score: how concerning the move is, five
  described levels from routine to severe) and the axes `in_scope`, `path_outside`,
  `path_denied`, `exposes_secrets`, plus `watch_for_hit` when the config names one and
  `prefix_safe` when a promotion probe rides along.
- **Relation** — the move alone, as plaintext, with one Choice per **precedent
  candidate**: an earlier call in this session of the same family (the same tool, or for
  bash the same program) that ran with the operator's consent, inlined into the question
  rather than referred to by index. The options are `same` (differs only in which file,
  range, term, or record), `wider` (goes further: writes where it read, sends where it
  fetched, a different host, account, or directory), and `different`. Each is asked in
  both option orders, because a decision model leans toward the first option, and the two
  are averaged (`src/curation/spotter/jev/relation.rs`).

**Consent** is read off the log (`src/curation/spotter/precedent.rs`): a call that ran and either
reached no spotter — the fast path, a granted tool, the human under `human_only` — or was
held by the spotter and then approved. A call the spotter cleared is not consent, so a
clear never becomes the evidence for the next one. A bound leaf (`agents__decide`) has
none: the child's history is not in this log.

The fold:

```
level  = the sketch Score's most probable level (0–4),
         raised to at least 3 by path_denied > 0.25, exposes_secrets > 0.15,
         or watch_for_hit > 0.25; to at least 2 by path_outside > 0.35 or in_scope ≤ 0.30
Gate   if exposes_secrets ≥ 0.5            — no precedent clears a printed secret
Clear  if precedents ≥ REQUIRED[level]     REQUIRED = [0, 0, 1, 2, never]
Gate   otherwise
```

where a precedent is a candidate read as `same` at 0.5 or more. `ClearAlways` is a clear
with a probe and `prefix_safe ≥ 0.8`. The rung never emits `Watch`: a move has the
precedent its level asks for or it does not. A shipping axis that trips raises the level
instead of gating outright, so an operator who has approved a read outside the grants
once has said what they mean, and the next read like it clears.

Two things the fold leaves out on purpose. **Operator turns are not read**: questions
about the session's history (was this asked for, does it go against something the
operator said, would they approve) separated approved holds from routine clears no better
than chance in the corpus, so what the operator said reaches this rung only through what
they then approved. And **a declined call does not veto** the moves like it: the corpus's
redirects were steering ("use the docs tool instead"), and letting one veto its family
held calls the operator went on to approve.

**The reason is synthesized.** A decision model emits no prose, so the one line the acting
session and the human gate read is written from the level, what raised it and the figure
that did, and the precedent found against the precedent required — and, when a candidate
read `wider`, how many approved calls the move goes further than:

```
routine (level 0).
level 2 (outside the grants, 62%): needs 1 approved call like it, found none; goes further than 2 calls the operator approved.
level 3 (sketch, 99%): needs 2 approved calls like it, found none.
```

It states what the model reported rather than a conclusion it never wrote. That line is
addressed to the acting session and the operator's gate, in the rung's own terms. Whoever
*decides* a held call through a status — a supervising session reading its child's gate,
docs/tools.md — reads a second phrasing, `jev::plain_reason`, composed beside `decide` from
the same recorded answers (the axes and the precedent reading on `Spot::axes`), never by
rewriting the string: the clause for what put the move at its level (the tripped axis, or
the sketch level by its description), then what the precedent reading said, with no level
numbers or figures:

```
its reviewer is unsure this fits the session's scope; no earlier call like it was allowed
its reviewer rates it serious: hard to undo, or reaching other people; only 1 earlier call like it was allowed, and a call like this needs 2
it reaches outside the paths this session is granted; no earlier call like it was allowed; it goes further than 2 calls allowed earlier
it may expose secret material
```

The status shows a prose-rung reason as written, and a fail-closed verdict as "its reviewer
could not judge it" (`held_because`, `src/session/status/view.rs`). A `Spot` whose relation
request failed has no precedent reading, and its phrasing names the cause alone. `Spot::reason`
is unchanged by any of this.

**Every reading is recorded.** `Spot::axes` carries the cold answers (the sketch
distribution among them) and the precedent reading — candidates asked, how many read
`same` and `wider`, how many were required, and each candidate's call id with its two
figures, so a clear can be traced to the approved call it matched and a different cut
replayed — so a live session's verdicts join the
calibration corpus as they happen (`eval/spotter/extract.py` carries them as
`recorded.axes`). The stance is no longer a function of one spot's numbers: precedent
depends on what the operator did before it, so re-scoring a policy means replaying
sessions in order. `eval/spotter/battery.py` asks the questions over recorded spots and
`battery_replay.py` replays a policy over the answers with no further requests. A
`--persona` against a decision model is refused: it has no prompt to replace.

What the corpus showed, and how each piece of this was chosen, is in
`journal/2026-10-08-precedent-spotter.md`: over 211 recorded spots in the families
examined, the band fold this replaced held 134 and this fold holds about 40, while missing
1 of 33 authored must-hold cases against the band fold's 1. The same fold and thresholds
carry to the other decision models OpenRouter lists, within a few cases of jev.

Fail-closed is unchanged on both rungs. A transport error, a missing answer for a question
that *was* asked, or an unparseable verdict all return `Origin::Fallback` + `Stance::Gate`.

**Spotter independence is contextual, not process-level.** It runs inside the acting
session's own daemon; the independence comes from its *fresh context*, not from isolation.
Only an in-process compromise of the daemon itself defeats it — which is the same
container-shaped tail the threat model already leaves to virtualization.

**The frame is collaborative on purpose.** The spotter is the climber's *spotter*, not a
tribunal: it exists to let the session move fast and commit hard, intervening only on the
handful of moves it would otherwise regret. The persona (`assets/prompts/spotter.md`,
`include_str!`d as the spotter client's byte-stable system prompt) carries that frame, and
the frame leaks into every verdict. The axis is **scope, not danger**: *is this move inside
the mission?* The scope is the authority — including over how much permanence is sanctioned
(a teardown scope clears `terraform destroy`). Reversibility/containment is only the default
heuristic *when the scope is silent*.

**The move block withholds the call's `intent`.** Every tool call carries a one-line
`intent` saying why it is being made ([tools.md](tools.md)); `render_move` removes it
before rendering, and a test pins the removal. Not because it would be useless — it is
exactly the context an underdetermined structured call lacks — but because its author
is the party being judged, and it knows the criteria. Two effects, both real, neither
measured:

- *Accuracy.* `rm -rf ./tmp/build` reads differently depending on what the session
  believed `./tmp` was, and the spotter has no other way to learn that.
- *Persuasion.* Intent is evidence about the session's **belief**, not a claim about
  the world: "this directory is scratch" is not evidence that it is. Any weight put on
  an unverifiable assertion is a channel for talking past the gate.

If it is turned on, the influence should be asymmetric — intent may freely *raise*
suspicion (a command that does not match its stated intent is a strong block signal,
and one available no other way), and may *lower* it only through a claim the spotter
can check (`is this path under $TMPDIR` is checkable; "this is safe" is not). It
augments the command; it never replaces it, which is the substitution the human
approval surface exists to prevent.

The cost even where it works: once the session learns the spotter reads the field, the
field changes. It stops being a cheap honest note about *why* and becomes partly a plea
to a gate that can punish it now — an immediate gradient, unlike "a human may read this
later" — which spends the honesty the condensed-transcript use depends on. So the order
is red team first: take commands the spotter blocks, have a model write intents designed
to get them through, read the flip rate. Withheld is the reversible default until then.

**A structured move carries a brief.** A bash command describes itself; `{"path": "x"}`
does not, so a call to a connected server's tool reaches the spotter as a name and a
JSON blob. The move block leads with what the mounted table knows about that tool —
which server advertised it, its description, its input schema, and the server's own
behavior hints — and only then shows the arguments (`tools::ToolBrief`,
`spotter::render_brief`). The brief is read from the `ToolDefinition` the mount
snapshotted, not from the wire `tools[]` entry, because `anthropic_def` keeps only the
three fields the Anthropic tool shape has: `annotations` and `title` are dropped there.
Description and schema are clipped to a fixed budget, so a verbose server can't crowd the
tail on every gated call. It rides the **fresh** block, so nothing about the cached prefix
moves.

The lookup covers everything a turn advertises, not just the toolbox providers:
`agent::brief` is the mirror of `agent::tool_definitions`, falling through to the
loop-owned tools (`consolidate`, the process-monitor/inbox family), whose definitions are held
in the same `ToolDefinition` shape a mount snapshots. That family once joined `tools[]`
outside the toolbox with no brief path, so the spotter judged `check_inbox` and
`list_processes` as bare names and called them opaque — the descriptions existed and never
reached the rung.

The hints (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`) are the
judged party's claims about itself, and the MCP spec warns clients not to make trust
decisions on them for untrusted servers. They are rendered anyway, labelled as
self-reported, because the threat model here is **error, not deception**: a well-meaning
server's hints are true, and a lying one is already out of scope (virtualization's tail).
A harness-owned tool's hints render without the caveat ("What the harness declares about
it") — its claims about itself carry no server distance.
The asymmetry that holds under either model is stated in the prompt — a hint is grounds to
*gate*, never sufficient on its own to *clear*. An absent hint renders as nothing rather
than as its negation: "the server said no" and "the server filled nothing in" are
different evidence.

**The request layout** is `[persona (system) | scope + watch-for (cached) | the move
(fresh)]`. The scope block is the config's scope prose plus the `[paths]` grants + cwd, so the
spotter judges "outside scope" against a fixed, external authority — never the session's own
narration. Grants render resolved to their absolute form (the same lexical rule the
deterministic rung applies) with the configured spelling as an annotation, and the block
states that a move's relative paths resolve against the cwd, with a worked example — a
spotter once gated a plain in-cwd write by comparing spellings (`scratch/…` "does not
start with `.`") instead of resolving; absolute grants make containment string-visible. The
cwd is supplied by the caller (`ForkSpot::with_cwd`), not read from the process: boot
passes the session's own directory, and the offline eval passes each case's recorded one, so
a case replayed from another checkout still resolves the way its session did. The persona is sized to clear Haiku's cache floor on its own, so even a minimal
config caches; within a session the scope is fixed, so every spotter call after the first is
a cache read (the move is the only fresh tail). The verdict is forced through a `spot` tool:
a graded [`Stance`] (`clear` / `clear_always` / `watch` / `gate`) plus a one-line `reason`
addressed to the session as a teammate. `Clear` (and `ClearAlways`) auto-approve; `Watch` and
`Gate` both route to the human gate today (the watch-window that gives `Watch` its own
intervention latency is a later unit). `ClearAlways` is the prefix-promotion safe-vote, below.

**Fail closed.** Any failure — transport error, no `spot` call — yields `Gate` with
`Origin::Fallback`: uncertainty routes to the human, never to auto-clear. A transient
upstream failure (a dropped stream, a timed-out or refused connection, an overloaded
status) is first retried up to three attempts, one and two seconds apart: a short burst is
worth absorbing, and an outage is not worth waiting out when the fallback is a prompt the
operator answers faster. (The triager has no fallback that is safe to show, so it
waits instead: a triage without a verdict holds its results until one arrives —
[triage.md](triage.md) — never a silent keep-all.)

**Each verdict is logged** as a `Payload::Spot` audit entry (the audit answer to "why did
this get through — or stop?", and the corpus for any future triaged approval model). It
projects to nothing — audit-only, never on the canonical wire. This is the *first* permission
decision that earns a log line: fast-path clears stay deliberately unlogged. `efference
spotter --scope … --watch-for … --path … "<cmd>"` runs the exact `assess` path on one move,
for iterating on the persona and config against real cases (`--watch-for` repeats; `--path`
takes `<path>:rw` / `<path>:r`). `--tool <name> --input <json>` judges a **structured** move
instead: with `--config`, the declared servers are mounted first, so the brief is built from
the definition the server actually advertises rather than a hand-written stand-in that could
drift from it. `--show-move` prints the exact block sent (implied by `--tool`), and
`--no-brief` withholds it — the A/B that measures what the brief changes. Config, mount, and
brief all resolve before credentials do, so a typo'd tool name or an unreachable server fails
on its own terms and the move preview works with no key at all. Its read-side companion, `efference dev spotter-log`, surveys
these `Spot` entries back out of the session logs — a stance-distribution summary (with the
fail-closed rate) plus each verdict correlated to the call it judged and its one-line reason,
across every `.efference/*/log.jsonl` or one session; `--stance`/`--no-fallback` filter, and
`--summary`/`--list` pick a half. The dev-facing lens for auditing how the spotter is doing.

**A `cd` that goes nowhere is left out of the move** (`policy::without_cd_into`). Agents prefix
their own working directory habitually (`cd /abs/project && cat .claude/x`), and a reviewer
asked to resolve that `cd` against the working-directory line reads the move as leaving it:
the prose rung gated such a read as "not under `.`" on a first run, and the decision rung's
false gates had a 63-case cluster of the same shape. When a bash command is `cd <dir> && rest`
with `<dir>` a literal resolving lexically to the session's cwd, both rungs judge `rest` — the
prose rung's move block says the `cd` was left out, and jev's `move.command` is the rest. A
`cd` anywhere else stays in the move for the reviewer to see. What the operator sees, and what
runs, is the command as written; only the reviewer's copy is shortened.

**A bash move states where its paths resolve** (`policy::path_words`, `policy::decide`). Both
rungs are given the words of a shell command that read as paths — a redirection target (marked
written), or an argument that is `.`/`..`, dot- or tilde-prefixed, or contains a `/` — resolved
lexically against the working directory or the literal `cd` before them, each with the grant
that decides it. The prose rung reads a list under the command (``- `.git/info/exclude`,
written → `/…/proj/.git/info/exclude`: under the grant `/…/proj` (read, write)``); jev reads
`move.paths`, one object per word with `as_written`, `resolved`, and `under_grant`, and its
three path-facing questions point at that field; its `in_scope` criteria also count ordinary
work on a granted path, a sibling checkout included, as inside the job's reach. The grants were already rendered absolute, but
the move side was left for the model to resolve, and both misread it: a first-run setup
session's append to `.git/info/exclude` gated on Haiku as "outside the project root" or "under
the denied `.efference`" (whose name the here-doc body mentioned) in 14 of 16 replays with the
live grant set, and cleared in 16 of 16 with the list.

The list must not read as complete when it is not, because a reviewer takes it as everything
the command touches: dropping `~/.cargo/registry/src/*/io.rs` because it does not resolve here
cleared a read outside every grant on jev. So:

- a word that expands at run time (a tilde, a variable, a brace) or follows a non-literal `cd`
  is listed unresolved (`expands_at_run_time` on jev) — except `$TMPDIR/…` with a literal
  remainder, which resolves under the session's scratch directory, since the harness set the
  variable; listed unresolved, it decided 89 of 162 re-scored `cd … && … "$TMPDIR/x"` gates on
  jev as "outside the grants";
- a pathname pattern (`src/*.rs`) resolves to its fixed prefix's grant, unless a more specific
  grant sits below that prefix where the pattern could match it — `.e*/x` can reach a denied
  `.efference`, `*.md` cannot, since a glob matches a dot-name only when it starts with `.` —
  and then it is reported as possibly spanning grants (`grant_undetermined`);
- when words go unread (a compound command, a function, past twelve entries, an unparseable
  command) the list says so (`paths_complete: false`).

Here-doc and here-string bodies are data and are not scanned; devices (`2>/dev/null`) are not
listed, since "no grant covers it" read as a write outside scope. The list names the deciding
grant only: an earlier wording that also said "outside the working directory" moved `cd
../sibling` moves into a granted sibling toward gate. Measured on the labeled corpus: Haiku
(one run) misses 22/23 → 15, false gates 52/53 → 45; jev (three runs, 2562 attempts) false
gates 568 → 527 with misses 4 → 5, the added one being `./target/release/efference log`, whose
path is in-tree and whose harm is what the binary does.

## The config-edit loop (`src/config/policy.rs`, `src/agent.rs`, `src/config.rs`, `src/session/config_edits.rs`)

A deny is not a dead-end: the orchestrator can **edit the config** at a gate. A
`Payload::ConfigEdit` carries one `ConfigChange` (`command` / `path` / `tool` / `scope`).
The **effective config** the cascade consults is
`base config + fold(config-edits)` — `policy::effective_config` is the batch fold (startup /
resumed log); the daemon then maintains it **incrementally** (`policy::apply_config_change` on
each `ConfigEdit` append) so a gate reads current policy in O(1). The fold is
**replace-at-key, last-write-wins** — materializing a table from an ordered log, exactly as a
database replays its write-ahead log. So **order matters** (the prior list-append fold was
order-independent), but it stays idempotent for today's changes: re-applying a change already
in the file *and* the log lands the same value, and replace-at-key admits *narrowing* a key,
which append couldn't.

The human upgrades a gate with a fourth `Approval::ConfigEdit` decision — ordinarily the grant
the gate offers (below). The loop **commits**
(`agent::commit_config_edit`): write the change to the session's config file (`ConfigFile::apply` —
a format-preserving `DocumentMut` edit that sets the key while leaving every comment and unrelated
line untouched, so auto-persist is sound for *every* slot, including a single `scope` narrowing),
append the `Payload::ConfigEdit`, then **re-classify this call against the new policy**.

**The file gets every committed grant, and a failed write fails the grant.** There is no
session-only tier: the per-session `config.toml` is the perimeter's readable form — what an operator
inspects to see what a session ran under, and what they copy a useful grant up to the origin
template from — so a grant in force that the file didn't show would be a divergence between two live
records. The write therefore precedes the append, and its failure aborts the commit (a redirect
naming the file) rather than degrading to something weaker that still logs success. A session
running on the built-in default role has no file, and there the grant lives in the log fold alone. It runs only if the changed rule now clears it; otherwise it stays
gated (a redirect). The orchestrator changes the *rule*; the mechanism authorizes the *call* —
nothing rides along unchecked.

A `ConfigEdit` projects to nothing on the wire (audit-only, like `Spot`). Its effect reaches
the model **by key**: a `commands`/`[tools]` edit only through the classifier, but a
**session-visible** edit (`scope`, `paths`) *also* surfaces as an `Ambient` notification.
`Session::append` is the one choke point — it already folds each `ConfigEdit` into the effective config, so
it also raises the notification there (`session_visible_notice`), which means **every**
initiator gets it without extra wiring. The notification is an *optional tail append* through the existing
inbox/triage machinery: the session learns its perimeter moved without the boot preamble ever
being re-rendered (live truth = `preamble + fold(notifications)`). The
spotter, fresh-context each call, just reads the effective scope directly.

**The edit lifecycle.** A `ConfigEdit` carries a `state` (`Proposed` / `Committed` / `Cancelled`)
and a `resolves: Option<Seq>`. Only a **committed** edit folds into the effective config; a
`Proposed` entry is inert and parks in the daemon's pending-proposals queue until an orchestrator
commits or cancels it (referencing the proposal's `seq` via `resolves`) — over `PATCH
/proposals/:seq` directly, or the `proposal` control tool (docs/operating.md). The synchronous paths —
the human gate, fs-watch, and an approved session edit — append `Committed` directly; the
asynchronous proposers go `Proposed → commit/cancel`.

### The grant a gate offers (`src/config/policy/gate_grant.rs`)

A gate offers at most **one** grant beside approve and abort: the `ConfigChange` that would
clear the held call and calls like it, with a label naming what it allows ("always allow
reading /Users/jbr/code/codex"). It rides `/state`'s `gate` topic as `grant: { change, label }`
([control-api.md](control-api.md)), and a client commits it by sending `change` unchanged as the
`config_edit` decision; who may commit is the same as for any decision. The label is the whole
rationale — there is no separate one — and nothing about the offer is logged until it is
committed, when the decision and the `ConfigEdit` record it as any grant.

The grant follows from **why the call held**, and the rung that held it decides whether one
applies (`agent::step::judge`), so it is never re-derived from the spotter's prose:

- **A structured call missing only its tool grant** offers that tool.
- **A structured call missing only path grants** (`policy::missing_grants`) offers a `[paths]`
  grant for the directory the call reached — the target itself when it is a directory, else its
  parent; for a batch, the shallowest such directory when it contains the rest — read-only when
  the call only reads. A directory already keyed is re-granted under its existing spelling. A
  target under a no-access grant offers nothing: the deny (the operator's, or the self-surface's)
  is an answer already given.
- **A bash call** offers a `[commands]` prefix: the shortest token-prefix of the bare command (a
  tolerated `cd <granted dir> &&` stripped) that no spotter probe has voted not safe
  (`ProbeLedger::refuted`) — the program name when the ledger holds no evidence, and the next
  token down past each refuted prefix, as the descent below does. The spotter's vote on this very
  call is recorded before the gate registers, so it counts.
- **Nothing confident, nothing offered.** A structured call missing both its tool and a path is
  held for two reasons; a command stage 1 rejects (a pipe, a `cd` into an ungranted directory) has
  no prefix that would clear it; `ConfigChange` has no command-scoped-to-a-path form. A
  `[commands]` deny offers nothing, being the operator's answer. A config write and a hand-off
  bypass the cascade, so no policy change would clear them. A **bound** gate (an allowing
  `agents__decide`) offers nothing: the leaf's policy is the child's, and what a supervisor's
  grant should cover is not settled.

Every offer is checked before it is made: applied to the effective config, it must make the
fast path clear the call. This is what keeps a `[commands]` key in force from being offered
around — under `"git push" = "gate"`, `git push origin main` offers `git push` (replacing the
key), never `git`, which the more specific key would still govern.

**The initiators built so far:** the human upgrading a gate (`Approval::ConfigEdit`); **fs-watch**
of the config file (the operator editing it externally takes effect live — `config_watch_loop`
diffs the file and emits `Committed` edits); and the **session editing its own config file**, the
config-as-reviewed-file mechanism that *replaced* the planned `propose_scope` tool — a session
`write`/`edit` to its `config.toml` is forced to the operator gate (`is_config_write`,
bypassing fast-path and spotter), and on approval the loop refreshes the file and folds it —
`agents__hand_off` takes the same route, since giving the session's place to a successor is
the operator's decision the way its policy is ([tools.md](tools.md)); the **autonomous flip**
(`--autonomous`), below; and **prefix-promotion** (the spotter files a `Proposed(Command)`),
below — the last `Proposed`-lifecycle user.

**A config edit has two outcomes, never three (`src/config.rs`).** The file is the session's policy
interface — the operator reads it to see the perimeter the session ran under — so it must never sit
in a state that doesn't parse, and an edit must never be *approved but inert*. `ConfigFile` holds
the file as a format-preserving `toml_edit` document plus the `Config` it validates to, under one
invariant: **the held document is the last content that parsed, and the disk always holds it.** Two
directions maintain it — `apply` + `sync` (memory → disk) edits a key and writes it out; `refresh`
(disk → memory) re-reads, *replacing* the held document when it parses (a merge would resurrect
keys the author deleted) and `sync`ing it back over the file when it doesn't. Reverting is therefore
not a separate mechanism: it is the same write the harness's own edits use, which is why a revert
preserves comments and key order rather than re-rendering the file from the parsed struct.

Every way the file can change routes through `refresh`, because the invariant is about the file's
state and not about who authored it: the fs-watch task calls it on any external edit, and the loop
calls it **synchronously after an approved config write**, before that call's result is logged. So
a write that doesn't parse comes back to the model as an error naming the parse failure, with the
file restored — rather than as the success the tool truthfully reported for bytes that changed
nothing. That third outcome is the defect this deletes: an approved write that reported success and
did nothing cost one session ~20 turns against a wall it believed it had cleared. The one state the
invariant can't reach — the file is invalid *and* the restore failed — is reported as itself
(`Refreshed::RevertFailed`) rather than claiming a revert that didn't happen.

**And it is terminal until a human repairs it.** Effective policy in memory is still the last
good config, but the file no longer shows it, and the file is what an operator reads to see the
perimeter a session ran under — so continuing would mean acting under permissions nobody can
inspect. `refresh_and_emit` therefore sets the **`config_repair` state topic** (`session.rs`,
control-api.md): the path, the parse error, the restore error, and a lead line saying a human must
fix the file. The loop awaits it (`Session::await_config_repair`) at every step, not only at a
trajectory's start, because an approved config write can land the condition mid-trajectory; it
resolves immediately in every other case. The park is an infinite wait on a `Notify` armed before
the state is read, the same lost-wakeup discipline the pace hold uses, and the loop phase reads
`blocked` with reason `config_repair` while it lasts — a decision point for `settle`
(docs/control-api.md).

It clears by the route that was always going to fix it: the human edits the file, fs-watch calls
`refresh` again, and any outcome that leaves valid content on disk — adopted, or a revert that
succeeded this time — clears the topic and wakes the loop. Making it terminal introduces no
blocked-with-nobody-home state, because a config write is the one operation that is **never**
autonomous: it gates for the orchestrator even under `--autonomous`, so a file in this condition
always has an owner.

## The review-level dial (`src/agent.rs`, `src/session/config_edits.rs`)

**Who reviews a call the fast-path didn't clear** is a runtime dial — a [`ReviewLevel`] ordinal on
`Session` (`policy.rs`), set over the control channel (`PATCH /loop {review_level}`,
`efference control review-level`) and reported on `GET /state`, orthogonal to pace
([operator participation](control-channel.md#operator-participation--the-intervention-points)). Four
positions, ordered by how much the position costs a human — which a supervising agent reduces
without eliminating:

- **`Autonomous`** — no synchronous human. The spotter still runs and can `Clear`; what changes is
  the residual: a spotter `Watch`/`Gate` no longer blocks on an absent human — it becomes an
  `Approval::Redirect` (`agent::autonomous_gate_message`) that hands the call back to the model with
  the spotter's reason and a pointer to its own per-session `config.toml`, the one policy interface it
  has. The model may *edit that file* to request the access — but that write hits the `is_config_write`
  carve-out and **still gates**, held for the orchestrator-above. So autonomous mode relocates the
  human, it doesn't remove it: routine gated calls self-resolve, and the operator's attention is spent
  only on genuine policy-change requests. (`--autonomous` boots into this position.)
  **A structured call's redirect names the grant it lacked.** The generic copy above says the
  call was gated; it does not say why the *tool* produced nothing, and a model reading a
  `write`/`edit` come back empty has been observed to conclude the tool is broken rather than
  that a grant is missing — session `1784334112726` decided "persist_edit without parameters
  doesn't work in autonomous mode" and switched to rewriting files through `cat > file <<EOF`,
  trading a reviewable diff for a whole-file rewrite. A model that thinks the tool is broken
  has no reason to fix its config; one that knows it lacks a grant does. So when the fast path
  can decompose the refusal (`policy::missing_grants`), the redirect
  (`agent::missing_grant_message`) leads with "the call did not run — this is not about the
  tool's behavior", names what is missing in the config's own vocabulary (`[tools]` does not
  grant `efference.edit`; no `[paths]` entry grants `write` for `src/lib.rs`), points at the config-edit
  remedy, and — for a write — says that the bash form lands on the same grant and gates the
  same way, which since the redirection carve-out it does. The two halves are reported
  independently: the path check requires the name grant as a precondition, which is true of
  the decision and misleading as a diagnosis.
- **`Supervisor`** — the session's **supervisor** reviews. The spotter still runs; what it will
  not clear is routed to the supervisor as a gate, which resolves it with `decide` through the
  ordinary approval route (the decide authority governs that call: a session's gates are
  decided by its supervisor and the operator, [tools.md](tools.md)). This is a fourth answer on
  the same dimension: `create` from a session defaults the child here, and handing a session back
  to the operator is the same dial set to `spotter_human` — there is no separate disown verb.

  **The supervisor is a pointer the host holds per session** (`Hosted::supervisor`,
  `daemon/host/supervise.rs`). Its default is `created_by`, so a session another session created
  answers to its creator and a session the operator created has none. It can be set to another
  session or cleared after creation — `PATCH /sessions/:id {"supervisor"}`, `efference control
  supervisor` ([control-api.md](control-api.md)) —
  and setting it is one action carrying everything supervision means:

  - *Gates.* A dial at the supervised default, `spotter_human`, moves to `supervisor`, as
    adoption does for a created session; an explicit `autonomous` or `human_only` is kept. Gates
    under `supervisor` then route to the new supervisor.
  - *Authority.* The new supervisor decides the session's gates, wherever the dial routes
    them, and drives it — `message`, `interrupt`, and `archive` ([tools.md](tools.md)). The
    previous supervisor's `decide` is refused from then on, naming the new one; a creator
    keeps driving the session it created. Supervision is not transitive: it reaches the
    supervised session, not the sessions that one created or supervises.
  - *Status.* The new supervisor is subscribed to the session's status with `wake`, as `create`
    subscribes a creator, and is told in its inbox (a must-wake notification) that it now
    supervises the session. Clearing or moving the pointer drops the previous supervisor's
    subscription.
  - *Push.* While the pointer names a live supervisor, the session's `ask`s and settles go
    to the supervisor alone: none of them pushes the operator's devices. Its gates push only
    when routed to the operator — the dial turned to `spotter_human` or `human_only`, or an
    escalation that reached the operator. Clearing the pointer, or the supervisor ending,
    resumes pushes from the session's next moment, and a gate held at that point is pushed
    then ([web-push.md](web-push.md)).

  The setting is refused when the new supervisor is not hosted or would make the chain cycle — the
  pointer chain is walked at set time and the refusal names the cycle (`c → a → b → c`). A
  supervisor in another project is accepted: it reaches the session by its id
  ([tools.md](tools.md)). Chains
  therefore terminate at the operator, since each session's own position is asked in turn. Every
  surface today is an operator surface: there is no in-session verb for setting it, so a
  supervising session cannot yet hand a session on, and a session cannot set its own. Each change
  is a `telemetry.supervisor_set` entry in the supervised session's log, projecting to nothing.
  The pointer itself is host control state and is not persisted: a resumed session starts again
  from its creator, if that session is hosted.

  **The position resolves when a call is routed, never at creation** (`session/review.rs`). A
  session with no live supervisor — a root, a cleared pointer, or a supervisor that has since
  ended — reviews as `SpotterHuman` from that moment, in the same fail-safe direction as
  `ReviewLevel::from_u8`: an unresolvable position moves toward the operator, never toward
  autonomous. Resolving once at creation is the failure this avoids — a child whose creator died
  would otherwise hold a call nobody was routed. `GET /state` therefore carries three fields:
  `review_level`, the dial's own value; `resolved_review_level`, the position in force; and
  `supervisor`, the id of the session that would answer (`null` for a root, a cleared pointer, or
  once the supervisor has ended), so "who answers this gate" is answerable while one is pending.
  All three are state topics and republish when the pointer moves or the supervisor ends.

  **Routing is the dial's, observation is the subscription's.** A session's status reaches its
  subscribers as an inbox notification either way ([inbox.md](inbox.md), `daemon/subscriptions.rs`);
  what `Supervisor` adds is that a *gate* also reaches the live supervisor with a must-wake and
  the reviewer's rendering of the status — the held call, why it was held, and the calls that
  decide it ([tools.md](tools.md)) — subscribed or not. The status view names the same
  reviewer (`reviewer`: the operator, or the supervisor's id), read from the same resolution. That is the distinction the position exists
  to draw: unsubscribing to stop answering a peer's gates also discarded its completion and stall
  notices, which were wanted. A creator is still subscribed with `wake` on `create`, because
  waking it when a child goes idle so it can continue is a separate use.

  **Authority is the pointer's.** Who may decide a gate is a separate rule from whom it is
  routed to. The operator's surfaces — the TUI, the web client, `efference control`, and for
  now the MCP mount — and the session's supervisor decide it, wherever the dial sends it,
  as does the session an escalation passed it to, for that gate alone (below).
  Creating a session or succeeding its creator does not by itself confer `decide`;
  `message`, `interrupt` and `archive` keep the drive relation ([tools.md](tools.md)). When
  the operator turns a child's dial to `spotter_human` while its creator is still its
  supervisor, the gate is routed to the operator and the creator may still decide it: the
  gate reaches the creator's subscription without wake (the operator is the one asked), and
  the creator's rendering reads `waiting for a decision from the operator or you`, with the
  `decide` call (`Relation::MayDecide`, `session/status/render.rs`).

  **Escalation passes one held call up.** The session a gate is routed to may answer it
  with `decide(…, escalate, message?)` instead of deciding it (docs/tools.md). The call
  stays held and is routed to that session's own supervisor, the next one up the pointer
  chain, while it is hosted; with none, to the operator. A session the call reaches may
  escalate it again, so it climbs one supervisor per escalation and ends at the operator.
  The message rides with it as a note to whoever decides next. Only the reviewer
  escalates in-session: a supervisor reading a gate routed elsewhere is refused, naming
  whom it is routed to. An operator client (`efference mcp`, `efference control`) is on
  the operator's side already, so its escalation goes to the operator, which asks the
  person through the gate views and a push ([web-push.md](web-push.md)).

  - *Routing.* The held call's escalations live on the pending gate (`Pending::escalations`,
    `session/approvals.rs`), and while it has any, the last one routes it rather than the
    dial (`Session::gate_reviewer`, `session/status/view.rs`): turning the dial or moving
    the pointer leaves it with the session it went to. The status watcher re-posts it to
    that session as its reviewer, with wake, and stops posting it to the escalator as one;
    the escalator's subscription reads it without wake, as waiting on the new reviewer,
    with an `escalated:` line naming whom it went to. A session it went to that has since
    ended routes it to the operator, by the fail-safe above.
  - *Authority.* The session an escalated call was last passed to may decide that call,
    though it does not supervise the holder; `decide` naming any other seq of the holder,
    and any later gate, is refused to it as to any non-supervisor
    (`HostExecutor::may_decide_gate`). The holder's supervisor keeps its own authority
    over the call it escalated.
  - *Record.* Each escalation is a `telemetry.escalation` entry in the holder's log —
    the call, the escalator as a `source`, the session it went to (absent for the
    operator), and the note — projecting to nothing. The gate topic and the status view
    carry the same hops ([control-api.md](control-api.md)), and the transcript prints
    them on the call's line.

  **A level change re-routes a gate already held; it never drops one.** The status watcher is
  level-triggered on the status, the resolved position, *and* the live supervisor, so turning the
  dial, moving or clearing the pointer, or the supervisor ending re-posts the held call to whoever
  now answers it, and a held call that comes to rest with the operator this way is pushed
  ([web-push.md](web-push.md)). Handing off future approvals is not
  abandoning a call that is holding a child right now. An escalated call is the exception:
  its escalations route it, so the watcher compares those instead of the position
  (`Routing`, `session/review.rs`), and a dial change does not move it.

  There is deliberately no `supervisor_only` position mirroring `HumanOnly`: if a nonhuman reviewer
  is acceptable at all, the spotter takes the first shot, being cheap, purpose-briefed, and
  resolving its most restrictive answer to this gate anyway.
- **`SpotterHuman`** — the supervised default: fast-path → spotter → human gate.
- **`HumanOnly`** — the human takes the spotter's role **entirely**: the spotter is **skipped**
  (no model call, no token cost, no `Spot` entry), and every call the fast-path didn't clear goes
  straight to the human gate. The spotter has *two* jobs — clearing residual calls **and** growing
  the allowlist (`clear_always` → prefix promotion) — so skipping it would also stop the allowlist
  growing, were it not for the human gate's own route to a rule: the **grant the gate offers**
  (above), which the human commits as a `config_edit` decision **directly** (no `Proposed`
  round-trip — the human *is* the reviewer). Every human gate offers one where a single change
  confidently fits, so the route serves plain `SpotterHuman` too; under `HumanOnly` it is the
  only way the allowlist grows during the session.

## Prefix-promotion: the harness learns its allowlist (`src/config/policy.rs`, `src/agent.rs`, `src/curation/spotter.rs`)

The allowlist grows by *probing*, so the spotter-call rate decays as a session's real toolchain
saturates. The cognitive load on the spotter stays a single extra yes/no, and the "learning" is
deterministic, exact, and auditable — a vote ledger, not a fuzzy model.

When a bash move reaches the spotter, the loop runs a **descent** (`policy::ProbeLedger`): it bumps a
`seen_count` for each of the command's token-prefixes — of the **bare** command, with any tolerated
leading `cd <dir> &&` stripped, so a promoted prefix generalizes across run directories rather than
pinning one target — then picks the **shortest** prefix that is
uncovered, seen at least `MIN_SIGHTINGS_TO_PROBE` times, and not yet resolved. That prefix rides the
*same* spot call — the move prompt names it, and the answer **is the stance**: `clear_always` =
clear the move *and* vote the prefix blanket-safe; any other stance is a not-safe vote. Folding the
answer into the stance (rather than a separate flag) makes the contradictions — `watch`/`gate` +
"always safe" — unrepresentable. Each call records a `Spot { stance, probed_prefix }` (still
audit-only, projects to nothing), and the ledger **folds from those `Spot` entries** (`probe_ledger`,
mirroring `effective_config`), so a resumed daemon recovers its counts and votes. When a prefix's
safe-votes reach `PROMOTE_AFTER_SAFE_VOTES` (`K`), the loop files a non-blocking `Proposed(Command)`
— inert until an orchestrator-above commits it, exactly the existing proposal sink.

The descent self-terminates: `git`→not-safe, then `git log`→safe promotes `git log`, whose subtree
then fast-path clears and never reaches the spotter again. `K`/`N` are the dials toward evidence-only,
human-out-of-the-loop promotion (each probe is a *fresh* spotter, so probing a prefix across its
recurrences is independent-judge consensus accruing in the ledger). `efference dev spotter --probe
<prefix> …` exercises one probe. See
[`../backlog/inbox/permissions/prefix-promotion.md`](../backlog/inbox/permissions/prefix-promotion.md).

## What is not built yet

The watch-window and the spotter's effect-preview. Prefix-promotion ships at `K = 1` (the first
`clear_always` files a proposal); the consensus thresholds (`K > 1`, dissent-tolerance) and
evidence-only auto-commit are dials over the same ledger, not yet turned. `Stance::Watch` resolves to the human gate (or, autonomous, the redirect) today;
the watch-window will map it to an intervention latency instead. The spotter currently judges
the raw command + config context; a neutral effect-preview is the planned input upgrade. Fleet-wide
persona caching (a second breakpoint isolating the persona so it's shared across differently-scoped
sessions) is a deferred optimization — per-session caching works today. See
[`../backlog/inbox/permissions/README.md`](../backlog/inbox/permissions/README.md).

[`toml_edit`]: https://docs.rs/toml_edit
[`DocumentMut`]: https://docs.rs/toml_edit/latest/toml_edit/struct.DocumentMut.html
[`Config`]: ../src/config.rs
