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

The very first line of the system prompt is, unconditionally (`src/env.rs`):

```
You are {model}. This session is `{id}` on this host; that is the id other agents use to reach you.
```

The configured model verbatim, then the session's id — its session dir's name,
the same string the control channel addresses it by. The line leads the prompt so
the model knows what it is before reading anything else: the harness
instructions, the `[context]` block, and the role all follow. An untold model
answers "which model are you" from its training priors, usually wrongly, and a
log that mixes models across a session needs the current one to know which
earlier turns were not its own. An untold session either invents an id or
hand-carries one through prose — a delegated task description observed typing
"My id lucid-lark" as a return address.

The two halves are in **different grammar on purpose**: one identity claim (`You
are {model}.`) and one possession (`This session is {id}`). Two identity claims
in consecutive sentences — `Your name is …`, a second `You are …` — are resolved
by treating the generated id as a persona to inhabit. The session has an id; the
model is not it. The noun is **id** because the rest of the agent-facing surface
already uses it: turns arrive prefixed `agent[<id>]:`
(`assets/prompts/session.md`) and every control tool takes a `session` id;
*address* stays the explanatory gloss rather than a second noun. The id is fixed
for the session's life, so the sentence is byte-stable in the cached prefix, and
its exact bytes are pinned in `src/env/test.rs`.

## The creator's id

When a session commissioned this one, one further sentence follows on the same
line (`src/env.rs`):

```
You are {model}. This session is `{id}` on this host; that is the id other agents use to reach you. It was created by `{created_by}`, which you can reach the same way.
```

It renders only when the host has a `created_by` for this session
([tools.md](tools.md)) — a root session's identity line ends at its own
id, with no sentence and no placeholder. The value comes from the host's own
record, plumbed through `SessionRequest` into the preamble at boot: never prompt
text, never an id read out of an upstream log, so nothing a turn says can change
who a session believes made it. Detaching changes only whether the child ends
with its creator, not who created it, so a detached child renders the same
sentence.

What the sentence says is a return address and nothing more. It does not claim
the creator is still running — it may have ended — and it grants no authority in
either direction; who may drive whom is the reach rule in
[tools.md](tools.md), enforced at the tool, not something the preamble confers.
Liveness and what the creator is doing are *changing* peer status, and the
identity line sits in the frozen cached prefix: putting either there would move
the prefix mid-session. Status reaches a session through turns and inbox
notifications instead ([inbox.md](inbox.md)). The creator id is immutable for the
session's life, which is what makes it safe to freeze.

A session made by `hand_off` ([tools.md](tools.md)) has a second provenance sentence,
from the same `Lineage` (`src/env.rs`): "It succeeds `{predecessor}`, which handed this work
off to it: the first message is that session's brief — context from it, not the operator's
instruction. Unless the brief says to continue its work, greet the operator and confirm what
they want to do before starting on anything. Ask the predecessor anything the brief leaves
open with `notify`, and archive it with `archive` once you have no more questions for it."
The default is to wait because a brief read as a task is the likely failure — a small model
taking "they mentioned adding a legend" as an order — and a brief that means *continue* (a
successor picking up work mid-flight) says so. Unlike
the creator sentence it confers something — the one session outside its descendants this
session may archive — and it says so, because that is the step the successor is expected
to take. It is fixed for the session's life like the creator's, and absent otherwise.

`efference dev wire` rebuilding an old log cannot recover this sentence:
`created_by` is host control state and is never logged. Sessions recorded since
the boot-preamble snapshot read the stored `system` back verbatim and are
unaffected.

