# The user state database

One sqlite file, `~/.efference/state.db`, holds the host's persistent state: which models
sessions have picked, each connection's model listing, the browsers subscribed to push, the
TOTP timesteps already spent, and the devices enrolled at the web bind. A push subscription
carries the material that delivers to that browser, so the file and its directory are
owner-only — see **Modes** below. The machine root, API keys, and OAuth grants stay in the
OS credential store ([secrets.md](secrets.md)). The configuration is still a text file
(`config.toml`, [config-format.md](../assets/guide/config-format.md)); whether it should move here is an
open question ([backlog/inbox/user-state-store.md](../backlog/inbox/user-state-store.md)).

Module: `src/state.rs` owns the path and the open; each table's own module owns its
schema.

## The tables

| Table | One row per | Held by | What reads it |
|---|---|---|---|
| `model_picks` | an explicit model pick | `src/history.rs` | the recent menu and the omitted-branch default ([transport.md](transport.md)) |
| `triage_picks` | an explicit separate triage pick, with the main model it ran with | `src/history.rs` | nothing yet; recorded for a later pairing default |
| `model_catalog` | a connection | `src/catalog.rs` | the model menus ([model-catalog.md](model-catalog.md)) |
| `remembered_projects` | a directory a session was started in | `src/remembered.rs` | the launch index, `GET /projects` ([control-api.md](control-api.md)) |
| `session_ids` | a session id and the root holding it | `src/session_ids.rs` | id minting, explicit names and resume ([session-title.md](session-title.md)) |
| `push_subscriptions` | a browser subscription | `src/push.rs` | the sender, per push ([web-push.md](web-push.md)) |
| `totp_burns` | an accepted TOTP timestep | `src/auth.rs` | the bind's code check ([web-bind.md](web-bind.md)) |
| `devices` | an enrolled device — one TCP credential | `src/devices.rs` | the gate, per request; `efference devices` ([web-bind.md](web-bind.md)) |

The burn ledger, the subscriptions, and the devices are user-scoped for a security reason
rather than a convenience one. The TOTP secret is derived from the machine root, so every daemon under the user shares it
and must share its burns too — a per-project ledger would leave a cross-repo replay
window. The VAPID key is derived from the same root, so every daemon can send to every
subscription, and a per-project store would hide a subscription from the session that
ought to reach it. The device-token key is derived from it as well, so a device's token
works at every daemon under the user, and a per-project table would leave a device
revoked in one place signed in at another. Each would be wrong per-project in the same
direction.

The `devices` table holds no secret: an id is public — it is the readable half of the
token — and a token for it also needs the key derived from the root, which is in the
credential store. A row is id, kind (`browser` or `script`), label, how the device
joined, when it was created, and when it was last seen (written at most hourly per
device).

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
which, like the pick beside it, never fails the creation writing it. Two are the
exception, and deliberately. The burn ledger reports the error and the caller rejects
the code rather than accept one it cannot prove single-use; its `is_burned` read, which
only chooses which code to hand a user, stays optimistic. The device table reports it
too: the gate refuses a request it cannot check against the table — a `500`, so the
client keeps its token rather than discarding it as bad — a sign-in whose device cannot
be recorded mints nothing, and a listing that cannot be read is an error rather than an
empty list, which would hide a row whose token still works.

**Modes.** The host boot tightens what it finds beside the control socket to owner-only
— the directory, `state.db`, `config.toml`, and the names earlier releases wrote
(`src/daemon/permissions.rs`; the socket itself in [control-channel.md](control-channel.md)).
The creating process's umask need not be as private as this content warrants — model
picks, subscriptions, the burn ledger, the device labels — so the tightening is part of boot rather than
of each file's creation. It is one-way (a file already stricter stays so), skips absent
files, and warns without failing boot when a mode cannot be changed.

## Concurrency

Every daemon under the user shares this file, and writes to it at the same time — a
subscribing browser, a session recording its model, an accepted code. Writers queue
behind each other on sqlite's lock, with a five-second timeout before the wait becomes
an error: the alternative is a `SQLITE_BUSY` on a path a turn depends on.

No connection is held across a read-check-write, because there is no way to hold one.
The burn ledger's check-and-burn is therefore a single statement —
`INSERT OR IGNORE` against a step primary key, where a conflicting insert *is* the
replay — rather than a read followed by a write that another daemon could interleave
with. The device lookup reads the row and writes `last_seen_at` only when it is empty
or more than an hour old, with the staleness test repeated in the `UPDATE` itself, so
two daemons answering one device at once write it once.

## When it goes wrong

The file is ordinary sqlite, so `sqlite3 ~/.efference/state.db` is a working way to look
at it, and deleting it discards every table. What that costs differs by table: model
picks and catalog listings rebuild themselves within days, remembered launch locations
rebuild as sessions are started there again, push subscriptions need one resubscribe from
the browser's service worker, the burn ledger re-opens at most the
current ~90-second skew window — bounded, because a code outside that window can no
longer verify whatever the ledger says about it — and losing the device table signs out
every browser and script, which enroll again as new devices.
