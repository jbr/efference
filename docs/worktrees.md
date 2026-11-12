# Worktree sessions

A session can work in its own git worktree: a second checkout of the project, on its own
branch, so its edits do not land in the checkout another session or the operator is
using. `POST /sessions {"worktree": true}` asks for one (`efference` CLI `--worktree`,
the `create` control tool's `worktree`). Worktree placement lives in
`src/record/session_dir/worktree.rs` and the git operations in `src/host/worktree.rs`; the directory
pair lives in `src/config/policy/directories.rs`.

## Project and working directory

Every session has two directories (`policy::Directories`, carried by `SessionPaths`,
`Session`, and the session header):

- the **project**: where `efference.toml` is read, where the session dir
  `.efference/<id>/` lives, what `[paths]` and `[context]` keys resolve against, and what
  the launch index and model history record;
- the **working directory**: where the session's tools run (bash, the built-in file
  tools, MCP mounts, background processes), what the paths a call names resolve against,
  and what a child session it creates inherits.

They are the same directory for a session without a worktree. A session is never run
*in* a worktree: there is one `.efference/` and one `efference.toml`, the project's, and
a worktree has neither of its own. The header records the working directory (`working`)
only when it differs, so a header without a worktree is unchanged.

The host derives the pair from the one `cwd` a creation request carries
(`session_dir::worktree::project_of`): a `cwd` inside `<project>/.efference/worktrees/<id>` belongs to
that project and works where it was sent, so a session created from inside a worktree —
by a person in a terminal there, or by a session working there — inherits the worktree.
A child that wants its own asks for `worktree: true`. The user-wide `env_hook`
([env.md](env.md)) runs at the project, on creation as on resume, so a session's
environment does not depend on which of the two started it.

## Creation

With `worktree: true`, boot runs `git worktree add -b efference/<id>
<project>/.efference/worktrees/<id> <HEAD>`, from the creator's working directory and on
the commit its `HEAD` names, with the session's child environment. The worktrees sit
together under one directory rather than inside each session dir, so one path prefix
covers all of them. A project that is not a git work tree, or whose `HEAD` does not
resolve, is refused before anything is minted; a boot that fails after the add removes
the worktree and its branch. The creation preview reports the same two preconditions
as a `worktree` boolean (`worktree::can_start`, sharing the check with `add`), and the
new-session forms show their worktree option only where it is `true`
(docs/control-api.md).

A creation never adopts a directory it did not mint: a path that exists, a worktree
git already registers there, or a branch `efference/<id>` left behind by an earlier
checkout refuses the add before git runs (`worktree::add`), and the name allocation
routes around such leftovers ([session-title.md](session-title.md)). What counts as
success is the checkout's coherence — registered at exactly the path, `HEAD` on the
requested branch, git dir usable — not the directory's presence, so a hook failure is
a recorded outcome below and an incoherent result is a refusal that touches nothing.

The checkout is a plain one, so the project's `post-checkout` hook runs. Warming a new
worktree — cloning build output, initializing submodules — is the project's concern,
expressed in that hook, not the harness's: what a warm tree needs is language- and
project-specific. (This repository's `.githooks/post-checkout` clones `target/` and
`web/node_modules` copy-on-write and initializes the theme submodules from the main
checkout.) Git returns the hook's exit status as its own, so a nonzero exit with the
directory present is a created worktree with a failed setup, not a failed creation.

Every git the harness runs drops git's repository-local variables (`GIT_DIR`,
`GIT_INDEX_FILE`, and the rest of `git rev-parse --local-env-vars`;
`worktree::unset_repository_env`). Git exports them to the hooks it runs, so an
environment captured inside one names that hook's repository; with them present, a
`git worktree add` acts on the wrong repository and a `git init` reinitializes it as
bare.

## The identity line

When the working directory differs from the project, the identity line
([context.md](context.md)) names both, and adds the branch when this session made the
worktree. Both are fixed at creation, so the sentence is byte-stable; a session without
a worktree renders exactly what it did before. A session never moves into or out of a
worktree mid-session for the same reason: the working directory is in the frozen
prefix.

## Permissions

`[paths]` keys resolve against the project and the paths a call names against the
working directory. A target inside any worktree, `.efference/worktrees/<id>/<rest>`, is
classified as `<project>/<rest>` (`session_dir::worktree::counterpart`, applied by
`Directories::target`): every worktree, the session's own and every other session's,
has exactly the project's grants, narrower ones included. A grant of `.` therefore
covers the project checkout and every worktree, for reading and writing — a worktree is
a convenience, not an isolation boundary, and path grants are mistake-guardrails
([permissions.md](permissions.md)). Only classification uses the counterpart; the tool
touches the path the call named. Both spotters are shown the same view: the project as
the working directory and each resolved path as its counterpart
(`Directories::judged_working`), because the worktree's real path sits under the
denied `.efference/` and a reviewer comparing strings read every in-tree move as a
denied path. `.efference/` itself is never mapped: it exists only at
the project, where the injected self-surface stack anchors.

