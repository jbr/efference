//! Enrolled devices (docs/web-bind.md): the `devices` table of the user state database
//! (docs/state-store.md), one row per TCP credential.
//!
//! The row is the whole of a device's standing. Its token is derived from the machine
//! root rather than stored ([`crate::auth::DeviceTokens`]), so the table holds public
//! ids and display text, and revoking one device is deleting its row: the gate reads
//! the row on every request, and every daemon under the user reads the same table.

use rusqlite::OptionalExtension;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod test;

/// This table's half of the user state database ([`crate::state`]). The id is the
/// primary key, so an insert that drew an id already present fails rather than
/// replacing the row that holds it.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS devices (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    joined TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER
);";

/// How old `last_seen_at` must be before a request writes it again: a burst of
/// requests is one read each, and at most one write per device per interval.
pub const SEEN_INTERVAL_SECS: i64 = 60 * 60;

/// The most characters a label keeps.
pub const LABEL_MAX_CHARS: usize = 64;

/// An enrolled device's id: 16 bytes from the OS random source, drawn fresh for
/// every enrollment, so a revoked id is never issued again. Public — it is the
/// readable half of a token and the table's key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(with = "String"))]
pub struct DeviceId([u8; DeviceId::LEN]);

impl DeviceId {
    pub const LEN: usize = 16;

    pub fn random() -> Self {
        let mut bytes = [0u8; Self::LEN];
        rand::fill(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Lowercase hex: the spelling the table, the routes, and the command line share.
    pub fn hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }

    /// The first eight hex digits, which `efference devices` lists a device by.
    pub fn short(&self) -> String {
        self.hex()[..8].to_string()
    }

    /// A whole id in hex, either case.
    pub fn parse_hex(hex: &str) -> Option<Self> {
        let bytes = data_encoding::HEXLOWER_PERMISSIVE
            .decode(hex.as_bytes())
            .ok()?;
        bytes.try_into().ok().map(Self)
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.hex())
    }
}

impl std::fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceId({})", self.hex())
    }
}

impl Serialize for DeviceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        Self::parse_hex(&hex)
            .ok_or_else(|| serde::de::Error::custom(format!("{hex:?} is not a device id")))
    }
}

impl ToSql for DeviceId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.hex()))
    }
}

impl FromSql for DeviceId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::parse_hex(text).ok_or_else(|| unrecognized("id", text))
    }
}

/// What holds a device's token.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "DeviceKind"))]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A browser that signed in, keeping the token in its own storage.
    Browser,
    /// A program given a token by `efference devices add`.
    Script,
}

impl Kind {
    pub const ALL: [Self; 2] = [Self::Browser, Self::Script];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Script => "script",
        }
    }
}

/// How a device got its token.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "DeviceJoined"))]
#[serde(rename_all = "lowercase")]
pub enum Joined {
    /// A browser showed a short code, the operator approved it at the host
    /// (`efference devices approve`), and the browser claimed it (`crate::sign_ins`).
    Code,
    /// A browser opened a sign-in link minted at the host — the one the host prints,
    /// `efference dev open`'s, an island's — and claimed it (`crate::sign_ins`).
    Link,
    /// `efference devices add`, over the host's socket.
    Add,
}

impl Joined {
    pub const ALL: [Self; 3] = [Self::Code, Self::Link, Self::Add];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Link => "link",
            Self::Add => "add",
        }
    }
}

impl ToSql for Kind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for Kind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or_else(|| unrecognized("kind", text))
    }
}

impl ToSql for Joined {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for Joined {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::ALL
            .into_iter()
            .find(|joined| joined.as_str() == text)
            .ok_or_else(|| unrecognized("joined", text))
    }
}

/// A stored value this build does not name. Only this module writes the table, so
/// this is a row from another build, and a listing fails on it rather than leaving
/// out a row whose token still authenticates.
#[derive(Debug, thiserror::Error)]
#[error("unrecognized device {column} {value:?}")]
struct Unrecognized {
    column: &'static str,
    value: String,
}

fn unrecognized(column: &'static str, value: &str) -> FromSqlError {
    FromSqlError::other(Unrecognized {
        column,
        value: value.to_string(),
    })
}

/// One row: an enrolled device, the holder of one TCP credential.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Device {
    pub id: DeviceId,
    pub kind: Kind,
    /// Display text, cleaned on the way in ([`clean_label`]).
    pub label: String,
    pub joined: Joined,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds of the last request recorded, written at most every
    /// [`SEEN_INTERVAL_SECS`]; `None` before the first.
    pub last_seen_at: Option<i64>,
}

/// A device just enrolled, with its token: the one answer that carries the token.
/// No `Debug`, because the token is a credential.
#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Enrolled {
    pub device: Device,
    pub token: String,
}

