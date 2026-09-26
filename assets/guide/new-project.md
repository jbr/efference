# Setting up a project for efference

Where a session's knowledge and perimeter are written down, what to put there, and how to
find out what the operator wants there.

## Two files, two jobs

A session's policy and boot knowledge come from one TOML file, resolved when the session
is created, in this order: `--config <path>` (which must exist), else `efference.toml` in the
working directory, else a scaffolded file with only an orientation entry in it. Nothing
else is scanned — there is no `EFFERENCE.md` convention, no auto-loaded `CLAUDE.md`, no
search for skill files.

At boot the resolved file is **copied once** into the session's own directory,
`.efference/<session-id>/config.toml`. That snapshot is the live policy for the rest of the
session; the original is an inert template, and two sessions started from one template
diverge freely. So:

- **`efference.toml` at the project root** — the template. Editing it changes what the
  *next* session starts with. This is the file a "set this project up" task writes.
- **`.efference/<id>/config.toml`** — this session's live perimeter. Editing it is how a
  running session *requests* a change: that write is always held for the operator, whatever
  else the config allows, and it takes effect the moment it is approved.

A malformed config is rejected whole rather than partially applied, and a rejected edit is
reverted, so if a change does not seem to take, check that the file still parses.

## How to do this task

Half of a template is fact and half is preference, and they are found differently. The
task runs in this order, across two of your turns:

1. **Survey the workspace yourself.** What the project is and how it builds and tests;
   which files carry secrets (check variable *names*, never print values); what is
   gitignored; which files a session would want to know before its first command; whether
   a `CLAUDE.md` is present, and if so whether Claude Code's memory directory for this
   workspace exists (below). Do not ask about any of this — you are inside the workspace.
2. **Ask, then end your turn.** Say in a few lines what the survey found, then put the
   operator's decisions (the list below) as questions — each one short, with the default
   you would pick if they say "your call" — and stop there. Do not paste a draft of the
   file: the file is what their answers produce, and a draft under the questions reads as
   already decided. There is no ask tool; a turn that ends on the questions is how they
   are asked, and the operator's next message answers them.
3. **On your next turn, write the files** from the answers — `AGENTS.md` if the project
   has none, then `efference.toml` — show them, and run `efference check`.

If the operator said up front to proceed without questions, skip step 2's stop: write your
defaults and list the decisions in your report.

**What is the operator's to decide** (propose a default, don't settle it):

- How broad the command grants are — `git = true` with `"git push" = "deny"`, or named
  sub-verbs; whether sessions may commit; anything that touches a network or a registry.
- Whether any built-in tool should be *withheld*. The default is all of them (below); a
  narrower list is a choice the operator makes, not one to make for them.
- Which MCP servers, if any, sessions should have connected (`[mcp.servers]`) — a
  documentation server, an issue tracker, a browser. Ask; nothing in the workspace says.
- What rides in the preamble beyond `AGENTS.md` (below), if anything — including whether
  to share the Claude Code memories, when the survey found them.
- The `scope` wording — what sessions in this project are *for* is theirs to say; you can
  draft it from the README but not confirm it.
- `watch-for`, if they want one at all (below).

**What is yours to do** without asking: a deny carve-out for any secret-bearing file;
grants for the project's own build tool; drafting `AGENTS.md` from the survey (below);
keeping `.efference/` out of version control; comments in the file saying what each entry
is for; running the check at the end.

## A starter template

The shape, shown for a TypeScript project; the build tool, the orientation file, and the
`scope` are the parts that change with the project.

```toml
scope = "Work inside this repository: read, build, test, and edit its sources."

tools = ["efference.*"]

[commands]
pnpm = true              # the project's own build/test tool, whatever it is
node = true
"git status" = true
"git diff" = true
"git log" = true
"git push" = "deny"
"efference check" = true # so a session can validate this file without a gate

[paths]
"." = true

[context]
"AGENTS.md" = true       # written for sessions; see below
"Environment at boot" = { exec = 'echo "working directory: $PWD"; echo "platform: $(uname -sm)"; echo "node: $(node --version 2>/dev/null || echo missing)"' }
```

Every key is described in `efference://guide/config-format`.

## The built-in tools

`"efference.*"` grants every tool the harness ships, now and as built-ins are added, and is
the right default: a path-bearing built-in still clears only inside a `[paths]` grant, so
the blast radius is the `[paths]` table's, not the glob's. Name tools individually only
when the operator wants one withheld. The set today:

- **Files** — `read`, `list`, `search`, `write`; `edit`, `find_anchor`, `retarget_edit`,
  `undo_edit` (the anchor-based edit pipeline); `read_image`, which drops out on its own on
  a model without image input.
- **The session's own housekeeping** — `consolidate` (curating the transcript),
  `check_inbox`, and the process supervisor `list_processes`, `read_process`,
  `watch_process`, `kill_process`. These act on the session, not the workspace, and there
  is rarely a reason to hold them behind a gate.
