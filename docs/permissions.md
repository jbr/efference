# Permissions — config, the fast-path & the spotter

A session is parameterized by `(config, cwd, orchestrator)`. The **config** is the
portable-policy half: what a session is *for* and what it may do, kept separate from cwd
(the per-invocation spatial scope, see [env](../src/env.rs) / the system preamble) so the
config stays portable. Policy is **data the classifier interprets**, never special-cased in
code — there are no privileged program names or directories in the mechanism, only config
the mechanism reads.

This doc covers what is built now: the config (`src/config.rs`), the deterministic
approval fast-path (`src/policy.rs`), and the independent spotter (`src/spotter.rs`),
wired into the turn loop's gate (`src/agent.rs`). The watch-window and the rest of the
config-edit loop are planned; see [`../backlog/permissions/`](../backlog/permissions/).

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

**Per-session snapshot (the config is a reviewed file).** At boot the origin (`serve --config <path>`,
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

tools = ["read", "write"]
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
- **`tools`** and **`[paths]`** — the structured-tool name set and the filesystem-grant map
  the fast-path reads (below).
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
A peek tool (`retain`/`triage`) called when nothing was asked of it is paired with the
nudge result ([redaction-fork.md](redaction-fork.md)). And a call naming **no tool this
turn advertised** is answered with `tools::unknown_tool_message` — the mounted names, plus
a pointer at `bash` for a name that is a program — as a `Direct` payload, so the correction
reaches the wire verbatim rather than through the triage gate.

The membership test reads the names off `agent::tool_definitions` itself, minus the peek
tools, so what the check accepts and what the request advertised cannot drift.

The check is ahead of the rungs rather than inside one because the call cannot run whatever
any rung answers. Asked anyway, the spotter answered: it returned `clear` for a call naming
a nonexistent tool, with a rationale invented for the call it imagined ("reversible query
that doesn't modify state or escape the repo"), and `spotter-log` counted that as a clean
clear. A rung that judges intent against `scope` and `watch-for` has nothing in its inputs
that says the target does not exist.

## The fast-path (`src/policy.rs`)

`classify(&Config, &ToolCall) -> Verdict` is a **pure function** — no I/O, no model —
returning `Clear` (auto-approve: no human gate, no model round-trip), `Gate` (fall
through to the spotter rung, then the human gate), or `Deny` (skip the spotter and go
straight to whoever gates). The loop consults it just before the gate: `Clear` becomes an
immediate approve; `Gate` hands the call to the spotter (below); `Deny` routes to the human
gate, or, autonomous, to the same redirect a spotter `Gate` takes there — `agent::denied_gate_message`
names the config rather than a spotter verdict nobody gave. `Deny` is reachable only from a
`[commands]` stance (below); everything else the fast-path is unsure of is a `Gate`, and it
only ever *accepts what it is certain about*.

### bash: a two-stage check; the order matters

Stage 1 (structural simplicity) runs **before** stage 2 (prefix match), so a trusted
`git diff` prefix can never clear `git diff > /etc/hosts` or `git diff $(rm x)` — those die
in stage 1, before any prefix is consulted. This ordering is what makes a blanket
`git = true` survivable in principle and a sub-verb `"git diff" = true` sound.

- **Stage 1** parses the command with `yash-syntax` — a real POSIX parser, not a regex —
  and accepts only a *single simple command*: no operators (`| & ; && ||`), no *file*
  redirection, no command/arithmetic substitution, no parameter/tilde expansion, no
  env-assignment prefix, no unquoted glob/brace. The one redirection it tolerates is a pure
  file-descriptor duplication/closure (`2>&1`, `>&2`, `2>&-`) — those rewire the process's
  own fds and touch no filesystem, so they're sound, and admitting them keeps the habitual
  `cmd 2>&1` on the fast path (without it a trailing `2>&1` knocks blanket `cargo = true` off,
  as a dogfood session found). A `>&` whose operand is a *filename* or an expansion is not a
  pure dup and still gates. The parser is needed (not a regex)
  because safety is quote-sensitive: the `|` in `rg 'a|b' src/` is one literal argument,
  not a pipe. yash *parses* without *expanding*, so we inspect word units ourselves for
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
  (observed in session `1784334112726`). Governing the bash form as the same write means
  routing around the tool changes nothing about the answer.

  **Two authorities govern and the more restrictive one wins.** The command must still clear
  stage 2 on its own — `cat` needs a `clear` stance — *and* the target must be write-granted.
  A `[commands]` `deny` refuses whatever the path says; a `clear` command writing outside a
  write grant gates. The `tools` set is not consulted: it names structured tools, and the tool
  being called is `bash`, whose name grant is its `[commands]` stance. Composed with the `cd`
  tolerance, the target resolves against the **`cd`'s** directory, since that is where the
  command runs. The self-surface stack needs no special case — it already denies writes under
  `.efference/`, so a here-doc aimed at a session's own `config.toml` or log fails the path
  check like any other ungranted target.

  The admitted shapes are exhaustive and everything else fails closed to the spotter as
  before: an expansion or glob in the target, more than one file redirection, an input
  redirection or here-string, a non-stdout fd (`2> file`), a here-doc whose content contains
  any expansion or substitution (an unquoted here-doc *runs* `$(…)`, which yash surfaces as a
  substitution unit), pipes, process substitution, lists, backgrounding, negation, assignment
  prefixes, and anything that does not parse. It is a governed exception, not a general
  loosening of stage 1's rejection of file redirections.
- **Stage 2** matches the lexed `[program, args…]` against the `[commands]` map: each key
  is **re-lexed with the same lexer** and compared token-wise (so `"git diff"` matches as
  `["git", "diff"]`, not by string), an empty-args key (`cargo`) addressing the whole
  program. Re-lexing a handful of keys is free next to parsing the command itself. The
  stance of the **longest** matching key is the answer (below).

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
  human, or the autonomous redirect) with no model call. Nothing the spotter could answer
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

**Why a POSIX parser is the right choice for a security gate.** yash is POSIX-strict, not
bash-complete, which costs only *coverage* (a few bash-only forms — `[[ ]]`, `<(…)`,
`for…do` — gate to the spotter), never *soundness*: bash's dangerous operators are
POSIX too, so they parse as operators and gate; bash-only syntax either fails to parse
(→ gate) or surfaces as inert literal words that match no allowlist entry. A young
bash-complete parser would improve coverage but put a broad, immature surface on the
security-critical path — the wrong risk trade. yash is pinned to the last standalone
version (`0.14.1`); later versions pull in the yash *runtime* (`yash-env`), which we don't
want — we parse, bash executes.

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

A structured (non-bash) tool clears only if its name is in the config's `tools` set
**and**, for the path-bearing ones, its target resolves under a `[paths]` grant with the
needed permission — reads (`read`/`list`/`search`/`find_anchor`) need `read`, writes
(`write`/`edit`) need `write` (`write` implies `read`). Nothing clears in code; an
empty config gates every tool, exactly as before.

The name match is **exact**, unlike `[commands]`'s token-prefix match — with one glob, below.
A connected provider's tools reach the wire namespaced (`<server>__<tool>`, docs/tools.md), so
clearing a whole server by exact entries means listing each of its tools; `tools = ["ferritin"]`
clears nothing. An entry is written and displayed in the dotted form (`"ferritin.get"`) — the file
the write-back produces, the sidebar's labels, and the transcript all agree — and is resolved
to the wire name at validate, so the set the fast-path reads holds one name per grant
however the operator spelled it.

#### `"<namespace>.*"`: a whole-namespace grant, and what it claims

```toml
tools = ["efference.*", "ferritin.*", "github.create_issue"]
```

A terminal `.*` grants a **namespace**: every tool that server advertises, cleared on the
fast path with no spotter on the way through. The namespace is the `<server>` half of the
wire name, resolved at the **first** `__` (server names may not contain one, so the boundary
is unambiguous in every direction); the harness's own embedded tools have no prefix, so they
sit in the `efference` namespace, and `"efference.*"` is the "every tool this harness ships"
grant that needs no editing when a built-in is added. The glob takes the same dotted→wire
route an exact entry takes (`ferritin.*` is stored as `ferritin__*`), so one set holds both
kinds, and `*` — absent from the API's tool-name alphabet — can never collide with a real
name. The star is **only** legal in that terminal position: `"ferritin.ge*"`, `"*"`, and
`"ferritin.admin.*"` fail the whole file at validate rather than parsing into a grant the
matcher would silently never satisfy.

**What a `.*` grant claims is trust in the *party*, not in an enumerated set.** A blanket
`cargo = true` is survivable because of stage 1: the structural check runs before any prefix
match, so a trusted prefix can never clear `cargo … > /etc/hosts`. A structured call has no
stage 1 — there is nothing between the grant and whatever JSON the server interprets — and the
granted set is defined by the other party and can grow: a server's table is frozen at mount
and may differ at the next one. So `"ferritin.*"` means
"anything you can do with this server is ok — if it adds a new tool, that is allowed too."
A server not trusted to that degree should not hold a `.*` grant; the harness does not
constrain the glob to the mount-time table, because a grant that needed that constraint is a
grant that shouldn't have been written. It is never the default on mount — the glob is an
explicit config act, and defaulting it would move the perimeter every time a server is added.

**Where the glob sits relative to `[paths]`.** The glob is a *name* grant, so the second
check is untouched: a path-bearing built-in under `"efference.*"` still clears only inside a
`[paths]` grant, which is why a whole-namespace grant over the built-ins has a bounded blast
radius. A connected tool's does not — the fast path knows nothing about what a remote tool
touches, so `"ferritin.*"` is bounded by the grant alone.

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
`.efference/<id>/config.toml` read-only. The harness **injects that self-surface stack in
`serve`** (`src/bin/efference/serve.rs`), layered over the operator's grants: the session dir holds this
session's control socket, its append-only log, and every *other* session's dir, all under the
operator's `.` grant — so without the deny nothing would stop a session from writing its own
log (corrupting the session record) or POSTing to its own control socket (approving its own gates,
injecting a user turn). The `config.toml` re-allow is the sanctioned scope-expansion path: the
session reads its own perimeter and *edits* the file, a write the governing read-only grant
refuses at the fast-path, so it gates for the operator. Injected keys are inserted *after* the
operator config, so a safety deny overrides an equal-specificity operator grant; an operator
can still override by naming a *strictly deeper* path. Because bash never touches the fast-path,
the deny layers are also rendered into the **spotter context** (`spotter.rs`), so a `curl
--unix-socket .efference/…/control.sock` or `echo >> …/log.jsonl` gates on scope grounds there
too. Path matching is lexical (`..`/`.` collapsed without touching the filesystem, so it works
for not-yet-existing write targets), which means a symlink inside a grant pointing outside is
treated as inside — acceptable under the error-not-deception model. For the same reason a path
carve-out is a **mistake-guardrail, not access control**: a symlink or a `cat ./sec*` through
bash reaches around it, so a deny here keeps a well-intentioned session out of a directory and
claims nothing more.

**No privileged client, enforced by reachability.** A session must never hand-roll
control-channel HTTP from bash; every legitimate control interaction is a tool. The deny
layers above are what enforce that, and the rule is written to cover a session's *own*
socket, a *peer's*, and a *parent's* uniformly — so the property survives into multiagent
topologies without a second mechanism (see
[`../backlog/multiagent/`](../backlog/multiagent/README.md)).

`serve` injects one more grant alongside the self-surface stack: the per-session **scratch
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
schema) is later architectural work; for now the coupling is marked, not hidden.

