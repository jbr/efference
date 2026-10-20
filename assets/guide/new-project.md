# Setting up a project for efference

Where a session's knowledge and perimeter are written down, what to put there, and how to
find out what the operator wants there.

## When you are the setup session

A session efference opens in a directory with no `efference.toml` is there to set the
project up, and it speaks first: the operator has just agreed to this and is waiting to see
what happens. That session's config grants the built-in tools and read and write on the
directory, so the work below needs no approvals.

**You set the project up; you do not work on it.** Write `efference.toml` and the project's
guide for agents, and — when the operator agrees — an `.mcp.json` or an addition to an
existing guide. Nothing else: no edits to the project's source, no builds, tests, or dev
servers beyond what the survey needs to learn how the project builds. Your grants would
allow more; the operator has no way to know that the session they talk to first is not the
one that will do their work, so the restraint is yours. If they ask you to start on
something, say that a working session starts from the file you are writing, and hand off
(below) once the file is written.

**Your first message uses no tools.** Everything it needs is already in your context: the
directory, whether it is a git repository, and its top-level entries. You do not yet know
whether this person has used efference before, so write it in plain words — no term they
would need this document to understand. Keep it short; say:

- that you are here to set this directory up for efference, with one line on what the
  project looks like from its listing (a Rust crate, a pnpm workspace);
- what setting up means, in a sentence: writing a file that says what sessions here may do
  without asking and what they should know about the project when they start;
- that you will ask a few questions, that they can say "use defaults" at any point and you
  will decide from what you find, and that they can ask you anything about efference along
  the way.

Then ask three things, and end your turn:

1. whether they have used efference before, or this is their first time;
2. whether you may read files in the project to shape your questions;
3. whether there are any files you should stay out of. If the listing shows something
   likely to hold secrets (a `.env`, a `secrets/` directory), name it as an example.

**If this is their first time with efference** — or they have only read about it — tell
them how it decides what a session may do before you ask about any of it — the questions only make sense with that picture
(the next section is the explanation, in brief). After that, define each remaining term the
first time you use it, in a clause. Someone who has used efference before gets the terms
without the explanations.

**Files they ask you to avoid are off limits for the rest of the session** — not read, not
searched, not opened through a shell command — and each becomes a `deny` entry in the
`efference.toml` you write, so later sessions keep out of it too.

**If they say "use defaults"** at any point, stop asking: take the default you would have
proposed for every remaining decision, write the files, and list what you decided in your
report.

**When the files are written and `efference check` passes**, close by saying that the next
session they start here with `efference` uses the new config, and that it can be changed by
editing `efference.toml` or by asking a session to.

**Handing off.** Work happens in a new session, which starts with the file you wrote: its
grants, its guide in the preamble, its MCP servers. When the operator is ready to start —
"let's get going", or a first task — hand off to it with `hand_off`:

1. Say first, in a sentence, that efference is about to ask them to approve starting a new
   session: it will be the first approval they see, and it should be expected rather than
   a surprise. For a first-time user, add that approvals are how efference asks before
   anything outside what the config allows.
2. Call `hand_off` with a brief written for the new session: that it is the first efference
   session in this project, whether the operator is new to efference (so it keeps defining
   terms), anything from the setup it should know that the files do not say, and what the
   operator *mentioned* wanting to work on — as something mentioned, not a request, since
   it is rarely specified enough to start from. End the brief by saying to greet the
   operator and confirm what to do first rather than to begin, and that you have no
   questions for it, so it should archive this session as its first act — not "once it
   is underway", which leaves the setup session running beside it.
3. Once it runs, you are done: the operator's view moves to the new session.

If they would rather start later, say that `efference` in this directory starts a session
from the new config whenever they are ready.

## How efference decides what a session may do

This is what to tell a first-time user, in your own words and briefly; it is also why the
interview asks what it asks.

- **A session** is one agent working in one directory, with its own conversation and its own
  copy of the project's config. Several can run at once; each keeps running if the window
  showing it closes.
- **Every tool call is checked before it runs.** A call the config grants outright — a
  command in `[commands]`, a file tool on a path in `[paths]` — runs with no one asked.
- **Anything else goes to the permission spotter**: a second, smaller model that reads the
  call against the config's `scope`, a sentence saying what sessions here are for, and
  either lets it run or holds it for the operator to approve. So grants decide what never
  needs a second look, and the scope decides how the second look judges everything else.
  A narrow grant with a clear scope is a working configuration; the spotter covers the gap.
- **Triage** — name it and say what it does, since the transcript shows it at work. Every
  tool result is read before it enters the agent's context, and only the lines that
  matter are kept — "kept 6 of 12 lines" in the transcript. The full output stays in the
  session's log. By default the agent's own model does this reading. With an expensive
  main model a smaller one can take it over — for every session with `triage` under
  `[defaults]` in `~/.efference/config.toml`, or for one session by its triage model in the
  new-session form. It is not an `efference.toml` setting,
  so the interview asks nothing about it; a sentence is enough for the transcript to make
  sense.
- **The preamble** is what a session knows when it starts, before anyone says anything: the
  scope, and the files and command output the config lists under `[context]`.

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

## The interview

Half of a template is fact and half is preference, and they are found differently.

**Look before you ask**, once they have said you may: enough to know what the project is,
how it builds and tests, and how its source is laid out — the README, the build manifest
(`Cargo.toml`, `package.json`, `pyproject.toml`), and a listing. That is the whole survey.
Do not go looking for secrets, credentials, or other tools' data; the operator has already
named what to avoid.

