//! Pending sign-ins (docs/web-bind.md): the `pending_sign_ins` table of the user state
//! database (docs/state-store.md), one row per browser waiting to collect a device token.
//!
//! A row comes from `POST /pair`, a browser asking to sign in, which the operator lets in
//! by typing the short code it displays into `efference devices approve` at the host; or
//! from `GET /auth/link`, a sign-in approved before it was asked for, whose claim secret
//! rides the link. Either way the browser collects its token with the claim secret at
//! `POST /pair/claim`, once. The table is user-scoped like the devices table, so the
//! socket of whichever daemon the operator reaches approves what any daemon's listener
//! took.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::devices::{Joined, clean_label};

#[cfg(test)]
mod test;

/// This table's half of the user state database ([`crate::state`]). A row is keyed by
/// its claim secret's SHA-256, so the table holds nothing that collects a token, and
/// the code is unique, so an insert that drew a live code fails rather than giving two
/// browsers one name.
///
/// `totp_burns` is the table of accepted TOTP timesteps that earlier builds kept for
/// the sign-in this one replaces. Nothing reads it, so opening this table drops it.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pending_sign_ins (
    claim BLOB PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    joined TEXT NOT NULL,
    approved INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
DROP TABLE IF EXISTS totp_burns;";

/// How long a pending sign-in lives from when it is made, in seconds: checked when it
/// is approved and when it is claimed.
pub const TTL_SECS: i64 = 90;

/// How long an approved sign-in stays collectable at least, in seconds. The TTL runs
/// from the request, so a code approved in its last second would otherwise expire
/// before the page's next one-second poll could claim it.
pub const CLAIM_GRACE_SECS: i64 = 15;

/// The most sign-ins that wait for approval at once. A request past it evicts the
/// oldest waiting one, so a stale tab never blocks a new attempt.
pub const WAITING_CAP: i64 = 8;

/// What a short code is spelled in: the digits and capitals without the look-alikes
/// `0 O 1 I L`, and without `U`, which Crockford's base32 also leaves out so that
/// codes spell fewer words.
const ALPHABET: &[u8; 30] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";

/// How many fresh codes an insert draws before giving up. A collision needs one of the
/// few live codes out of 30⁶, so a second draw is already rare.
const CODE_ATTEMPTS: usize = 8;

/// What the operator reads off the new browser and types at the host: six characters of
/// [`ALPHABET`], unique among the live pending sign-ins. It authorizes nothing — it
/// names a pending sign-in for the socket to approve, and names nothing once that one
/// is claimed or expired, so an `approve` line in shell history carries no authority.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShortCode([u8; ShortCode::LEN]);

impl ShortCode {
    pub const LEN: usize = 6;

    fn random() -> Self {
        Self(std::array::from_fn(|_| {
            ALPHABET[rand::random_range(0..ALPHABET.len())]
        }))
    }

    /// A code as a person typed it: either case, with whitespace and dashes ignored.
    /// `None` when what is left is not six characters of the alphabet.
    pub fn parse(typed: &str) -> Option<Self> {
        let mut code = [0u8; Self::LEN];
        let mut len = 0;
        for c in typed.chars().filter(|c| !c.is_whitespace() && *c != '-') {
            let c = c.to_ascii_uppercase();
            if len == Self::LEN || !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
                return None;
            }
            code[len] = c as u8;
            len += 1;
        }
        (len == Self::LEN).then_some(Self(code))
    }

    /// The stored spelling: the six characters, no dash.
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("the alphabet is ASCII")
    }
}

/// The displayed spelling, `KQ7-M4X`: two groups of three, which reads off a screen
/// and types back more reliably than six in a row.
impl std::fmt::Display for ShortCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (head, tail) = self.as_str().split_at(Self::LEN / 2);
        write!(f, "{head}-{tail}")
    }
}

/// The 128-bit value that collects a device token, once: handed to the page that asked,
/// or carried in a link's fragment, and stored only as its SHA-256. No `Debug`, because
/// it is a credential while it lives.
pub struct ClaimSecret([u8; ClaimSecret::LEN]);

impl ClaimSecret {
    pub const LEN: usize = 16;

    /// Its base64url spelling's length: sixteen bytes are 22 characters unpadded.
    const CHARS: usize = 22;

