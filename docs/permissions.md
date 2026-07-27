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
obfuscates, and obfuscation is out of scope. The axis the policy decides on is therefore
**reversibility/containment** ("can the harness undo this; is it confined to scope?"), a
question often decidable from observable facts, not "is this command dangerous?", which is
undecidable from a bash string.

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
cargo = true          # blanket — key lexes to ["cargo"]
"git diff" = true     # sub-verb — key lexes to ["git", "diff"]

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
- **`[commands]`** — the bash prefix-allowlist, a **set** of command-prefix keys (below).
- **`tools`** and **`[paths]`** — the structured-tool name set and the filesystem-grant map
  the fast-path reads (below).
- **`trust-threshold`** — where the watch-window/gate cutoffs will sit. Parsed, unused.

**Two-layer decode, fail-closed.** Serde reads the file into a raw struct
(`deny_unknown_fields`, so a typo'd key is a loud error), then `validate` folds it into the
`Config`: every `[commands]` key is **lexed with the fast-path's own lexer**
(`policy::simple_command_tokens`) so the stored key and the command it's matched against
agree token-for-token, and a key that isn't a single simple command (`"git | rm"`) **fails
the whole file** — no silent skip. A `false` command value is rejected too: the fold is
additive and can't deny, so a deny would silently do nothing.

## The fast-path (`src/policy.rs`)

`classify(&Config, &ToolCall) -> Verdict` is a **pure function** — no I/O, no model —
returning `Clear` (auto-approve: no human gate, no model round-trip) or `Gate` (fall
through to the spotter rung, then the human gate). The loop consults it just before the
gate: `Clear` becomes an immediate approve; `Gate` hands the call to the spotter (below).
It is the cheapest rung of the cascade, and it only ever *accepts what it is certain about*,
erring toward `Gate` on any doubt.

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
- **Stage 2** matches the lexed `[program, args…]` against the `[commands]` set: each key
  is **re-lexed with the same lexer** and compared token-wise (so `"git diff"` matches as
  `["git", "diff"]`, not by string), an empty-args key (`cargo`) blanket-trusting the
  program. Re-lexing a handful of keys is free next to parsing the command itself.

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
verbs (`git push`, `git reset`) fall through. Memoization (the prefix-promotion growth of this
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
treated as inside — acceptable under the error-not-deception model.

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

**The frame is collaborative on purpose.** The spotter is the climber's *spotter*, not a
tribunal: it exists to let the session move fast and commit hard, intervening only on the
handful of moves it would otherwise regret. The persona (`assets/prompts/spotter.md`,
`include_str!`d as the spotter client's byte-stable system prompt) carries that frame, and
the frame leaks into every verdict. The axis is **scope, not danger**: *is this move inside
the mission?* The scope is the authority — including over how much permanence is sanctioned
(a teardown scope clears `terraform destroy`). Reversibility/containment is only the default
heuristic *when the scope is silent*.

**The request layout** is `[persona (system) | scope + watch-for (cached) | the move
(fresh)]`. The scope block is the config's scope prose plus the `[paths]` grants + cwd, so the
spotter judges "outside scope" against a fixed, external authority — never the session's own
narration. The persona is sized to clear Haiku's cache floor on its own, so even a minimal
config caches; within a session the scope is fixed, so every spotter call after the first is
a cache read (the move is the only fresh tail). The verdict is forced through a `spot` tool:
a graded [`Stance`] (`clear` / `clear_always` / `watch` / `gate`) plus a one-line `reason`
addressed to the session as a teammate. `Clear` (and `ClearAlways`) auto-approve; `Watch` and
`Gate` both route to the human gate today (the watch-window that gives `Watch` its own
intervention latency is a later unit). `ClearAlways` is the prefix-promotion safe-vote, below.

**Fail closed.** Any failure — transport error, no `spot` call — yields `Gate` with
`Origin::Fallback`: uncertainty routes to the human, never to auto-clear. (Contrast the
distiller, which fails *open* to keep-all, because dropping output loses data whereas wrongly
clearing a move does harm.)

**Each verdict is logged** as a `Payload::Spot` audit entry (the audit answer to "why did
this get through — or stop?", and the corpus for any future distilled approval model). It
projects to nothing — audit-only, never on the canonical wire. This is the *first* permission
decision that earns a log line: fast-path clears stay deliberately unlogged. `efference
spotter --scope … --watch-for … --path … "<cmd>"` runs the exact `assess` path on one move,
for iterating on the persona and config against real cases (`--watch-for` repeats; `--path`
takes `<path>:rw` / `<path>:r`). Its read-side companion, `efference spotter-log`, surveys
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

## The review-level dial (`src/agent.rs`, `src/daemon/config_edits.rs`)

**Who reviews a call the fast-path didn't clear** is a runtime dial — a [`ReviewLevel`] ordinal on
`Shared` (`policy.rs`), set over the control channel (`POST /review-level`, `efference review-level`)
and reported on `GET /state`, orthogonal to pace (backlog/participation/README.md). Three
positions of increasing human involvement:

- **`Autonomous`** — no synchronous human. The spotter still runs and can `Clear`; what changes is
  the residual: a spotter `Watch`/`Gate` no longer blocks on an absent human — it becomes an
  `Approval::Redirect` (`agent::autonomous_gate_message`) that hands the call back to the model with
  the spotter's reason and a pointer to its own per-session `config.toml`, the one policy interface it
  has. The model may *edit that file* to request the access — but that write hits the `is_config_write`
  carve-out and **still gates**, held for the orchestrator-above. So autonomous mode relocates the
  human, it doesn't remove it: routine gated calls self-resolve, and the operator's attention is spent
  only on genuine policy-change requests. (`serve --autonomous` boots into this position.)
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
`seen_count` for each of the command's token-prefixes, then picks the **shortest** prefix that is
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
