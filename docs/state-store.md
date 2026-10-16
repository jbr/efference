# The user state database

One sqlite file, `~/.efference/state.db`, holds the host's persistent state that is not a
secret: which models sessions have picked, each connection's model listing, the browsers
subscribed to push, and the TOTP timesteps already spent. The values that *are* secrets —
the machine root, API keys, OAuth grants — are in the OS credential store instead
([secrets.md](secrets.md)). The configuration is still a text file
(`config.toml`, [config-format.md](../assets/guide/config-format.md)); whether it should move here is an
open question ([backlog/inbox/user-state-store.md](../backlog/inbox/user-state-store.md)).

Module: `src/state.rs` owns the path and the open; each table's own module owns its
schema.

## The tables

| Table | One row per | Held by | What reads it |
|---|---|---|---|
| `model_picks` | an explicit model pick | `src/history.rs` | the recent menu and the omitted-branch default ([transport.md](transport.md)) |
| `model_catalog` | a connection | `src/catalog.rs` | the model menus ([model-catalog.md](model-catalog.md)) |
| `remembered_projects` | a directory a session was started in | `src/remembered.rs` | the launch index, `GET /projects` ([control-api.md](control-api.md)) |
| `push_subscriptions` | a browser subscription | `src/push.rs` | the sender, per push ([web-push.md](web-push.md)) |
| `totp_burns` | an accepted TOTP timestep | `src/auth.rs` | the bind's token check ([web-bind.md](web-bind.md)) |

Two of these are user-scoped for a security reason rather than a convenience one. The
TOTP secret is derived from the machine root, so every daemon under the user shares it
and must share its burns too — a per-project ledger would leave a cross-repo replay
window. The VAPID key is derived from the same root, so every daemon can send to every
subscription, and a per-project store would hide a subscription from the session that
ought to reach it. Both would be wrong per-project in the same direction.

## Opening

Every operation opens its own connection, passing the calling module's
`CREATE TABLE IF NOT EXISTS` statements to `state::open`. A pooled or shared connection
would buy nothing — each store is touched once per session creation, once per menu
preview, once per push — and `Connection` is `Send` but not `Sync`. Applying the schema
on every open is the migration story: a new table lands in its own module beside the
existing ones, and a new column lands as an `ALTER TABLE` in a follow-up change.
Nothing is read back at startup, so there is no schema version to number.

An open that fails is the *table's* problem rather than the store's, and the caller
decides what that means. Four of the five fold the error away: an empty catalog still
serves from memory, absent picks still fall back to the config's choice, an unreadable
subscription table means nobody is subscribed, a push that does not go out cannot fail a
turn, and a remembered location that cannot be stored is a picker entry that does not
appear — which, like the pick beside it, never fails the creation writing it. The burn
ledger is the exception, and deliberately: it reports the error and the caller rejects
the code rather than accept one it cannot prove single-use. Its `is_burned` read, which
only chooses which code to hand a user, stays optimistic.

## Concurrency

Every daemon under the user shares this file, and writes to it at the same time — a
subscribing browser, a session recording its model, an accepted token. Writers queue
behind each other on sqlite's lock, with a five-second timeout before the wait becomes
an error: the alternative is a `SQLITE_BUSY` on a path a turn depends on.

No connection is held across a read-check-write, because there is no way to hold one.
The burn ledger's check-and-burn is therefore a single statement —
`INSERT OR IGNORE` against a step primary key, where a conflicting insert *is* the
replay — rather than a read followed by a write that another daemon could interleave
with.

## When it goes wrong

The file is ordinary sqlite, so `sqlite3 ~/.efference/state.db` is a working way to look
at it, and deleting it discards all five tables. What that costs differs by table: model
picks and catalog listings rebuild themselves within days, remembered launch locations
rebuild as sessions are started there again, push subscriptions need one resubscribe from
the browser's service worker, and the burn ledger re-opens at most the
current ~90-second skew window — bounded, because a code outside that window can no
longer verify whatever the ledger says about it.
