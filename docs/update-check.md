# The update check

efference can tell its user that a newer release is out. It does not replace its own
binary: the install command (`curl -fsSL https://efference.net/install.sh | sh`) installs
over an existing copy, and that command is what the check points at. Nothing else
efference does contacts efference.net, so the automatic check runs only with the user's
answer.

## What is asked, and what is believed

The release workflow publishes `releases/latest.json` on efference.net for each stable
release — `{"version": "0.2.0", "tag": "v0.2.0", "url": "https://efference.net/releases/v0.2.0"}`
— with `latest.json.minisig` beside it (the bucket layout is in `process/releasing.md`).
A check is one GET for each, through the shared HTTP client ([transport.md](transport.md)),
so the requests carry the address and the `User-Agent` with efference's version and
nothing else (`src/platform/update_check.rs`).

The document is believed only when its minisign signature checks against the release
public key compiled into the binary (`RELEASE_PUBLIC_KEY`), verified in-process by
`minisign-verify`, **and** the signature's trusted comment is exactly
`efference <tag> latest.json` for the tag the document names. The trusted comment is
covered by the signature, so a valid signature made for another file or another release
cannot be presented for this one. The signature is checked before the document is parsed.
Anything short of both checks — an unreachable server, a status other than `200`, a
missing or unverifiable signature, a comment for another tag or file, a document that is
not a release — is a failed check, never an available version. A binary carries only the
key it was built with, so rotating the release key leaves every older binary unable to
verify a newer release.

A release is newer when its `version` is greater than the running build's
(`CARGO_PKG_VERSION`) as semantic versions; a stable release is newer than a prerelease of
the same version. `latest.json` names stable releases only.

## Who asks, and when

- **`efference update --check`** asks once, in its own process, and prints the installed
  and newest versions, and the install command when the newest is newer
  (`src/bin/efference/update.rs`). Running it is the permission, so it asks whatever the
  configuration says, and it needs no host. `efference update` without `--check` refuses,
  naming the check and the install command. `efference --version` prints the version, then the git revision (`+dirty` with uncommitted changes) and cargo profile directory the binary was built from: `0.1.3-dev (a1b2c3d4e+dirty, debug)`. A version with a `-dev` suffix is a build from the tree between releases (process/releasing.md); the revision and profile are stamped by `build.rs` and left out where git or the target layout is unavailable.
- **A host asks automatically only when `[updates] check = true`** in
  `~/.efference/config.toml` (`src/config/user_config/updates.rs`). A file without the key never
  checks; the key is not a default to find and turn off. `efference setup` asks the
  question, yes preselected, wherever neither the file nor the run has an answer: last on a
  first run, and on leaving the management list otherwise, whose "Update check" entry
  changes an answer already given (`src/setup/updates.rs`). An island is a host here and
  checks the same way.

The host's check (`src/host/update_check.rs`) runs under the host's `Swansong`:
at start, then again a day after each answered check, or an hour after a failed one. The
wait is measured on the wall clock, read every five minutes, because a monotonic sleep
stops counting while a machine sleeps. A failure is logged once per run of failures
(`warn`) and changes nothing a client reads; the last answer stands. A newer release found
is logged at `info`. The check never installs anything.

## Where clients read the answer

The answer is host state: `Host` holds one `NewerVersion` (`src/session/newer_version.rs`)
and hands it to every session it adopts, as it does the attention table. Each hosted
session publishes it as the `newer_version` state topic — `{version, tag, url,
update_command}`, or `null` while no newer release is known — and the host wakes every
hosted session's topic watcher when the value moves ([control-api.md](control-api.md)).
It is a session topic because a session's `/events` stream is what both clients follow:
the TUI follows nothing host-wide, and the web client's one host-wide subscription, the
session listing, carries rows of sessions. A starting session does not publish the
topic, and a session outside a host publishes `null`.

The TUI states `efference X.Y is available` at the right edge of its status bar, dimmed,
where the line has room after the session's own status ([tui.md](tui.md)); the web client
states it after the turn readout, with the install command as its hover title
([web-client.md](web-client.md)). Both placements are first ones.
