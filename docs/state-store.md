# The user state database

One sqlite file, `~/.efference/state.db`, holds the host's persistent state: which models
sessions have picked, each connection's model listing, the browsers subscribed to push, the
devices enrolled at the web bind, and the sign-ins pending there. A push subscription
carries the material that delivers to that browser, so the file and its directory are
owner-only — see **Modes** below. The machine root, API keys, and OAuth grants stay in the
OS credential store ([secrets.md](secrets.md)). The configuration is still a text file
(`config.toml`, [config-format.md](../assets/guide/config-format.md)); whether it should move here is an
open question ([backlog/inbox/user-state-store.md](../backlog/inbox/user-state-store.md)).

Module: `src/platform/state.rs` owns the path and the open; each table's own module owns its
schema.

## The tables

| Table | One row per | Held by | What reads it |
|---|---|---|---|
| `model_picks` | an explicit model pick | `src/config/history.rs` | the recent menu and the omitted-branch default ([transport.md](transport.md)) |
| `triage_picks` | an explicit separate triage pick, with the main model it ran with | `src/config/history.rs` | nothing yet; recorded for a later pairing default |
| `model_catalog` | a connection | `src/inference/catalog.rs` | the model menus ([model-catalog.md](model-catalog.md)) |
| `remembered_projects` | a directory a session was started in | `src/remembered.rs` | the launch index, `GET /projects` ([control-api.md](control-api.md)) |
| `session_ids` | a session id and the root holding it | `src/session_ids.rs` | id minting, explicit names and resume ([session-title.md](session-title.md)) |
| `push_subscriptions` | a browser subscription | `src/platform/push.rs` | the sender, per push ([web-push.md](web-push.md)) |
| `devices` | an enrolled device — one TCP credential | `src/platform/devices.rs` | the gate, per request; `efference devices` ([web-bind.md](web-bind.md)) |
| `pending_sign_ins` | a browser waiting to collect a device token: a `/pair` request, or a sign-in link | `src/platform/sign_ins.rs` | the claim and approval routes; `efference devices` ([web-bind.md](web-bind.md)) |

The subscriptions, the devices, and the pending sign-ins are user-scoped for a security
reason rather than a convenience one. The VAPID key is derived from the machine root, so
every daemon can send to every subscription, and a per-project store would hide a
subscription from the session that ought to reach it. The device-token key is derived
from the same root, so a device's token works at every daemon under the user, and a
per-project table would leave a device revoked in one place signed in at another. A
pending sign-in is approved over a socket, and a per-project table would leave a sign-in
that one daemon's listener took unapprovable from the socket the operator reached. Each
would be wrong per-project in the same direction.

The `devices` table holds no secret: an id is public — it is the readable half of the
token — and a token for it also needs the key derived from the root, which is in the
credential store. A row is id, kind (`browser` or `script`), label, how the device
joined, when it was created, and when it was last seen (written at most hourly per
device).

The `pending_sign_ins` table holds no secret either: a row is keyed by its claim
secret's SHA-256, so reading the table collects no token, and its short code authorizes
nothing, since only a socket approves. A row is the claim's digest, the code (unique), the
label, how the device will join (`code` or `link`), whether it is approved, and when it was
made and expires (90 s later). Every write prunes the expired rows, and a request evicts
the oldest waiting one past the cap of 8 ([web-bind.md](web-bind.md)). Opening the table
drops `totp_burns`, the accepted TOTP timesteps earlier builds kept, which nothing reads.

## Opening

Every operation opens its own connection, passing the calling module's
`CREATE TABLE IF NOT EXISTS` statements to `state::open`. A pooled or shared connection
would buy nothing — each store is touched once per session creation, once per menu
preview, once per push — and `Connection` is `Send` but not `Sync`. Applying the schema
on every open is the migration story: a new table lands in its own module beside the
existing ones, and a new column lands as an `ALTER TABLE` in a follow-up change.
Nothing is read back at startup, so there is no schema version to number.

An open that fails is the *table's* problem rather than the store's, and the caller
decides what that means. Most fold the error away: an empty catalog still serves from
memory, absent picks still fall back to the config's choice, an unreadable subscription
table means nobody is subscribed, a push that does not go out cannot fail a turn, and a
remembered location that cannot be stored is a picker entry that does not appear —
which, like the pick beside it, never fails the creation writing it. The web bind's two
tables are the exception, and deliberately. The device table reports the error: the
gate refuses a request it cannot check against the table — a `500`, so the client keeps
its token rather than discarding it as bad — a claim whose device cannot be recorded
mints nothing, and a listing that cannot be read is an error rather than an empty list,
which would hide a row whose token still works. The pending sign-ins report it too: a
request, an approval, or a claim the table cannot answer is a `500`, never an answer
that the sign-in was recorded, approved, or unknown.

**Modes.** The host boot tightens what it finds beside the control socket to owner-only
— the directory, `state.db`, `config.toml`, and the names earlier releases wrote
(`src/daemon/permissions.rs`; the socket itself in [control-channel.md](control-channel.md)).
The creating process's umask need not be as private as this content warrants — model
picks, subscriptions, the device labels — so the tightening is part of boot rather than
of each file's creation. It is one-way (a file already stricter stays so), skips absent
files, and warns without failing boot when a mode cannot be changed. The two modes,
`0700` for directories and `0600` for files, are constants in `src/record/session_dir.rs`, which the
writers of a session's log and attachments read as well.

A project's `.efference/` is owner-only by the same rule, applied by the sessions that
use it rather than at boot (`src/daemon/permissions.rs`, called from `src/boot/mint.rs` and
`src/boot/resume.rs`). Minting a session creates `.efference/` `0700` when absent and
tightens it when it exists; resuming a session tightens it before the log is reopened. The
session directory is created `0700`, and the files efference writes in it — the log
(and a torn tail's saved fragment), the `config.toml` snapshot, `usage.jsonl`, attachments
— are `0600`. The directory's mode is what protects everything beneath it, so existing
session directories and logs are not walked, and a worktree under `.efference/worktrees/`
keeps the modes git gives its checked-out files. The control socket is not peer-checked:
its owner-only mode is the boundary ([control-channel.md](control-channel.md)).

## Concurrency

Every daemon under the user shares this file, and writes to it at the same time — a
subscribing browser, a session recording its model, a browser asking to sign in. Writers
queue behind each other on sqlite's lock, with a five-second timeout before the wait
becomes an error: the alternative is a `SQLITE_BUSY` on a path a turn depends on.

No connection outlives the operation that opened it, so a read-check-write that another
daemon could interleave with is one statement or one transaction instead. A claim is a
single `DELETE … RETURNING` of the approved row, where the delete that returns it *is*
the first claim, and an approval a single `UPDATE … RETURNING` of the waiting one. A
request's prune, eviction, and insert run in one immediate transaction, so two daemons
asking at once cannot both find room under the cap, and the code's uniqueness is the
table's constraint, so a colliding draw fails its insert and is drawn again. The device
lookup reads the row and writes `last_seen_at` only when it is empty or more than an
hour old, with the staleness test repeated in the `UPDATE` itself, so two daemons
answering one device at once write it once.

## When it goes wrong

The file is ordinary sqlite, so `sqlite3 ~/.efference/state.db` is a working way to look
at it, and deleting it discards every table. What that costs differs by table: model
picks and catalog listings rebuild themselves within days, remembered launch locations
rebuild as sessions are started there again, push subscriptions need one resubscribe from
the browser's service worker, losing the pending sign-ins costs a browser that was
waiting one new code, and losing the device table signs out every browser and script,
which enroll again as new devices.