- `bash` is not in this list on purpose: a shell command is judged as command text by
  `[commands]` and the spotter, and no `tools` entry clears or withholds it.

A connected server's tools take the same shapes — `"ferritin.*"` for the whole server,
`"ferritin.get"` for one — but a server glob is a statement of trust in that server's whole
table, present and future, so it is the operator's call rather than a default.

## What belongs in `[context]`

`[context]` is the only route by which a session boots knowing anything about the project.
Nothing is picked up by name — an `AGENTS.md` or `CLAUDE.md` already in the repo rides only
if listed here.

**List `AGENTS.md`, and write it if the project has none.** A README is written for a
person deciding whether to install the thing; a session needs what a new contributor is
told on day one, in the order a session meets it: what the project is in two sentences,
how to build and test, the layout of the source and what each part owns, the conventions
the code observes (formatting, where tests live, how errors are handled), and where the
records are (changelog, docs). Draft it from the survey — a page at most, present tense,
no praise — and show it with the config. Do not list the README alongside it: what the
README says that a session needs belongs in `AGENTS.md`, and the rest is install and
license text re-sent on every request.

**Claude Code memories.** A `CLAUDE.md` in the repo means Claude Code has worked here, and
it may have kept per-project memories at
`~/.claude/projects/<workspace path with every "/" replaced by "-">/memory/` — for a
workspace at `/home/<user>/code/<project>`, that is
`~/.claude/projects/-home-<user>-code-<project>/memory/`. Each memory is one Markdown file
with a frontmatter `description`, so listing the directory rides as one stub per memory,
read on demand. The index file `MEMORY.md` is Claude Code's own and is excluded. Whether
efference sessions should see them is the operator's call — they were written for another
harness and may say things that are not true here — so ask, and if yes the entry is:

```toml
[context]
Memories = { path = "/home/<user>/.claude/projects/-home-<user>-code-<project>/memory", exclude = ["MEMORY.md"] }
```

Read-only as written; `create = true` would let sessions add memories of their own there,
which is a further question.

List the files every session should have:

- A bare path (`"CLAUDE.md" = true`) rides in the preamble **in full** — unless the file's
  own YAML frontmatter carries a `description`, in which case only a one-line stub rides
  and the body is read on demand through `read`. Any readable file may be listed, not
  only Markdown.
- A directory expands to every `.md` file beneath it, recursively, one listing per file.
- Every listed path is read-granted automatically, so a stub's path resolves through `read`
  with no `[paths]` entry at all.

That last point cuts both ways: listing a directory read-grants everything under it. List
deliberately.

Every byte of the preamble is re-sent on every request, so size it like a cost: `AGENTS.md`
and an environment line are a few kilobytes and fine; a changelog or a large reference is
worth a frontmatter `description` so only a stub rides. `efference check` prints the byte
count.

## What to leave out

- **Secrets.** Name a vault reference in `[env]` rather than a literal value; a resolved
  reference is masked out of the log automatically.
- **`watch-for`.** It is read only by the permission spotter and is never shown to the
  session, which is the point of it. It is the operator's to set: if the survey turns up a
  hazard worth naming, put it in the proposal, not the file.
- **Anything the session should discover for itself.** The preamble is frozen at boot and
  every byte of it is re-sent on every request.

## Facts that settle the usual doubts

- `[paths]` governs the structured file tools only. A `bash` command is judged by
  `[commands]` and the spotter, never by `[paths]`, so a path deny is a guardrail against a
  mistaken tool call, not a wall — a secret-bearing file still needs the `scope` and the
  spotter to stay unread through the shell.
- `.efference/` is denied by the harness underneath whatever the template grants, with this
  session's own `config.toml` re-allowed read-only. A template does not need its own
  `.efference` entry, and one does not interfere with the re-allow.
- A `[context]` entry's `update`/`create` and a `[paths]` write grant are two routes to the
  same clearance; a file under `"." = true` is writable whether or not its context entry
  says so.
- Unknown keys, an un-lexable `[commands]` key, and a `[context]` file that does not exist
  are all boot failures, which is what the check below is for.

## Checking the result

`efference check` (or `efference check <path>`) validates a template without minting a session: the parse and
the same soundness checks a session runs at boot, then every `[context]` entry resolved from
the current directory — `exec` entries do run — then every `[mcp.servers.*]` stanza mounted,
so each server's advertised tools print against the file's `tools` grants. It prints what the file grants and the
preamble's size, or the boot-time error. Stored MCP credentials are reused; a server needing
authorization is reported, never authorized here. **The task is complete when the check passes and
the operator has seen the decisions.** A session started from the file then holds a
verbatim copy at `.efference/<id>/config.toml`, and `efference control state` prints the
**effective** policy in force, which is that file plus every change approved since boot.