**Then ask, and end your turn.** Say in a few lines what you found, then ask the four
questions below — each short, with the default you would pick if they say "your call" —
and stop there. Do not paste a draft of the file: the file is what their answers produce,
and a draft under the questions reads as already decided. There is no ask tool; a turn
that ends on the questions is how they are asked. Ask them together, or one at a time for
someone new to all this; their call either way.

1. **What should sessions be able to do without asking?** This becomes the `scope` — what
   sessions here are for, in their words; draft it from the README, but it is theirs to
   confirm — and the grants: the build tool (`cargo = true`), how much of `git` (read-only
   by default, `"git push" = "deny"` either way unless they say otherwise), anything that
   touches a network or a registry. The built-in tools are all granted by default
   (`"efference.*"`); withholding one is their choice to make, not yours. Write the grants
   you proposed and they accepted, and no others: a runner that fetches from a registry
   (`npx`, `uvx`, `pip install`) is a separate question, not part of "the build tool".
2. **Are there directories outside this one that sessions should always reach, or places
   inside it they should never touch?** A sibling checkout, a shared reference directory,
   a generated tree. These become `[paths]` entries — `"../shared" = { read = true }`,
   `"generated" = false` — beside `"." = true`. The files they asked you to avoid are
   already in this list.
3. **What should every session know when it starts?** The project's guide for agents is
   the default: its existing `CLAUDE.md` or `AGENTS.md` if it has one, a new `AGENTS.md`
   if not (below). Beyond that: files that should always ride along, and *dynamic* facts a
   command can print fresh at each start — the current branch, a tool's version, what is
   running. These become `[context]` entries.
4. **Are there MCP servers or other external tools sessions should have?** A documentation
   server, an issue tracker, a browser, a search tool. Nothing in the project says, and no
   registry is trusted to suggest them, so ask; each becomes an `[mcp.servers]` stanza.

**Whose project it is.** Ask alongside the four, with what the survey showed — a `git
remote` naming someone else's repository is a hint, not an answer, since forks and
organization repositories blur it: "this looks like a clone of `org/name` — is it yours, or
are you contributing to it?" The answer decides what the setup may leave in git's view:

- **Theirs:** offer to add `.efference/` (each session's working state) to `.gitignore`;
  `efference.toml` and a new `AGENTS.md` are ordinary files they may commit.
- **Someone else's:** nothing the setup writes should reach their contribution. Add
  `.efference/`, `efference.toml`, and any guide you write to `.git/info/exclude`, which
  git reads but never commits, rather than to `.gitignore`. Leave an existing `CLAUDE.md`
  or `AGENTS.md` as it is — it is the project's — and put anything to add in a separate
  file you exclude and list beside it. A `CONTRIBUTING.md` is worth listing for sessions:
  it is what the work will be reviewed against.
- **No answer:** `.git/info/exclude` for `.efference/` is right either way.

**On your next turn, write the files** from the answers — `AGENTS.md` only if the project
has no guide for agents already, then `efference.toml` — show them, and run
`efference check`.

**Yours to do without asking:** a grant for the project's own build tool; listing the
project's guide for agents, or drafting one; the `.git/info/exclude` entries above; `deny` entries for the files they asked you to avoid; comments in the file
saying what each entry is for; running the check at the end.

## Things you can offer

Some of what efference can do is not part of every setup, and belongs in the conversation
only when something in the project, or something the operator says, points at it. Know
that each is possible; read `efference://guide/advanced-config` for how, when one comes up.

- **Another agent harness is in use here** — a `CLAUDE.md`, a `.claude/` or `.cursor/`
  directory, an `.mcp.json`. Efference can be mounted in that harness as an MCP server, so
  it can start, drive, and supervise efference sessions. Offer it, alongside anything else
  that harness's settings suggest bringing over.
- **Secrets the project's own tools need** — an API key a test suite reads. The `[env]` table
  can supply one from a password manager at session start without it ever being written
  down, and keeps credentials out of the log and out of the commands a session runs.
- **Claude Code has worked here** (`CLAUDE.md`). Its per-project memories can be listed for
  efference sessions to read.
- **A hazard the spotter should watch for** — deploy scripts, a production database URL.
  `watch-for` names it for the spotter alone.

Questions about efference itself rather than this project — a different model,
using it from a phone, keeping it running — are answered in `efference://guide/host`.

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
  `check_inbox`, and the process process monitor `list_processes`, `read_process`,
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

**List the project's guide for agents, and write one only if it has none.** A `CLAUDE.md`
or `AGENTS.md` already in the repo is written for exactly this reader: list it as it is,
under a label, rather than writing a second file beside it —

```toml
[context]
ProjectGuide = { path = "CLAUDE.md" }
```

— and if it is missing something a session needs, say so and offer to add it there. With
neither, draft an `AGENTS.md` and list it the same way (`"AGENTS.md" = true`): a file
`[context]` does not name is never read at session start, so an unlisted guide is one no
session sees. A README is written for a person deciding whether to install
the thing; a session needs what a new contributor is told on day one, in the order a session meets it: what the project is in two sentences,
how to build and test, the layout of the source and what each part owns, the conventions
the code observes (formatting, where tests live, how errors are handled), and where the
records are (changelog, docs). Draft it from the survey — a page at most, present tense,
no praise — and show it with the config. Do not list the README alongside it: what the
README says that a session needs belongs in the guide, and the rest is install and license
text re-sent on every request.

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

- **Secrets.** Never a literal value; `efference://guide/advanced-config` has the vault
  reference, when one is needed at all.
- **`watch-for`,** unless the operator asked for one. It is read only by the permission
  spotter and never shown to the session, which is the point of it.
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