The line is also where a knowledge-cutoff note would attach; provider
metadata has a slot for one (OpenRouter's `knowledge_cutoff`) that is
unpopulated today, so the cutoff is not rendered
([../backlog/model-identity.md](../backlog/model-identity.md) holds the
render-when-present remainder).

## One mechanism, three config shapes

CLAUDE.md-alikes, "skills", and "memories" are one mechanism: markdown files the operator
lists, each self-describing via frontmatter. A `[context]` entry takes one of four
shapes, disambiguated by its value (the fourth, `builtin`, is the guide overlay below):

```toml
[context]
"CLAUDE.md"       = true                                       # bare path — read-only
"ARCHITECTURE.md" = { update = true }                          # bare path — read + update
Memories = { path = "memories/", create = true }               # labeled group
Skills   = { paths = ["skills/a.md", "skills/b.md"] }          # labeled group, explicit list
EfferenceGuide = { builtin = "guide" }                         # documents compiled into the binary
```

A **bare path** (the key is the path) renders with no heading and may carry only
`update`. A **labeled group** (the value carries `path`/`paths`) points at a location; its
key is a heading — *pure presentation*, zero meaning to the harness. "Memories" and
"Skills" are never tool concepts, just rows an operator wrote. `create`, `sort`,
`exclude`, and `description` are group-only (they are about a location of many files); a
directory expands to every `.md` file **recursively** beneath it — which is what makes a
Claude-Code-style skills tree (`skills/<name>/SKILL.md`, one directory per skill) land as
one listing per skill — skipping hidden children, following symlinks with a cycle guard
(a symlinked skill collection is a real layout), default order `name asc` by display path
(the SQL-ish `sort` override below reorders it). Validation fails closed on
the nonsense shapes (`false` entries, group-only keys on a bare path, `exec` mixed with
files).

`exclude` prunes the expansion: each entry names a path relative to the configured
directory (`exclude = ["MEMORY.md", "archive"]` drops that file and that subtree),
matched by whole path components during the walk — an excluded subtree is skipped
*unread*, so a file inside it that would fail the load cannot. It filters expansion only,
never an explicitly listed path. The motivating case is a directory whose layout another
tool owns — a Claude-Code memory store, say, whose index file duplicates the per-file
frontmatter this renderer already extracts and whose `archive/` is deliberately retired —
where the fix must not require restructuring a directory the harness doesn't own.

`description` carries a prose hint for the model about how to use the group's files —
e.g. for a memory store whose files are read by path with the `read` tool. It renders as a
`<description>` child element inside the group wrapper, before the file listing. It has no
meaning to the harness; the model decides what to do with it.

## `sort` — a group's files are rows

`sort = "<field> [asc|desc]"`, comma-separated, reorders a labeled group's files
(`src/context/sort.rs`). Each file is a **row**; its columns are its top-level frontmatter
keys plus two built-ins, `name` (the file's frontmatter `name`, else its basename) and
`modified` (its mtime). A frontmatter key of the same name wins over the built-in.

- **Typing is per column, over the whole group.** If every row that has a value for the
  column parses as a number, the column compares numerically; one non-numeric value makes
  the whole column lexical. Nothing is declared — the data decides.
- **Missing sorts last, in both directions** (NULLS LAST): direction reorders the rows that
  have a value; it does not promote the ones that don't.
- **An implicit final `name asc`** breaks ties, then the display path, so the order is total
  and the resolved listing is reproducible.
- A spec that isn't `<field> [asc|desc]` **fails the load closed** — a misspelled sort
  quietly reverting to the default would be an order the operator did not write.

Sort order is a pure semantic choice, never a stability constraint: the preamble is read
once at boot and the resolved order is frozen into the `Payload::Preamble` snapshot, so
changing a `sort` can only affect the next session's prefix.

The frontmatter parser stays the minimal one (`src/context/frontmatter.rs`) and keeps
every top-level scalar key verbatim as a string column. A full YAML engine would buy typed
values these semantics don't ask for — the numeric/lexical call is made from the strings,
per column — so the dependency waits for a need that nested keys or real YAML types create.

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
**full basename**, extension included (`<CLAUDE.md>`, not `<CLAUDE>`: the tag then names a
file the model can `read`) — and wraps a multi-file group in a verbatim `<Heading>` tag.
When the group
carries a `description`, it renders as a `<description>…</description>` child element
before the file listing. Inlined bodies ride verbatim inside their tag, so a body's own
`#`/`---`/``` cannot blur a boundary. The
format is byte-pinned and deliberately cheap to flip: whether tags actually beat markdown
here is an unverified assumption, recorded for A/B in
`backlog/preamble-format-experiment.md`. The block splices into the system prompt after
the curation preamble and before the role scope (*what it may do*) — knowledge before
perimeter (`env::system_prompt`). The curation preamble itself (`assets/prompts/session.md`)
states the turn-attribution convention in one sentence, so a prefix is read as an
address rather than as content: a user turn may open with a `source:` prefix naming
who submitted it, and a turn with no prefix is from the operator
(docs/control-api.md).

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

## The guide overlay — a fourth entry shape whose files are in the binary

`Label = { builtin = "guide" }` lists a set of documents compiled into the binary
(`src/guide.rs`, sources in `assets/guide/` plus [operating.md](operating.md)). The one
mount name is `guide`; any other fails the file rather than rendering an empty section.

They are **born resolved**: each document is a compiled-in `(slug, description, body)`
triple that enters as a `Resolved::Item::Listed` directly, so the entry reuses the *render*
half of this pipeline and never touches the disk or the frontmatter parser. What renders is
an ordinary labeled group — the operator's key as the heading, a `<description>` (the
mount's own, unless the operator wrote one), and one stub per document.

Each stub's `path` is a reserved, **scheme-shaped** prefix rather than a filesystem path:
`efference://guide/<slug>`. Scheme-shaped because the dispatch layer only ever compares the
front of a path against a string, and this leaves room for `read` to grow other schemes
without re-deciding what a leading `/` would have meant.

- **Interception is at tool dispatch, before the real `read`** (`src/agent/step.rs`): a
  `read` naming the prefix is answered with the embedded bytes as an ordinary `RawResult`.
  Because the served body is a normal tool result, `project` and byte-stability never learn
  it is special — the log and projection core is untouched, which is what makes the whole
  mechanism cheap.
- **The prefix is read-granted, never write-granted** (`src/policy/paths.rs`). Read clears
  with no config at all, which is the point: a session that knows nothing else still knows
  how to learn. Write is refused by both grant sources, and a guide path never resolves
  through `[paths]` at all — joined onto the cwd it would look like an ordinary relative
  path, and a broad grant would then appear to cover it.
- **Only `read` resolves on the prefix.** A `write`/`edit`/`list`/`search` naming it is
  answered before the permission cascade with the list of documents, because no rung's
  answer could make it run. The stub listing substitutes for a directory listing; `ls` and
  glob over the prefix are a stated non-feature rather than a half-implementation.
- **A batch mixing documents and files is refused whole**, naming the file. The two halves
  come from different places and answering half of one silently would be worse than one
  corrective turn.
- **Snapshot.** The stubs ride the `Payload::Preamble` snapshot like any group; the bodies
  are served later and stored on their `ToolResult`. Across a mid-session binary upgrade
  those can diverge, and the log honestly records what was served each time.

**One document in full: `builtin = "guide/<slug>"`.** The same value with a slug renders
that document's body inline, tagged with the operator's label the way an `exec` section is
(`guide::builtin_section`). The scaffold uses it to carry `new-project` whole, so a setup
session starts with the task in hand rather than behind a `read`. An unknown slug fails the
file.

**Listing is opt-in when a config exists, and the mount exists regardless.** An operator's
config renders the group only if it names the entry. The scaffold written for a session
with no project config *does* name it (`src/boot.rs`) — a visible, deletable
line in the session's own file, the same way orientation is — because a config-less session
whose only recovery path is "read how to write a config" needs that path listed. Deleting
the line stops the listing; the documents stay readable at their paths either way.

**Compiled-in means testable.** Filesystem skills drift independently of the tool; these are
versioned with the binary, so `src/guide/test.rs` asserts the references still hold: every
fenced `toml` block in a document parses and validates as a real config (the schema rejects
unknown fields, so a renamed key fails there), every tool name a document declares is one
`agent::tool_definitions` advertises and is actually mentioned in its prose, and every
`efference://` link resolves to a document that exists.

## The kickoff: `first-turn`

`first-turn = "…"` makes the session's first turn the agent's. The API needs a user message
before the model can speak, so the harness supplies one: when the runtime starts
(`src/daemon/runtime.rs`) with a `first-turn` configured and no `User` entry in the log, it
submits the text as a turn whose `source` is `efference` (`frontend::KICKOFF_SOURCE`). The
check is on the log, not on how the session began, so a resumed session that has had its
opening does not get a second one.

The kickoff belongs to the session's opening the way the preamble does, so neither client
draws it as a turn: the TUI projection and the web transcript tree skip a `User` entry with
that source, and the agent's reply is the first thing shown. It is an ordinary entry
otherwise — logged, projected with its `source:` line, visible to a log reader. Because a
turn with that source is not drawn, it is reserved: `POST /turns` refuses a client-asserted
`source: efference`, which would otherwise be a way to put a turn in front of the model that
the operator never sees.

The scaffold written for a session with no project config carries a `first-turn` line, so
such a session is the setup profile: the agent greets the operator and offers to set the
project up. The line is visible and deletable in the session's own config, like the
orientation `exec` entry.

## Boot discipline: freeze, snapshot, fail closed

- **Read once at boot, frozen.** Bodies and stubs are resolved from disk once and frozen
  into the cached prefix; a later edit to a listed file never re-renders the preamble
  (the same discipline as scope: never mutate behind a cache breakpoint).
- **Snapshotted into the log.** Because the preamble folds in file contents that drift on
  disk, it is no longer reconstructable from `config + current files` — so boot appends
  the full rendered system prompt as a `Payload::Preamble` entry once at boot
  ([log-and-projection.md](log-and-projection.md)). The log stays the complete record; `efference dev wire`
  reads the snapshot back instead of re-resolving; and once order is baked into the
  snapshot, sort order is a pure semantic choice, never a stability constraint. The entry
  also carries the **structured** `Resolved` listing (the `Section`/`Item` tree) beside the
  flat string — additive, so the byte-exact string stays the wire truth while the structure
  survives as a fold for the surfacing rider ([redaction-fork.md](redaction-fork.md)).
  The same boot entry carries the session's fixed connection/provider/native-model
  identity as unprojected metadata; offline readers derive wire placement from it
  rather than from the current config.
- **Fail closed, loudly.** A named file that can't be read, or an unterminated
  frontmatter block, fails startup — silently booting without configured context would be
  the model running with knowledge it was told it had.

`efference check` runs the same resolution against a template without minting a session, so a config can be tried before it boots anything (docs/operating.md).

## Deferred

Live mid-session discovery of context files (`backlog/live-context-update.md`) and glob
paths (`backlog/context-globs.md`).