    fn random() -> Self {
        let mut bytes = [0u8; Self::LEN];
        rand::fill(&mut bytes);
        Self(bytes)
    }

    /// A presented secret, base64url without padding. `None` for anything else, which
    /// names no pending sign-in.
    pub fn parse(encoded: &str) -> Option<Self> {
        if encoded.len() != Self::CHARS {
            return None;
        }
        let mut bytes = [0u8; Self::LEN];
        (URL_SAFE_NO_PAD.decode_slice(encoded, &mut bytes).ok()? == Self::LEN)
            .then_some(Self(bytes))
    }

    /// The spelling a page and a link carry.
    pub fn encoded(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    /// What the table keys the row by.
    fn digest(&self) -> Vec<u8> {
        Sha256::digest(self.0).to_vec()
    }
}

/// What `POST /pair` hands the page that asked: the claim secret, for it alone, and the
/// code it displays. No `Debug`, because of the secret.
pub struct Requested {
    pub claim: ClaimSecret,
    pub code: ShortCode,
}

/// A pending sign-in as the approving side sees it: the label the browser gave and when
/// it asked. There is no code field, so no listing or approval answer can carry one —
/// the operator approves by typing what the browser displays, never by picking from a
/// list (docs/web-bind.md).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "PendingSignIn"))]
pub struct Pending {
    /// Display text, cleaned on the way in ([`clean_label`]).
    pub label: String,
    /// Unix seconds.
    pub created_at: i64,
}

/// What a claim found ([`Store::claim`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// Live and not yet approved: the page asks again.
    Waiting,
    /// Approved, and this was its first claim: the row is gone, and the caller enrolls
    /// the browser it describes.
    Approved { label: String, joined: Joined },
    /// Nothing live answers to the secret: claimed already, expired, evicted, or never
    /// made.
    Unknown,
}

