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

## The identity line

The very first line of the system prompt is `You are {model}.` — the configured
model verbatim, unconditionally (`src/env.rs`). It leads the prompt so the model
knows what it is before reading anything else: the harness instructions, the
`[context]` block, and the role all follow. An untold model answers "which model
are you" from its training priors, usually wrongly, and a log that mixes models
across a session needs the current one to know which earlier turns were not its
own. The line is also where a knowledge-cutoff note would attach; provider
metadata has a slot for one (OpenRouter's `knowledge_cutoff`) that is
unpopulated today, so the cutoff is not rendered
([../backlog/model-identity.md](../backlog/model-identity.md) holds the
render-when-present remainder).

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
"Skills" are never tool concepts, just rows an operator wrote. `create`, `sort`, and
`description` are group-only (they are about a location of many files); a directory
expands to every `.md` file **recursively** beneath it — which is what makes a
Claude-Code-style skills tree (`skills/<name>/SKILL.md`, one directory per skill) land as
one listing per skill — skipping hidden children, following symlinks with a cycle guard
(a symlinked skill collection is a real layout), default order `name asc` by display path
(the SQL-ish `sort` override is parsed but not yet applied). Validation fails closed on
the nonsense shapes (`false` entries, group-only keys on a bare path, `exec` mixed with
files).

`description` carries a prose hint for the model about how to use the group's files —
e.g. for a memory store whose files are read by path with the `read` tool. It renders as a
`<description>` child element inside the group wrapper, before the file listing. It has no
meaning to the harness; the model decides what to do with it.

## Mode is the file's call; grants are the operator's

Two orthogonal axes land on two authorities on purpose:

- **Inlined vs. listed** is decided by the *file*, via frontmatter (`src/context.rs`). No
  frontmatter (or no `description`) → the whole body is **inlined** into the cached
  preamble — the CLAUDE.md role, always-on knowledge. A frontmatter `description` → only a
  one-line **stub** rides in the preamble (name, description, `Read {path} …`); a real
  skill file already has name+description, so it becomes a read-on-demand stub with no
  special handling. `inline: true` overrides a described file back to inlined. The
  frontmatter parser is minimal but handles what real skill files use: `key: value` lines
  plus YAML block scalars (`description: >-` folded over indented lines); nested
  structures are consumed, never misread as top-level keys.
- **Read-only vs. writable** (`update`/`create`) is decided by the *config*, because a
  file granting itself write access would be exactly the self-authorization the
  config-as-reviewed-file design forbids.

The reveal mechanism is **the filesystem, not a new tool**: every listed path is
read-allowed by default (the `[context]` rung in `src/policy.rs` — see
[permissions.md](permissions.md)), so a stub's `Read {path}` resolves through the `read`
tool the model already has. No `skill()` verb, no reveal machinery — progressive doc
reveal reduces to a read grant. A group with `create` *is*
a memory store: descriptions are the index, `read` is recall, a write is a save, and
`create` without `update` is an append-only store — the same shape as the log.

## Render — pseudo-XML name tags (provisional)

`context::Resolved::render` renders each file as a `<name>…</name>` tag — the operator's
label (when a labeled entry resolves to one file), else the frontmatter `name`, else the
basename — and wraps a multi-file group in a verbatim `<Heading>` tag. When the group
carries a `description`, it renders as a `<description>…</description>` child element
before the file listing. Inlined bodies ride verbatim inside their tag, so a body's own
`#`/`---`/``` cannot blur a boundary. The
format is byte-pinned and deliberately cheap to flip: whether tags actually beat markdown
here is an unverified assumption, recorded for A/B in
`backlog/preamble-format-experiment.md`. The block splices into the system prompt after
the curation preamble and before the role scope (*what it may do*) — knowledge before
perimeter (`env::system_prompt`).

## Exec sections — a section whose body is a command's stdout

`Label = { exec = "…" }` is just another row: the command runs **once at boot** (`bash
-c`, session cwd — the same entrypoint as the bash tool), its stdout becomes the section
body at the entry's position in document order, frozen and snapshotted like everything
else. That per-boot-not-per-turn semantics is the only coherent one for a frozen cached
prefix: `Date = { exec = "date" }` honestly means "the date as of session boot." Bash as
the entrypoint means the content can come from anything the operator can script; volatile
stdout costs cross-boot cache sharing, an opt-in the operator wrote. It fails closed like
a file would — a spawn failure, non-zero exit (stderr quoted), a 10-second timeout (the
child's process group is killed so a hung command can't hang boot), or non-UTF-8 stdout
all fail the load. An exec runs with harness privileges, ungated: the operator authored
the config, so gating their own boot command would be theater — but it *is* an
arbitrary-code-execution surface at session start, stated plainly.

**Orientation is an exec entry, not code.** The once-hardcoded orientation block (cwd,
platform, project type) is gone from `src/env.rs`; *where the session is* is now an
ordinary config line — the scaffold written for a config-less session ships
`"Environment at boot" = { exec = 'echo "working directory: $PWD"; …' }`, so a bare
session still boots oriented, visibly and editably, and a written config owns the whole
preamble (omitting the entry omits the block — explicit over magic).

## Boot discipline: freeze, snapshot, fail closed

- **Read once at boot, frozen.** Bodies and stubs are resolved from disk once and frozen
  into the cached prefix; a later edit to a listed file never re-renders the preamble
  (the same discipline as scope: never mutate behind a cache breakpoint).
- **Snapshotted into the log.** Because the preamble folds in file contents that drift on
  disk, it is no longer reconstructable from `config + current files` — so `serve` appends
  the full rendered system prompt as a `Payload::Preamble` entry once at boot
  ([log-and-projection.md](log-and-projection.md)). The log stays the complete record; `efference wire`
  reads the snapshot back instead of re-resolving; and once order is baked into the
  snapshot, sort order is a pure semantic choice, never a stability constraint. The entry
  also carries the **structured** `Resolved` listing (the `Section`/`Item` tree) beside the
  flat string — additive, so the byte-exact string stays the wire truth while the structure
  survives as a fold for the surfacing rider ([redaction-fork.md](redaction-fork.md)).
- **Fail closed, loudly.** A named file that can't be read, or an unterminated
  frontmatter block, fails startup — silently booting without configured context would be
  the model running with knowledge it was told it had.

## Deferred

Live mid-session discovery of context files (`backlog/live-context-update.md`), and
globs + the `sort` override + the fallback-tag-name call
(`backlog/context-globs-and-sort.md`).
