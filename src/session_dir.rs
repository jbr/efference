//! The on-disk shape of a session directory (docs/control-channel.md, docs/state-store.md):
//! the directory session dirs live under, the name of the log inside one, the ids that may
//! name one, and the owner-only modes they and their files are created with.
//!
//! The worktree placement, the id register, the archived-session scan, discard, the log
//! writer and the attachment store each read these, and none of them needs the host that
//! serves a session. The module imports nothing from the crate, so every one of them can
//! import it (docs/README.md, *Dependency direction*).

/// The directory every session dir lives under, relative to the working directory a
/// session was started in (docs/control-channel.md). One session is
/// `<SESSION_BASE>/<id>/`, holding standard-named files: its log ([`LOG_NAME`]) and its
/// config snapshot.
pub const SESSION_BASE: &str = ".efference";

/// The name of the log inside a session dir (docs/log-and-projection.md).
pub const LOG_NAME: &str = "log.jsonl";

/// Owner-only mode for the state directory, a session directory, and the host socket:
/// nothing another local account can traverse or connect to.
pub(crate) const DIR_MODE: u32 = 0o700;

/// Owner read/write for the state files.
pub(crate) const FILE_MODE: u32 = 0o600;

/// Whether `id` is a single plain path segment — ASCII alphanumerics, `-`, `_`, `.`,
/// and neither `.` nor `..`. Anything else names no session: an id arrives from a
/// URL and is joined onto a filesystem path, so it is validated before the join
/// rather than trusted. The web client's base derivation (`web/src/api/base.ts`)
/// accepts the same set.
pub(crate) fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod test;
