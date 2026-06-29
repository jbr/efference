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

A KDL document, parsed into typed structs by [`knus`]; the types *are* the schema. A
malformed file surfaces a `knus::Error` (a `miette::Diagnostic`) rendered with a
source-span underline — the load-time diagnostic that stands in for KDL's thin editor
tooling. Resolution: an explicit `serve --config <path>` (which must exist), else the
`efference.kdl` probe (absent → the empty default config, so nothing auto-clears — safe).
The resolved source is reported on `GET /state` so "which config am I running?" is never
ambiguous.

For now there is **exactly one config file per session** — no built-in base, no merge.
"Merge by copy-and-edit" holds until usage patterns settle the real merge semantics; the
`extends` ancestor pointer is parsed but the merge engine is deliberately unbuilt. What
the config holds:

- **`scope`** — the positive perimeter, *shared with the session*. It projects into the
  cached preamble (`Config::preamble_section` → `env::system_prompt`). The projection must
  be **byte-stable for the session** (the same caching invariant as `project(log)`, see
  [substrate](substrate.md)): it is rendered once at startup from the scope text and
  frozen. Changing the config is a cold cache write, which is fine — the config does not
  change mid-session. (Live, mid-session scope edits surface as notifications instead of
  re-rendering the preamble; that's planned, not built — see config-edit.md.)
- **`watch-for`** — the "things to watch for" list, *spotter-only*. Kept off the session's
  preamble for **attention-division** (not anti-gaming — the threat model is error, not
  deception, so there's no test to game): the focused session shouldn't carry low-probability
  peripheral hazards in working memory; the spotter holds them. Consumed by the spotter rung.
- **`allow "prog" "arg" …`** — the bash prefix-allowlist (below).
- **`tool "name"`** and **`path "." read=true write=true`** — the structured-tool and
  filesystem grants the fast-path reads (below).
- **`trust-threshold`** — where the watch-window/gate cutoffs will sit. Parsed, unused.

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
`allow "git"` survivable in principle and a sub-verb `allow "git" "diff"` sound.

- **Stage 1** parses the command with `yash-syntax` — a real POSIX parser, not a regex —
  and accepts only a *single simple command*: no operators (`| & ; && ||`), no
  redirection, no command/arithmetic substitution, no parameter/tilde expansion, no
  env-assignment prefix, no unquoted glob/brace. The parser is needed (not a regex)
  because safety is quote-sensitive: the `|` in `rg 'a|b' src/` is one literal argument,
  not a pipe. yash *parses* without *expanding*, so we inspect word units ourselves for
  the expansion metacharacters bash would act on at runtime (`* ? [ {` anywhere; a leading
  `~`) — which is why `git diff HEAD~1` clears but `~/x` and `cp {a,b}` gate.
- **Stage 2** matches the lexed `[program, args…]` against the config's
  `(program, safe-arg-prefix)` allowlist: a token-wise prefix match, empty prefix =
  blanket-trust the program.

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

The allowlist is curated along the reversibility axis: blanket `allow "cargo"` only for
programs whose *whole* arg-space is recoverable; sub-verb prefixes (`allow "git" "diff"`,
`allow "git" "status"`) for programs with an irreversible sub-space, letting the dangerous
verbs (`git push`, `git reset`) fall through. Memoization (the eventual config-edit-fed
growth of this list) is sound only at **program-prefix granularity, never command
granularity** — safety is discontinuous in the command string (`./build` vs `/build`), so
exact-command memoization is what strands dead entries and is anti-correlated with safety.

### structured tools: name grant + path permission

A structured (non-bash) tool clears only if its name is in the config's `tool` grants
**and**, for the path-bearing ones, its target resolves under a `path` grant with the
needed permission — reads (`read`/`list`/`search`/`read_node`) need `read`, writes
(`write`/`preview_edit`) need `write` (`write` implies `read`). Nothing clears in code; an
empty config gates every tool, exactly as before.

Two properties are load-bearing. **Grants are additive subtrees with no deny.** A `path`
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
(fresh)]`. The scope block is the config's scope prose plus the `path` grants + cwd, so the
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
spotter --brief … --watch-for … --path … "<cmd>"` runs the exact `assess` path on one move,
for iterating on the persona and config against real cases (the `--brief` flag feeds the
`scope` field; its rename to `--scope` rides with the TOML migration — see config-edit.md).

## The config-edit loop (`src/policy.rs`, `src/agent.rs`, `src/config.rs`)

A deny is not a dead-end: the orchestrator can **edit the config** at a gate. A
`Payload::ConfigEdit` carries one *additive* `Amendment` (`allow` / `path` / `tool`) and a
`Lifetime` (`session` or `persist`). The **effective config** the cascade consults is
`base config + fold(config-edits)` — `policy::effective_config` is the batch fold (startup /
resumed log); the daemon then maintains it **incrementally** (`policy::apply_config_edit` on
each `ConfigEdit` append) so a gate reads current policy in O(1). Amendments are additive
only — the fold is order-independent and idempotent, so a config edit replayed (already in
the config file *and* the log) is harmless.

The human upgrades a gate with a fourth `Approval::ConfigEdit` decision. The loop **commits**
(`agent::commit_config_edit`): append the `Payload::ConfigEdit`, optionally append-persist it
to the KDL config (`config::persist_grant` — v1 syntax, comment-preserving, deduped), then
**re-classify this call against the new policy**. It runs only if the amended rule now clears
it; otherwise it stays gated (a redirect). The orchestrator changes the *rule*; the mechanism
authorizes the *call* — nothing rides along unchecked. A `ConfigEdit` projects to nothing on
the wire (audit-only, like `Spot`); its effect reaches the model only through the classifier.

This is the **first initiator** of the config-edit loop (the human upgrading a blocking gate).
The others — the spotter's non-blocking bash-prefix promotion, the session's own
`propose_scope`, and fs-watch of the config file — are not built yet. See
[`../backlog/permissions/config-edit.md`](../backlog/permissions/config-edit.md).

## What is not built yet

The watch-window, the spotter's effect-preview, and the rest of the config-edit loop (spotter
prefix-promotion, the `propose_scope` tool, the autonomous mode, fs-watch). `Stance::Watch`
resolves to the human gate today; the watch-window will map it to an intervention latency
instead. The spotter currently judges the raw command + config context; a neutral
effect-preview is the planned input upgrade. Fleet-wide persona caching (a second breakpoint
isolating the persona so it's shared across differently-scoped sessions) is a deferred
optimization — per-session caching works today. See
[`../backlog/permissions/README.md`](../backlog/permissions/README.md).

[`knus`]: https://docs.rs/knus
