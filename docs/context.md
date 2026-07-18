# The `[context]` preamble builder — what the session boots knowing

A Claude Code session on a repo boots with the whole architecture map from `CLAUDE.md`;
without this subsystem an efference session got only the orientation block (cwd, platform,
project type) and re-discovered the repo from scratch every session. The `[context]` table
in `efference.toml` closes that gap: **the operator declares what knowledge the session
boots with — nothing is auto-discovered.** No `EFFERENCE.md` convention, no auto-loaded
`CLAUDE.md`, no scanning for skill files. The one config file is the single source of
truth for what the session starts knowing, the same way it already is for the session's
perimeter. Schema in `src/config.rs`; resolution and render in `src/context.rs`; splicing
in `src/env.rs`; the read/write grants in `src/policy.rs`.

## One mechanism, three config shapes

CLAUDE.md-alikes, "skills", and "memories" are one mechanism: markdown files the operator
lists, each self-describing via frontmatter. A `[context]` entry takes one of three
shapes, disambiguated by its value:

```toml
[context]
"CLAUDE.md"       = true                                       # bare path — read-only
"ARCHITECTURE.md" = { update = true }                          # bare path — read + update
Memories = { path = "memories/", create = true }               # labeled group
Skills   = { paths = ["skills/a.md", "skills/b.md"] }          # labeled group, explicit list
```

A **bare path** (the key is the path) renders with no heading and may carry only
`update`. A **labeled group** (the value carries `path`/`paths`) points at a location; its
key is a heading — *pure presentation*, zero meaning to the harness. "Memories" and
"Skills" are never tool concepts, just rows an operator wrote. `create` and `sort` are
group-only (they are about a location of many files); a directory expands to its `.md`
children, default order `name asc` (the SQL-ish `sort` override is parsed but not yet
applied). Validation fails closed on the nonsense shapes (`false` entries, group-only keys
on a bare path, `exec` mixed with files).

## Mode is the file's call; grants are the operator's

Two orthogonal axes land on two authorities on purpose:

- **Inlined vs. listed** is decided by the *file*, via frontmatter (`src/context.rs`). No
  frontmatter (or no `description`) → the whole body is **inlined** into the cached
  preamble — the CLAUDE.md role, always-on knowledge. A frontmatter `description` → only a
  one-line **stub** rides in the preamble (name, description, `Read {path} …`); skills
  compatibility falls out, since a real skill file has name+description and becomes a
  read-on-demand stub. `inline: true` overrides a described file back to inlined.
- **Read-only vs. writable** (`update`/`create`) is decided by the *config*, because a
  file granting itself write access would be exactly the self-authorization the
  config-as-reviewed-file design forbids.

The reveal mechanism is **the filesystem, not a new tool**: every listed path is
read-allowed by default (the `[context]` rung in `src/policy.rs` — see
[permissions.md](permissions.md)), so a stub's `Read {path}` resolves through the `read`
tool the model already has. No `skill()` verb, no reveal machinery — this is the README's
"progressive doc reveal" behavior falling out of a read grant. A group with `create` *is*
a memory store: descriptions are the index, `read` is recall, a write is a save, and
`create` without `update` is an append-only store — the same shape as the log.

## Render — pseudo-XML name tags (provisional)

`context::Resolved::render` renders each file as a `<name>…</name>` tag — the operator's
label (when a labeled entry resolves to one file), else the frontmatter `name`, else the
basename — and wraps a multi-file group in a verbatim `<Heading>` tag. Inlined bodies ride
verbatim inside their tag, so a body's own `#`/`---`/``` cannot blur a boundary. The
format is byte-pinned and deliberately cheap to flip: whether tags actually beat markdown
here is an unverified assumption, recorded for A/B in
`backlog/preamble-format-experiment.md`. The block splices into the system prompt after
orientation (*where* the session is) and before the role scope (*what it may do*) —
knowledge between the two (`env::system_prompt`).

## Boot discipline: freeze, snapshot, fail closed

- **Read once at boot, frozen.** Bodies and stubs are resolved from disk once and frozen
  into the cached prefix; a later edit to a listed file never re-renders the preamble
  (the same discipline as scope — mutating behind a cache breakpoint is the cardinal sin).
- **Snapshotted into the log.** Because the preamble folds in file contents that drift on
  disk, it is no longer reconstructable from `config + current files` — so `serve` appends
  the full rendered system prompt as a `Payload::Preamble` entry once at boot
  ([substrate.md](substrate.md)). The log stays the complete record; `efference wire`
  reads the snapshot back instead of re-resolving; and once order is baked into the
  snapshot, sort order is a pure semantic choice, never a stability constraint.
- **Fail closed, loudly.** A named file that can't be read, or an unterminated
  frontmatter block, fails startup — silently booting without configured context would be
  the model running with knowledge it was told it had.

## Deferred

Live mid-session discovery of context files (`backlog/live-context-update.md`), `exec`
sections whose body is a command's stdout (`backlog/dynamic-context-exec.md` — parsed,
skipped by `resolve`), and globs + the `sort` override.