/// The pending sign-ins. User-scoped, like the devices they become: the socket of any
/// daemon under the user approves a sign-in another daemon's listener took.
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The real location: the user state database's `pending_sign_ins` table.
    pub fn user_scoped() -> Self {
        Self::new(crate::state::user_scoped())
    }

    fn connect(&self) -> Result<rusqlite::Connection, crate::state::Error> {
        crate::state::open(&self.path, SCHEMA)
    }

    /// Record a browser asking to sign in (`POST /pair`), waiting for approval under a
    /// fresh code. When [`WAITING_CAP`] sign-ins wait already, the oldest of them goes to
    /// make room.
    pub fn request(&self, label: &str, now: SystemTime) -> Result<Requested, crate::state::Error> {
        self.insert(
            &clean_label(label, "browser"),
            Joined::Code,
            now,
            ShortCode::random,
        )
    }

    /// A sign-in approved before it was asked for (`GET /auth/link`): its claim secret
    /// goes in a link, and the page that opens the link claims at once. It waits for no
    /// approval, so it neither counts against [`WAITING_CAP`] nor is evicted by
    /// requests: a peer churning `POST /pair` cannot displace a link minted at the
    /// terminal. Its label is the kind's name until the claiming page names it.
    pub fn approved_link(&self, now: SystemTime) -> Result<ClaimSecret, crate::state::Error> {
        Ok(self
            .insert("browser", Joined::Link, now, ShortCode::random)?
            .claim)
    }

    /// Insert one row, approved when it is a link's, under a code from `fresh` that no
    /// live row holds. Expired rows are pruned first; a request then evicts down to one
    /// below [`WAITING_CAP`]. One immediate transaction, so two daemons inserting at
    /// once cannot both find room under the cap.
    fn insert(
        &self,
        label: &str,
        joined: Joined,
        now: SystemTime,
        mut fresh: impl FnMut() -> ShortCode,
    ) -> Result<Requested, crate::state::Error> {
        let approved = joined == Joined::Link;
        let now = unix_secs(now);
        let mut db = self.connect()?;
        let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune(&transaction, now)?;
        if !approved {
            transaction.execute(
                "DELETE FROM pending_sign_ins WHERE rowid IN (
                     SELECT rowid FROM pending_sign_ins WHERE approved = 0
                     ORDER BY created_at, rowid
                     LIMIT max(0, (SELECT count(*) FROM pending_sign_ins WHERE approved = 0)
                                  - ?1 + 1))",
                [WAITING_CAP],
            )?;
        }
        let mut attempts = 0;
        let requested = loop {
            let requested = Requested {
                claim: ClaimSecret::random(),
                code: fresh(),
            };
            let inserted = transaction.execute(
                "INSERT INTO pending_sign_ins
                     (claim, code, label, joined, approved, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    requested.claim.digest(),
                    requested.code.as_str(),
                    label,
                    joined,
                    approved,
                    now,
                    now + TTL_SECS
                ],
            );
            attempts += 1;
            match inserted {
                Ok(_) => break requested,
                // A live row holds the code: the statement is rolled back alone, and the
                // next draw goes into the same transaction.
                Err(e)
                    if attempts < CODE_ATTEMPTS
                        && e.sqlite_error_code()
                            == Some(rusqlite::ErrorCode::ConstraintViolation) =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        };
        transaction.commit()?;
        Ok(requested)
    }

    /// Approve the waiting sign-in `code` names, answering what it is so the operator
    /// sees what they let in. `None` when no live sign-in waits under the code: never
    /// made, expired, claimed, or approved already, since a code is approved once.
    pub fn approve(
        &self,
        code: &ShortCode,
        now: SystemTime,
    ) -> Result<Option<Pending>, crate::state::Error> {
        let db = self.connect()?;
        let now = unix_secs(now);
        prune(&db, now)?;
        Ok(db
            .query_row(
                "UPDATE pending_sign_ins
                 SET approved = 1, expires_at = max(expires_at, ?2 + ?3)
                 WHERE code = ?1 AND approved = 0 AND expires_at > ?2
                 RETURNING label, created_at",
                params![code.as_str(), now, CLAIM_GRACE_SECS],
                |row| {
                    Ok(Pending {
                        label: row.get(0)?,
                        created_at: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Collect a sign-in with its claim secret ([`Claim`]). The first claim after
    /// approval deletes the row, so a second finds nothing: the delete is the check, and
    /// two daemons answering one secret at once enroll one device between them.
    pub fn claim(
        &self,
        claim: &ClaimSecret,
        now: SystemTime,
    ) -> Result<Claim, crate::state::Error> {
        let db = self.connect()?;
        let now = unix_secs(now);
        prune(&db, now)?;
        let digest = claim.digest();
        let collected = db
            .query_row(
                "DELETE FROM pending_sign_ins
                 WHERE claim = ?1 AND approved = 1 AND expires_at > ?2
                 RETURNING label, joined",
                params![digest, now],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Joined>(1)?)),
            )
            .optional()?;
        if let Some((label, joined)) = collected {
            return Ok(Claim::Approved { label, joined });
        }
        let waiting = db
            .query_row(
                "SELECT 1 FROM pending_sign_ins WHERE claim = ?1 AND expires_at > ?2",
                params![digest, now],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        Ok(if waiting {
            Claim::Waiting
        } else {
            Claim::Unknown
        })
    }

    /// The live sign-ins waiting for approval, oldest first: label and age, never a
    /// code. A read, so expired rows are passed over rather than pruned.
    pub fn waiting(&self, now: SystemTime) -> Result<Vec<Pending>, crate::state::Error> {
        let db = self.connect()?;
        let mut statement = db.prepare(
            "SELECT label, created_at FROM pending_sign_ins
             WHERE approved = 0 AND expires_at > ?1
             ORDER BY created_at, rowid",
        )?;
        let waiting = statement
            .query_map([unix_secs(now)], |row| {
                Ok(Pending {
                    label: row.get(0)?,
                    created_at: row.get(1)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(waiting)
    }

    /// Drop every pending sign-in, answering how many there were — what root rotation
    /// does beside deleting the devices, so a sign-in approved under the old root cannot
    /// enroll under the next (docs/web-bind.md).
    pub fn clear(&self) -> Result<usize, crate::state::Error> {
        Ok(self
            .connect()?
            .execute("DELETE FROM pending_sign_ins", [])?)
    }
}

/// Drop what has expired. Every write runs it, so the table holds what is live and what
/// expired since the last write, nothing older.
fn prune(db: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    db.execute("DELETE FROM pending_sign_ins WHERE expires_at <= ?1", [now])
}

/// Seconds since the Unix epoch; a pre-1970 clock reads as 0.
fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
