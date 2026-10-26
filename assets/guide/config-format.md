# The `efference.toml` format

Every key is optional. Unknown keys are a hard error — a typo fails the whole file rather
than being silently ignored, and so does any entry the harness cannot read soundly.

A first line of `#:schema https://efference.net/schemas/project.v1.json` gives an editor
running a TOML language server (Taplo, Tombi) the file's schema: completion, a description
of each key on hover, and a warning on a misspelled key before efference reads the file. The
editor fetches the schema from efference.net, so efference.net sees a request from the
operator's machine; deleting the line stops that, and nothing else reads it.

`consolidation-bytes = 65536` sets the opportunistic consolidation threshold:
UTF-8 tool argument JSON plus post-triage projected result text in the newly
eligible working span. Default: 64 KiB. Images and pure dialogue do not count.
Zero checks any nonempty tool-work budget; it does not force an account.

## Top-level keys

```toml
scope = "Work only inside this repo."          # what this session is for
watch-for = ["network egress to non-registry hosts"]
trust-threshold = 0.5
first-turn = "Summarize what changed since the last session."
tools = ["read", "write", "ferritin.*"]
extends = ["base.toml"]
```

- **`scope`** — a string (not a `[scope]` table). It is rendered into the session's frozen
  preamble, and it is the authority the permission spotter judges a move against: the
  question at that rung is "is this inside the mission", not "is this dangerous".
- **`watch-for`** — hazards the spotter should look out for. Spotter-only; deliberately
  never shown to the session.
- **`trust-threshold`** — parsed, reserved for the watch-window; nothing reads it yet.
- **`first-turn`** — a user turn the harness sends when a session starts with nothing yet
  said, so the agent speaks first. Like the preamble it opens the session rather than being
  a turn: the clients do not draw it, and it arrives marked `source: efference`. Read once at
  boot. Omit it for a session that waits for the operator.
- **`tools`** — the structured tools a call may use without review. Matching is exact, with
  one glob: a terminal `.*` grants a whole namespace (`ferritin.*` for that server's tools,
  `efference.*` for every tool the harness itself ships). A star anywhere else fails the
  file. A connected server's tool may be written either as the harness spells it on the wire
  (`ferritin__get`) or in the dotted display form (`ferritin.get`).
- **`extends`** — parsed as an ancestor pointer; the merge engine is not built, so it does
  nothing today.

## `[commands]` — bash prefixes

```toml
[commands]
cargo = true            # blanket: the whole program
"git diff" = true       # one sub-verb
git = "clear"           # `true` is sugar for this
"git push" = "deny"     # a carve-out inside the broader grant
```

Keys are command prefixes; values are one of `clear`, `watch`, `gate`, `deny` (`true` =
`clear`, `false` = `deny`). Resolution is **longest-prefix-wins**, so a broad grant can
carry named exceptions and nesting is unbounded. A key that is not a single simple command
(`"git | rm"`) fails the whole file.

`clear` skips both the spotter and the human. `watch` and `gate` send the move to the rung
that gates it. `deny` skips the spotter and goes straight to the operator, who can still
allow it; only an autonomous session declines it outright.

A grant here only matters once the command is structurally simple: pipes, substitutions,
globs, expansions, and env-assignment prefixes are rejected *before* any prefix is matched,
so a blanket grant can never clear `cargo … > /etc/hosts`. Two shapes are tolerated: a
trailing file-descriptor duplication (`2>&1`), and a single leading `cd <dir> &&` whose
target is read-granted.

## `[paths]` — filesystem grants

```toml
[paths]
"." = true                   # sugar for { read = true, write = true }
"/refs" = { read = true }
"secrets" = false            # a deny once it is the governing prefix
```

Each key is a subtree. Resolution is longest-prefix-wins, so a deeper key carves an
exclusion out of a broader one, and `write` implies `read`. Matching is lexical (`.` and
`..` collapsed without touching the filesystem), so a symlink out of a granted tree is
treated as inside it: these are guardrails against a mistake, not access control.

A path-bearing tool clears only when its name is in `tools` **and** its target is granted
here. A shell command's arguments are not judged by this table — a shell command answers
to `[commands]` and the spotter — except for two shapes: a leading `cd <dir> &&` needs the
directory read-granted, and a single `>` or here-doc write needs its target write-granted.
So a deny here guards the structured tools against a mistaken call and little else. `.efference/` is denied by the harness underneath this table, with the
session's own `config.toml` re-allowed read-only; a template needs no entry for it.

## `[context]` — what the session boots knowing

Nothing is auto-discovered; this table is the whole of it. A value's shape picks the kind:

```toml
[context]
"CLAUDE.md"       = true                                  # bare path, read-only
"ARCHITECTURE.md" = { update = true }                      # bare path, writable
Memories = { path = "memories/", create = true, description = "…" }
Skills   = { paths = ["skills/a.md", "skills/b.md"] }
Runbooks = { path = "runbooks/", exclude = ["archive"] }
"Environment at boot" = { exec = 'echo "cwd: $PWD"' }
EfferenceGuide = { builtin = "guide" }
```

- A **bare path** (the key is the path) renders with no heading and may carry only
  `update`.
- A **labeled group** (the value carries `path` or `paths`) uses its key as a heading —
  pure presentation, with no meaning to the harness. `create`, `sort`, `exclude`, and
  `description` are group-only. A directory expands to every `.md` file beneath it,
  recursively.
- An **`exec` entry**'s body is the command's stdout, run once at boot under `bash -c` in
  the session's directory and then frozen. `Date = { exec = "date" }` therefore honestly
  means "the date as of session boot". It runs with harness privileges and is not gated.
- A **`builtin` entry** names documents compiled into the binary — the ones you are
  reading now. `"guide"` lists them all as stubs; `"guide/<slug>"` puts that one document's
  whole body in the preamble under the entry's label (`{ builtin = "guide/new-project" }`).

Whether a listed file's **whole body** or a **one-line stub** rides in the preamble is the
file's own call, via YAML frontmatter: no frontmatter `description` means the body is
inlined; a `description` means only a stub rides and the body is fetched with `read`.
`inline: true` forces a described file back to inlined.

Whether it is **writable** is the config's call: `update` allows modifying files that
exist, `create` allows adding new ones, and `create` without `update` is an append-only
store. Every listed path is read-granted with no `[paths]` entry needed.

Everything here is read once at boot and frozen — editing a listed file mid-session does
not re-render the preamble — and a listed file that cannot be read fails startup.

## `[env]` — per-variable environment policy

```toml
[env]
"*_KEY"    = { mask = true }        # never store its value in the log
"CI_*"     = { strip = true }       # keep it out of spawned children
RUST_LOG   = { value = "debug" }    # a literal value
SOME_KEY   = "op://Vault/Item/field"  # resolved at boot, always masked
OTHER      = { cmd = "…" }          # stdout of a command
```

Keys are exact names or `*` globs, with exact beating glob. `*_KEY`, `*_SECRET`,
`*_TOKEN`, `*_PASSWORD`, and `*_CREDENTIALS` are masked by default. A value the harness
fetched for the session is masked whatever `mask` says.

## `[bash]`, `[transport]`

```toml
[bash]
background-after = 30        # seconds a foreground command may block before it detaches
background-after-max = 600   # ceiling on what one call may ask for

[transport]
stream-ceiling = 300         # seconds one response's stream may stay open
keepalive-interval = 240     # seconds the prompt cache may go untouched at idle; 0 = no ping
effort = "high"              # e.g. low/medium/high/xhigh/max; boot-validated against the model's own list; omit for the provider default (printed at boot)
```

The keepalive only runs at all if the model's provider metadata reports a prompt
cache behind it; the interval says how often to touch it, and `0` turns it off.

`effort` is how much effort the model spends per turn, a separate axis from
whether it thinks. The value is accepted as given — there is no fixed vocabulary
— and validated at session start against the levels the model's metadata
declares: on OpenRouter a level the model does not declare is refused at boot
with the supported list (OpenRouter would otherwise map it to the nearest
silently), and the provider default is printed on the boot line when nothing is
set. Anthropic's metadata names no levels, so a misspelled one surfaces as the
provider's own 400 on the first turn. The `--effort` flag at session creation overrides this key, both
are read once at session start, and neither can change mid-session. `xhigh` and
`max` require thinking, so pairing either with `EFFERENCE_THINKING=off` is
refused at startup.

## `[mcp.servers.<name>]` — connected providers

```toml
[mcp.servers.ferritin]
command = "ferritin"
args = ["mcp"]

[mcp.servers.sentry]
url = "https://mcp.sentry.dev/mcp"
optional = true
```

Exactly one of `command` (a stdio subprocess, which may carry `args` and `env`) or `url` (a
streamable-HTTP endpoint, which may not) is required. Names must fit `[a-zA-Z0-9_-]`, be at
most 32 characters, and contain no `__`, because a server's tools reach the wire as
`<server>__<tool>`.

A declared server that does not mount **stops the boot**, naming the server and the reason.
`optional = true` opts out; an optional server that fails is reported to the operator on
stderr and to the session in the system prompt, so neither party mistakes it for a
capability that was never configured.