### the `[context]` rung: a listing is a grant

A `[context]` entry ([context.md](context.md)) is an independent clearance source, unioned
per-path with the name-grant rule (above): listing a path **is** a read grant — the preamble
stub invites the model to `Read {path}`, so that read fast-path clears with no `tools` or
`[paths]` config at all. The same lexical subtree rule applies, and the caveat is stated loudly: a
location entry (dir, and later glob) read-grants everything under it, so operators must
list deliberately. The entry's write grants ride the same check: `update` clears a write
to an **existing** file under the entry, `create` a write to a **new** one — `create`
without `update` is an append-only store. Telling create from update is the classifier's
one filesystem touch (an existence probe) — sound here because a verdict is derived state,
never on the wire. The context rung covers only the structured path-bearing tools; bash
never clears through it. A write to the session's own config file still bypasses
everything to the human gate.

## The spotter (`src/spotter.rs`)

What the fast-path can't `Clear` falls to the **spotter** before it reaches the human — a
one-shot, **fresh-context** model call (Haiku band, on its own `Client`, model-independent
of the acting session) that re-decides the residual. Independence is the point: the
acting session can't check its own folly because it shares its own trajectory; the spotter
can, precisely because its context is built from scratch — *not* `project(log)`.

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
does not, so a call to a connected server's tool used to reach the spotter as a name and a
JSON blob. The move block now leads with what the mounted table knows about that tool —
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
loop-owned tools (`consolidate`, the supervisor/inbox family), whose definitions are held
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
cwd is supplied by the caller (`ForkSpot::with_cwd`), not read from the process: `serve`
passes the session's own directory, and the offline eval passes each case's recorded one, so
a case replayed from another checkout still resolves the way its session did. The persona is sized to clear Haiku's cache floor on its own, so even a minimal
config caches; within a session the scope is fixed, so every spotter call after the first is
a cache read (the move is the only fresh tail). The verdict is forced through a `spot` tool:
a graded [`Stance`] (`clear` / `clear_always` / `watch` / `gate`) plus a one-line `reason`
addressed to the session as a teammate. `Clear` (and `ClearAlways`) auto-approve; `Watch` and
`Gate` both route to the human gate today (the watch-window that gives `Watch` its own
intervention latency is a later unit). `ClearAlways` is the prefix-promotion safe-vote, below.

