# Permissions — role config & the approval fast-path

A session is parameterized by `(role-config, cwd, orchestrator)`. The **role** is the
portable-policy half: what a session is *for* and what it may do, kept separate from cwd
(the per-invocation spatial scope, see [env](../env.rs) / the system preamble) so the
role stays portable. Policy is **data the classifier interprets**, never special-cased in
code — there are no privileged program names or directories in the mechanism, only config
the mechanism reads.

This doc covers what is built now: the role config (`src/role.rs`) and the deterministic
approval fast-path (`src/policy.rs`), wired into the turn loop's gate (`src/agent.rs`).
The fuller cascade — an independent judge, a watch-window, a scope-change petition loop —
is planned; see [`../backlog/permissions/`](../backlog/permissions/).

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

## The role config (`src/role.rs`)

A KDL document, parsed into typed structs by [`knus`]; the types *are* the schema. A
malformed file surfaces a `knus::Error` (a `miette::Diagnostic`) rendered with a
source-span underline — the load-time diagnostic that stands in for KDL's thin editor
tooling. Resolution: an explicit `serve --config <path>` (which must exist), else the
`efference.kdl` probe (absent → the empty default role, so nothing auto-clears — safe).
The resolved source is reported on `GET /state` so "which role am I running?" is never
ambiguous.

For now there is **exactly one config file per session** — no built-in base, no merge.
"Merge by copy-and-edit" holds until usage patterns settle the real merge semantics; the
`extends` ancestor pointer is parsed but the merge engine is deliberately unbuilt. What
the role holds:

- **`brief`** — positive scope, *shared with the session*. It projects into the cached
  preamble (`Role::preamble_section` → `env::system_prompt`). The projection must be
  **byte-stable for the session** (the same caching invariant as `project(log)`, see
  [substrate](substrate.md)): it is rendered once at startup from the brief text and
  frozen. Changing roles is a cold cache write, which is fine — roles do not change
  mid-session.
- **`judge-rubric`** — the red-flag rubric, *judge-only*. Kept off the session's preamble
  so the session cannot teach to the test. Consumed by the (unbuilt) judge rung.
- **`allow "prog" "arg" …`** — the bash prefix-allowlist (below).
- **`tool "name"`** and **`path "." read=true write=true`** — the structured-tool and
  filesystem grants the fast-path reads (below).
- **`trust-threshold`** — where the watch-window/gate cutoffs will sit. Parsed, unused.

## The fast-path (`src/policy.rs`)

`classify(&Role, &ToolCall) -> Verdict` is a **pure function** — no I/O, no model —
returning `Clear` (auto-approve: no human gate, no model round-trip) or `Gate` (fall
through to the human gate today, the judge later). The loop consults it just before the
gate: `Clear` becomes an immediate approve; `Gate` calls `frontend.approve`. It is the
cheapest rung of the planned cascade, and it only ever *accepts what it is certain about*,
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
- **Stage 2** matches the lexed `[program, args…]` against the role's
  `(program, safe-arg-prefix)` allowlist: a token-wise prefix match, empty prefix =
  blanket-trust the program.

**Why a POSIX parser is the right choice for a security gate.** yash is POSIX-strict, not
bash-complete, which costs only *coverage* (a few bash-only forms — `[[ ]]`, `<(…)`,
`for…do` — gate to the human/judge), never *soundness*: bash's dangerous operators are
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
verbs (`git push`, `git reset`) fall through. Memoization (the eventual petition-fed
growth of this list) is sound only at **program-prefix granularity, never command
granularity** — safety is discontinuous in the command string (`./build` vs `/build`), so
exact-command memoization is what strands dead entries and is anti-correlated with safety.

### structured tools: name grant + path permission

A structured (non-bash) tool clears only if its name is in the role's `tool` grants
**and**, for the path-bearing ones, its target resolves under a `path` grant with the
needed permission — reads (`read`/`list`/`search`/`read_node`) need `read`, writes
(`write`/`preview_edit`) need `write` (`write` implies `read`). Nothing clears in code; an
empty role gates every tool, exactly as before.

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

## What is not built yet

The independent judge, the scope-change petition loop, and the watch-window. The
fast-path's `Verdict` is binary today precisely as the extension point: the judge will
split `Gate` into judge → {clear, watch, gate}. Fast-path clears are intentionally **not**
logged (deterministic, zero-cost; the `tool_use`/result already land in the log); the
judge's verdicts are what will earn a `Triage`-kin audit entry. Petition-fed growth of the
bash allowlist waits on the petition loop. See
[`../backlog/permissions/README.md`](../backlog/permissions/README.md).

[`knus`]: https://docs.rs/knus