/// A label fit to print. It is requester-supplied text that reaches the operator's
/// terminal, so terminal escape sequences are dropped whole
/// (`crate::process_monitor::strip_formatting`, the parser process output goes
/// through) rather than leaving their text printed, remaining control characters and the [`reorders`] characters become spaces,
/// whitespace runs collapse, and the result is cut to [`LABEL_MAX_CHARS`]; what is left
/// empty is `fallback`. `crate::attachments::sanitize` does the same for a file name.
pub fn clean_label(suggested: &str, fallback: &str) -> String {
    let spaced: String = crate::process_monitor::strip_formatting(suggested)
        .chars()
        .map(|c| match c.is_control() || reorders(c) {
            true => ' ',
            false => c,
        })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = collapsed.chars().take(LABEL_MAX_CHARS).collect();
    match cut.trim_end() {
        "" => fallback.to_string(),
        label => label.to_string(),
    }
}

/// The bidirectional-formatting characters: not control characters, but each one
/// reorders the text after it on screen.
fn reorders(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// The enrolled devices. User-scoped, like the pending sign-ins beside them: every
/// daemon under the user derives the same token key, so every one must read the same
/// rows, or a device revoked through one would still sign in at another.
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The real location: the user state database's `devices` table.
    pub fn user_scoped() -> Self {
        Self::new(crate::state::user_scoped())
    }

    fn connect(&self) -> Result<rusqlite::Connection, crate::state::Error> {
        crate::state::open(&self.path, SCHEMA)
    }

    /// Enroll a device under a fresh id. The label is cleaned here, so every path
    /// into the table is; an empty one is the kind's name.
    pub fn insert(
        &self,
        kind: Kind,
        label: &str,
        joined: Joined,
        now: SystemTime,
    ) -> Result<Device, crate::state::Error> {
        let device = Device {
            id: DeviceId::random(),
            kind,
            label: clean_label(label, kind.as_str()),
            joined,
            created_at: unix_secs(now),
            last_seen_at: None,
        };
        self.connect()?.execute(
            "INSERT INTO devices (id, kind, label, joined, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                device.id,
                device.kind,
                device.label,
                device.joined,
                device.created_at
            ],
        )?;
        Ok(device)
    }

    /// Whether `id` names an enrolled device, recording it seen. The row's existence is
    /// the whole answer; `last_seen_at` is written only when it is empty or older than
    /// [`SEEN_INTERVAL_SECS`], and a write that fails is logged and leaves the answer
    /// standing, since the row was read.
    ///
    /// An error is a table this could not read, and the caller refuses the request
    /// rather than admit a token it could not check.
    pub fn seen(&self, id: &DeviceId, now: SystemTime) -> Result<bool, crate::state::Error> {
        let db = self.connect()?;
        let Some(last_seen) = db
            .query_row(
                "SELECT last_seen_at FROM devices WHERE id = ?1",
                [id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
        else {
            return Ok(false);
        };
        let now = unix_secs(now);
        if last_seen.is_none_or(|at| now - at > SEEN_INTERVAL_SECS)
            && let Err(e) = db.execute(
                // The staleness test repeats in the statement, so two daemons
                // answering the same device at once write it once.
                "UPDATE devices SET last_seen_at = ?2
                 WHERE id = ?1 AND (last_seen_at IS NULL OR last_seen_at < ?2 - ?3)",
                rusqlite::params![id, now, SEEN_INTERVAL_SECS],
            )
        {
            log::warn!("could not record device {} as seen: {e}", id.short());
        }
        Ok(true)
    }

    /// Every enrolled device, oldest first. A row this build cannot read fails the
    /// listing ([`Unrecognized`]) rather than being left out of it.
    pub fn list(&self) -> Result<Vec<Device>, crate::state::Error> {
        let db = self.connect()?;
        let mut statement = db.prepare(
            "SELECT id, kind, label, joined, created_at, last_seen_at
             FROM devices ORDER BY created_at, id",
        )?;
        let devices = statement
            .query_map([], |row| {
                Ok(Device {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    label: row.get(2)?,
                    joined: row.get(3)?,
                    created_at: row.get(4)?,
                    last_seen_at: row.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(devices)
    }

    /// Revoke one device: `Ok(false)` when no row had that id.
    pub fn remove(&self, id: &DeviceId) -> Result<bool, crate::state::Error> {
        Ok(self
            .connect()?
            .execute("DELETE FROM devices WHERE id = ?1", [id])?
            > 0)
    }

    /// Revoke every device, answering how many there were — what root rotation does
    /// beside replacing the root (docs/web-bind.md).
    pub fn clear(&self) -> Result<usize, crate::state::Error> {
        Ok(self.connect()?.execute("DELETE FROM devices", [])?)
    }
}

/// Seconds since the Unix epoch; a pre-1970 clock reads as 0.
fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