**Fail closed.** Any failure — transport error, no `spot` call — yields `Gate` with
`Origin::Fallback`: uncertainty routes to the human, never to auto-clear. (The distiller
fails the same direction with a different artifact: a failed triage gate withholds the
output as a hard error on the wire — [redaction-fork.md](redaction-fork.md) — never a
silent keep-all.)

**Each verdict is logged** as a `Payload::Spot` audit entry (the audit answer to "why did
this get through — or stop?", and the corpus for any future distilled approval model). It
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
on its own terms and the move preview works with no key at all. Its read-side companion, `efference spotter-log`, surveys
these `Spot` entries back out of the session logs — a stance-distribution summary (with the
fail-closed rate) plus each verdict correlated to the call it judged and its one-line reason,
across every `.efference/*/log.jsonl` or one session; `--stance`/`--no-fallback` filter, and
`--summary`/`--list` pick a half. The dev-facing lens for auditing how the spotter is doing.

## The config-edit loop (`src/policy.rs`, `src/agent.rs`, `src/config.rs`, `src/daemon/config_edits.rs`)

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

The human upgrades a gate with a fourth `Approval::ConfigEdit` decision. The loop **commits**
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
the model **by key**: a `commands`/`tools` edit only through the classifier, but a
**session-visible** edit (`scope`, `paths`) *also* surfaces as an `Ambient` notification.
`Shared::append` is the one choke point — it already folds each `ConfigEdit` into the effective config, so
it also raises the notification there (`session_visible_notice`), which means **every**
initiator gets it without extra wiring. The notification is an *optional tail append* through the existing
inbox/triage machinery: the session learns its perimeter moved without the boot preamble ever
being re-rendered (live truth = `preamble + fold(notifications)`). The
spotter, fresh-context each call, just reads the effective scope directly.

**The edit lifecycle.** A `ConfigEdit` carries a `state` (`Proposed` / `Committed` / `Cancelled`)
and a `resolves: Option<Seq>`. Only a **committed** edit folds into the effective config; a
`Proposed` entry is inert and parks in the daemon's pending-proposals queue until an orchestrator
commits or cancels it (referencing the proposal's `seq` via `resolves`). The synchronous paths —
the human gate, fs-watch, and an approved session edit — append `Committed` directly; the
asynchronous proposers go `Proposed → commit/cancel`.

**The initiators built so far:** the human upgrading a gate (`Approval::ConfigEdit`); **fs-watch**
of the config file (the operator editing it externally takes effect live — `config_watch_loop`
diffs the file and emits `Committed` edits); and the **session editing its own config file**, the
config-as-reviewed-file mechanism that *replaced* the planned `propose_scope` tool — a session
`write`/`edit` to its `config.toml` is forced to the operator gate (`is_config_write`,
bypassing fast-path and spotter), and on approval the loop refreshes the file and folds it; the **autonomous flip**
(`serve --autonomous`), below; and **prefix-promotion** (the spotter files a `Proposed(Command)`),
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
inspect. `refresh_and_emit` therefore sets the **`config_repair` state topic** (`daemon.rs`,
control-api.md): the path, the parse error, the restore error, and a lead line saying a human must
fix the file. The loop awaits it (`Shared::await_config_repair`) at every step, not only at a
trajectory's start, because an approved config write can land the condition mid-trajectory; it
resolves immediately in every other case. The park is an infinite wait on a `Notify` armed before
the state is read, the same lost-wakeup discipline the pace hold uses.

It clears by the route that was always going to fix it: the human edits the file, fs-watch calls
`refresh` again, and any outcome that leaves valid content on disk — adopted, or a revert that
succeeded this time — clears the topic and wakes the loop. Making it terminal introduces no
blocked-with-nobody-home state, because a config write is the one operation that is **never**
autonomous: it gates for the orchestrator even under `--autonomous`, so a file in this condition
always has an owner.

## The review-level dial (`src/agent.rs`, `src/daemon/config_edits.rs`)

**Who reviews a call the fast-path didn't clear** is a runtime dial — a [`ReviewLevel`] ordinal on
`Shared` (`policy.rs`), set over the control channel (`PATCH /loop {review_level}`, `efference review-level`)
and reported on `GET /state`, orthogonal to pace ([operator participation](control-channel.md#operator-participation--the-intervention-points)). Three
positions of increasing human involvement:

- **`Autonomous`** — no synchronous human. The spotter still runs and can `Clear`; what changes is
  the residual: a spotter `Watch`/`Gate` no longer blocks on an absent human — it becomes an
  `Approval::Redirect` (`agent::autonomous_gate_message`) that hands the call back to the model with
  the spotter's reason and a pointer to its own per-session `config.toml`, the one policy interface it
  has. The model may *edit that file* to request the access — but that write hits the `is_config_write`
  carve-out and **still gates**, held for the orchestrator-above. So autonomous mode relocates the
  human, it doesn't remove it: routine gated calls self-resolve, and the operator's attention is spent
  only on genuine policy-change requests. (`serve --autonomous` boots into this position.)
  **A structured call's redirect names the grant it lacked.** The generic copy above says the
  call was gated; it does not say why the *tool* produced nothing, and a model reading a
  `write`/`edit` come back empty has been observed to conclude the tool is broken rather than
  that a grant is missing — session `1784334112726` decided "persist_edit without parameters
  doesn't work in autonomous mode" and switched to rewriting files through `cat > file <<EOF`,
  trading a reviewable diff for a whole-file rewrite. A model that thinks the tool is broken
  has no reason to fix its config; one that knows it lacks a grant does. So when the fast path
  can decompose the refusal (`policy::missing_grants`), the redirect
  (`agent::missing_grant_message`) leads with "the call did not run — this is not about the
  tool's behavior", names what is missing in the config's own vocabulary (`tools` does not
  list `edit`; no `[paths]` entry grants `write` for `src/lib.rs`), points at the config-edit
  remedy, and — for a write — says that the bash form lands on the same grant and gates the
  same way, which since the redirection carve-out it does. The two halves are reported
  independently: the path check requires the name grant as a precondition, which is true of
  the decision and misleading as a diagnosis.
- **`SpotterHuman`** — the supervised default: fast-path → spotter → human gate.
- **`HumanOnly`** — the human takes the spotter's role **entirely**: the spotter is **skipped**
  (no model call, no token cost, no `Spot` entry), and every call the fast-path didn't clear goes
  straight to the human gate. The catch the design turns on: the spotter has *two* jobs — clearing
  residual calls **and** growing the allowlist (`clear_always` → prefix promotion) — so skipping it
  can't just be "gate everything," or learning stops. The human inherits both jobs via the gate's
  **`approve-always-at-prefix`** grant (the `.allow` affordance / a `config_edit` approval decision): a self-authorizing `ConfigChange` committed **directly** (no `Proposed` round-trip — the human *is* the reviewer). That grant works at any human gate, so it improves plain `SpotterHuman` too; `HumanOnly` just relocates the learning to the human rather than losing it.

## Prefix-promotion: the harness learns its allowlist (`src/policy.rs`, `src/agent.rs`, `src/spotter.rs`)

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
recurrences is independent-judge consensus accruing in the ledger). `efference spotter --probe
<prefix> …` exercises one probe. See
[`../backlog/permissions/prefix-promotion.md`](../backlog/permissions/prefix-promotion.md).

## What is not built yet

The watch-window and the spotter's effect-preview. Prefix-promotion ships at `K = 1` (the first
`clear_always` files a proposal); the consensus thresholds (`K > 1`, dissent-tolerance) and
evidence-only auto-commit are dials over the same ledger, not yet turned. `Stance::Watch` resolves to the human gate (or, autonomous, the redirect) today;
the watch-window will map it to an intervention latency instead. The spotter currently judges
the raw command + config context; a neutral effect-preview is the planned input upgrade. Fleet-wide
persona caching (a second breakpoint isolating the persona so it's shared across differently-scoped
sessions) is a deferred optimization — per-session caching works today. See
[`../backlog/permissions/README.md`](../backlog/permissions/README.md).

[`toml_edit`]: https://docs.rs/toml_edit
[`DocumentMut`]: https://docs.rs/toml_edit/latest/toml_edit/struct.DocumentMut.html
[`Config`]: ../src/config.rs
