# Session id and title

A session has two names with two jobs. The **id** is fixed at creation and is how
anything reaches the session: its directory under `.efference/`, the `--name` a
client resolves, the `/sessions/<id>/` prefix on a host, and the address a peer
agent will message ([control-channel.md](control-channel.md)). The **title** is
one line saying what the session is for, written and rewritten as the work
moves, and shown wherever a person picks a session out of several. Keeping them
apart is what lets the id be short and stable while the title stays true.

## The id: a random word pair

When `POST /sessions` carries no `name`, the host mints `adjective-noun` from two
short lists (`daemon/slug.rs`): common words, unambiguous to spell, safe to say
aloud, so an id can be typed from memory or read over a shoulder. The pair must be
free both in the host's live map and on disk under the session base — a dead
session's directory still holds its name — and a taken pair is redrawn: a random
name carries nothing worth keeping through a collision. Only when a bounded run
of draws all collide does the last pair take a numeric suffix (`amber-heron-2`),
so minting terminates in a directory that has used up most of the space. An explicit `name` is accepted as before, subject
to the same single-path-segment rule.

There is no rename. A peer that was handed the id in a message and has not yet
sent to it would be reaching a name nobody could see was still in use; a mutable
title removes most of the pressure to rename. The id is the directory name, so the
fully-qualified form is still a path, but a name is now short enough that the
path is rarely what gets handed around.

## The title: a log entry, presentation only

The current title is the text of the last `Payload::Title` entry
([log-and-projection.md](log-and-projection.md)). It projects to nothing — it is
log metadata like timestamps and telemetry, so a title written mid-session cannot
move the cached prefix, and the model is never shown it. The `session_identity`
topic ([control-api.md](control-api.md)) carries `id` and `title` beside the model
and cwd, republished whenever the title moves, so the TUI header and session drawer
([tui.md](tui.md)), the web top bar and session list ([web-client.md](web-client.md)),
and `GET /sessions` all read one source. On boot the identity's title is the fold
over the log, so a resumed log would come back titled.

Each entry names its author: `triage` for the peek below, `operator` for
`PUT /title`. A model-written title starts from whatever the operator last set, so
an override is not fought. An entry is appended even when the peek kept the
previous text, so the log records that the question was asked; the predicate
below depends on that.

## The title peek

The title is asked of the triage model, not the agent: the agent is not given a
tool for it (every tool on the main session is noise for the work it is doing),
and the ask never enters its transcript. The peek is the fork's shape
([triage.md](triage.md)) with a different tail: the log projected
through whatever has landed, a cache breakpoint, and a user-role ask for one
line of at most six words saying what the session is for as a whole, with the
standing title quoted and an instruction to keep it verbatim unless the purpose
changed or the title outgrew the budget. The answer is the completion's **text**, not a tool
call. A tool would have to ride the shared `tools[]`, which the agent turn also
sends, so the agent would see a schema that is never for it and the envelope pin
would move for a job unrelated to results; a text answer leaves the request shape
untouched, and `tool_choice` stays `auto` on both lineages so the shared peek
still reads the agent's cache. On the own-model lineage the ask is the same; that
lineage forces its tool choice on result peeks, so a title ask is a differently
keyed messages read there, which the rarity of the ask makes affordable.

The first non-empty line comes back with wrapping quotes, backticks, a trailing
period, or a `Title:` label removed. A tool call, no text, a line over
`TITLE_MAX_WORDS`, or a line over `TITLE_MAX_CHARS` is unusable and retried
in-fork on the fork's attempt budget with a renewal sentence; exhaustion
records nothing, so the next title check asks again. Requests are recorded under the
`title` usage origin, so the cache report shows them on their own row.

The ask states the length in words because a model cannot count characters —
it generates in tokens, and words are the unit it can actually aim at — while
the parse still holds a character ceiling for the runaway case. Six words is
the `<title>` register the surfaces want: the sidebar, the top bar, and the
session list all truncate longer lines.

## When it runs

`agent/title.rs` holds one predicate, `should_solicit_title`. The **title check**
tests it at each turn start, each seam-drain (a user turn delivered between a
completion's results and the next request), and each trajectory end, however it
ends — the model answered, gave up, or the turn was aborted. Those are junctures
where a main request's cache is already warm. It says yes:

- when no title has been written;
- when a `User` entry landed after the last title — purpose moves with the
  user's turns, so each is answered once, at the moment it lands;
- when `TITLE_FALLBACK_TRAJECTORIES` assistant turns have ended since the last
  title without one, for an autonomous run whose purpose drifts with nobody
  typing.

Each ask runs as a detached task on the loop's runtime: the loop never waits
for it, the peek reads the prefix the last request cached while the loop moves
on, and the `Title` entry lands mid-trajectory when the answer comes back. A
task that wakes with the cache expired holds the way the idle triage
holds — waiting for the session's own next read rather than reading an expired
prefix before a person has acted.

One ask is in flight per session. A title check that passes while one runs — a
one-turn session reaches its trajectory's end before the ask from the turn's
start has written anything, so the predicate still says yes — marks a rerun
instead of spawning a second peek, and the task re-checks the predicate when its
ask ends. A `User` entry that landed after the ask read the log and before its
`Title` was appended sits before that title, where the predicate counts it as
answered though the peek never saw it; the rerun then skips the predicate and
asks again.

The predicate is the one place to tune cadence; the expectation is that titles
change rarely and that a re-ask mostly confirms.