A grant key that leaves the project (`../sibling`) resolves from the project, as its
author wrote it. A path the *model* types is resolved from its working directory and is
not rewritten: `../sibling` from a worktree names a directory inside
`.efference/worktrees/`, and the call fails visibly. The identity line's absolute paths
are what the model uses instead. Manifest paths that leave the repository (a Cargo path
dependency on `../sibling`) break the same way inside a worktree; absolute paths, or the
project's `post-checkout` hook, bridge them.

## Occupancy, archive, and resume

A managed worktree is shared: every session whose working directory is at or beneath
its root works in it, whether the session made it or inherited it at creation, at a
hand-off, or by resuming with a working directory inside it. The tree is occupied while
any such session exists — starting, hosted, or dormant with a log that does not end in
an archive (`occupancy::remains`, with `inventory::sitting` reading the dormant
directories). A session being archived counts as leaving from the moment its end runs
until its entry has left the map (`end.rs`, `Departure`), so when two occupants are
archived at once, each recount treats the other as gone.

An archived session is resumable, so archive does not treat the session as finished.
Archiving any occupant considers the tree for retirement — archive only, including an
island's exit, which archives each of its sessions ([resumption.md](resumption.md)); a
host shutdown leaves worktrees in place — and a start that leaves the map without
adopting is a leaving too (`preparation.rs`). The tree goes at the last occupant
leaving (`worktree::retire`):

- a worktree with no uncommitted or untracked changes (`git status --porcelain` empty;
  ignored files such as `target/` do not count) is removed; its branch is deleted only
  when another branch already contains its tip, and kept otherwise, since it then holds
  the only copy of those commits. A clean worktree on a detached `HEAD` no branch
  contains is kept;
- a worktree with changes is kept untouched.

Before any removal, the checkout must be the tree's own: registered at the root by
`project`'s git, its git dir usable, and its `HEAD` on `efference/<id>` (or detached at
a commit that branch contains, the case above). A path another repository registered,
or a checkout someone moved to another branch, is kept untouched with its branch
(`src/host/worktree/checkout.rs`) — leftovers are routed around, never deleted. The same
gate guards the rollback of a failed creation (`worktree::remove`).

The archive response's `worktree` field and a `Worktree` log entry record which happened
([control-api.md](control-api.md)); an occupant whose archive leaves the tree to the
sessions still working in it answers without the field and records nothing. When the
session never had a turn, a worktree that is removed goes with a discarded session, and
one that is kept makes it an ordinary archive ([resumption.md](resumption.md)).

Resume re-adds a missing tree at its original path — from its kept branch, or a fresh
`efference/<id>` at the project's `HEAD` — for any session working at or beneath it
(`worktree::restore`). The tree's own identity names the path and branch, derived from
the working directory the header recorded (`session_dir::worktree::root_of`), not the
resuming session's id, so a successor re-creates its predecessor's tree and the
identity line is byte-identical. A nested working directory keeps its place inside the
restored tree; one that was never tracked is still gone, and creating it is the user's
to do — a restore never makes directories inside the tree. A working directory inside
no managed worktree that is gone refuses the resume, like any missing working
directory.

### Synchronization

Every check of who occupies a root, and every git operation that creates, restores, or
removes one, runs under that root's lock (`src/host/locks.rs`), taken before the
sessions map lock and never the reverse. The recount and the removal are one critical
section (`occupancy::retire_unoccupied`); the map lock covers only the recount. A
session admitted after the recount is on the map, so no later removal can begin, and
before it uses the tree it takes the root's lock itself, which orders it after a
removal already under way: a creation then refuses the directory that is gone
(`Host::wait_out_removal`), and a resume re-creates the tree (`boot::resume`). Two
restores of one removed tree serialize the same way — the second finds the first's
checkout in place and records nothing.

Every section takes its guard on a blocking thread, inside the closure that does the
work, so a cancelled await cannot release it mid-removal and no async thread waits
while a hook or a removal runs. A boot cancelled before it has taken over its new
checkout rolls it back, whether git was still running or had just finished
(`boot::prepare`, `boot::mint`). A lock is per root, so a slow `post-checkout` hook
delays only the operations on that one tree.

Discarding a kept worktree is git's: `git worktree remove --force` and `git branch -D`.

## The log entry

`Payload::Worktree` (`src/record/log/worktree.rs`) records `added`, `restored` (git's base,
exit status, and output, which includes the project hook's), `removed` (whether the branch
went too), `kept` (why), and `remove_failed`. It projects to nothing, like `Cold`, and its
free text is masked like any other ([log-and-projection.md](log-and-projection.md)). It
is the audit trail for a slow, cold, or failed setup.

Both clients draw the entry as a one-line notice in their transcripts — the branch on an
add or restore with a failed setup's exit status on the line itself, and the outcome on
remove, keep, or failure ([tui.md](tui.md), [web-client.md](web-client.md)). The setup
`output` is not shown in the TUI; the web collapses it behind a disclosure.

## Git only

Placement, the permission rule, the identity line, the header, and the log entry are not
git-specific. Creating, removing, and the clean-and-merged check are, and they are the
whole of what `src/host/worktree.rs` runs. A non-git version control system (jj's
`jj workspace add`, Mercurial's `hg share`) would replace those three behind the same
functions.
