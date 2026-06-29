# Permissions — config, the fast-path & the spotter

A session is parameterized by `(config, cwd, orchestrator)`. The **config** is the
portable-policy half: what a session is *for* and what it may do, kept separate from cwd
(the per-invocation spatial scope, see [env](../env.rs) / the system preamble) so the
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
virtualization's job (run the daemon in a container). This boundary is load-bearing: it is
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
both reads the config and writes the harness's edits back with comments intact. Resolution:
an explicit `serve --config <path>` (which must exist), else the `efference.toml` probe
(absent → the empty default config, so nothing auto-clears — safe). The resolved source is
reported on `GET /state`.

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
  [substrate](substrate.md)): it is rendered once at startup from the scope text and
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

### bash: a two-stage check, order load-bearing

Stage 1 (structural simplicity) runs **before** stage 2 (prefix match), so a trusted
`git diff` prefix can never clear `git diff > /etc/hosts` or `git diff $(rm x)` — those die
in stage 1, before any prefix is consulted. This ordering is what makes a blanket
`git = true` survivable in principle and a sub-verb `"git diff" = true` sound.

- **Stage 1** parses the command with `yash-syntax` — a real POSIX parser, not a regex —
  and accepts only a *single simple command*: no operators (`| & ; && ||`), no
  redirection, no command/arithmetic substitution, no parameter/tilde expansion, no
  env-assignment prefix, no unquoted glob/brace. The parser is needed (not a regex)
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
verbs (`git push`, `git reset`) fall through. Memoization (the eventual config-edit-fed
growth of this list) is sound only at **program-prefix granularity, never command
granularity** — safety is discontinuous in the command string (`./build` vs `/build`), so
exact-command memoization is what strands dead entries and is anti-correlated with safety.

### structured tools: name grant + path permission

A structured (non-bash) tool clears only if its name is in the config's `tools` set
**and**, for the path-bearing ones, its target resolves under a `[paths]` grant with the
needed permission — reads (`read`/`list`/`search`/`read_node`) need `read`, writes
(`write`/`preview_edit`) need `write` (`write` implies `read`). Nothing clears in code; an
empty config gates every tool, exactly as before.

Two properties are load-bearing. **Grants are additive subtrees with no deny.** A `[paths]`
grant trusts its *entire* subtree; there is no carve-out to deny a sensitive subdir within
an allowed parent (you cannot allow `.` but deny `./secrets`) — explicitly out of scope
for now. Path matching is lexical (`..`/`.` collapsed without touching the filesystem, so
it works for not-yet-existing write targets), which means a symlink inside a grant
pointing outside is treated as inside — acceptable under the error-not-deception model.

**Mapping a tool to its path field is a deliberate wart.** The classifier reaches into
each built-in's JSON input by known key (`read`→`paths`, `write`→`path`,
`preview_edit`→`file_path`, …); the string literals are centralized in `policy.rs`. The
sharp case: `persist_edit` writes to disk but carries *no* path (the target was named by an
earlier `preview_edit`), so write permission is enforced at `preview_edit` — the only
edit-pipeline call that names the file — and `persist_edit` clears on the name grant. The
cohesive fix (an MCP-superset that carries permission/notification metadata alongside the
schema) is later architectural work; for now the coupling is marked, not hidden.

## The spotter (`src/spotter.rs`)

What the fast-path can't `Clear` falls to the **spotter** before it reaches the human — a
one-shot, **fresh-context** model call (Haiku band, on its own `Client`, model-independent
of the acting session) that re-decides the residual. Independence *is* the product: the
acting session can't check its own folly because it shares its own trajectory; the spotter
can, precisely because its context is built from scratch — *not* `project(log)`.

**The collaborative frame is load-bearing.** The spotter is the climber's *spotter*, not a
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
a graded [`Stance`] (`clear` / `watch` / `gate`) plus a one-line `reason` addressed to the
session as a teammate. `Clear` auto-approves; `Watch` and `Gate` both route to the human
gate today (the watch-window that gives `Watch` its own intervention latency is a later unit).

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
takes `<path>:rw` / `<path>:r`).

## The config-edit loop (`src/policy.rs`, `src/agent.rs`, `src/config.rs`, `src/daemon.rs`)

A deny is not a dead-end: the orchestrator can **edit the config** at a gate. A
`Payload::ConfigEdit` carries one `ConfigChange` (`command` / `path` / `tool` / `scope`) and
a `Lifetime` (`session` or `persist`). The **effective config** the cascade consults is
`base config + fold(config-edits)` — `policy::effective_config` is the batch fold (startup /
resumed log); the daemon then maintains it **incrementally** (`policy::apply_config_change` on
each `ConfigEdit` append) so a gate reads current policy in O(1). The fold is
**replace-at-key, last-write-wins** — materializing a table from an ordered log, exactly as a
database replays its write-ahead log. So **order matters** (the prior list-append fold was
order-independent), but it stays idempotent for today's changes: re-applying a change already
in the file *and* the log lands the same value, and replace-at-key admits *narrowing* a key,
which append couldn't.

The human upgrades a gate with a fourth `Approval::ConfigEdit` decision. The loop **commits**
(`agent::commit_config_edit`): append the `Payload::ConfigEdit`, optionally persist it to the
TOML file (`config::persist_config_change` — a format-preserving `DocumentMut` edit that sets
the key while leaving every comment and unrelated line untouched, so "yes, always" auto-persist
is sound for *every* slot, including a single `scope` narrowing), then **re-classify this call
against the new policy**. It runs only if the changed rule now clears it; otherwise it stays
gated (a redirect). The orchestrator changes the *rule*; the mechanism authorizes the *call* —
nothing rides along unchecked.

A `ConfigEdit` projects to nothing on the wire (audit-only, like `Spot`). Its effect reaches
the model **by key**: a `commands`/`tools` edit only through the classifier, but a
**session-visible** edit (`scope`, `paths`) *also* surfaces as an `Ambient` notification.
`Shared::append` is the seam — it already folds each `ConfigEdit` into the effective config, so
it also raises the notification there (`session_visible_notice`), which means **every**
initiator gets it for free. The notification is an *optional tail append* through the existing
inbox/triage machinery: the session learns its perimeter moved without the boot preamble ever
being re-rendered (live truth = `preamble + fold(notifications)`, the substrate thesis). The
spotter, fresh-context each call, just reads the effective scope directly.

This is the **first initiator** of the config-edit loop (the human upgrading a blocking gate),
plus the session-visible-edit notification. The others — the spotter's non-blocking bash-prefix
promotion, the session's own `propose_scope`, and fs-watch of the config file — are not built
yet. See [`../backlog/permissions/config-edit.md`](../backlog/permissions/config-edit.md).

## What is not built yet

The watch-window, the spotter's effect-preview, and the rest of the config-edit loop (spotter
prefix-promotion, the `propose_scope` tool, the autonomous mode, fs-watch). `Stance::Watch`
resolves to the human gate today; the watch-window will map it to an intervention latency
instead. The spotter currently judges the raw command + config context; a neutral
effect-preview is the planned input upgrade. Fleet-wide persona caching (a second breakpoint
isolating the persona so it's shared across differently-scoped sessions) is a deferred
optimization — per-session caching works today. See
[`../backlog/permissions/README.md`](../backlog/permissions/README.md).

[`toml_edit`]: https://docs.rs/toml_edit
[`DocumentMut`]: https://docs.rs/toml_edit/latest/toml_edit/struct.DocumentMut.html
[`Config`]: ../src/config.rs
