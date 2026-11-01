//! Web push (docs/web-push.md): the sender half of "step away from the keyboard".
//!
//! Two events mean the session needs its operator — a pending approval gate and a
//! settled turn — and both happen inside a live daemon, so each daemon sends push for
//! its own events. There is no watcher process and no per-session state: the VAPID
//! keypair is derived from the machine root (`auth.rs`) and never stored, and the
//! subscriptions the browser produced are rows in one machine-scoped table every
//! daemon reads ([`crate::platform::state`], docs/state-store.md).
//!
//! The three pieces here: [`Subscription`] and [`Store`] (what the browser handed over,
//! and where it is kept), `encrypt` (RFC 8291 payload encryption), and `vapid`
//! (RFC 8292 self-identification). [`send`] is the RFC 8030 request that carries them.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

mod encrypt;
mod vapid;

#[cfg(test)]
mod test;

/// How long a push service should hold an undelivered message, in seconds
/// (RFC 8030 §5.2). Short on purpose: both events this harness pushes are "the
/// session wants you *now*", and a gate answered an hour later is a notification
/// about a moment that has passed.
const TTL_SECS: u32 = 600;

/// One browser subscription, in the shape `PushSubscription.toJSON()` produces
/// minus `expirationTime` (which the push services in use leave null, and which
/// the store would only be able to act on by deleting an entry a `410` already
/// deletes).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    /// The opaque push resource URL. Also the identity of the subscription: the
    /// browser mints a fresh one per subscribe, so it is what an upsert keys on
    /// and what an unsubscribe names.
    pub endpoint: String,
    /// The subscription's P-256 public key, uncompressed X9.62, base64url.
    pub p256dh: String,
    /// The 16-octet authentication secret, base64url.
    pub auth: String,
}

/// A subscription as the browser serializes one — `{endpoint, keys: {p256dh, auth}}`.
/// Accepted verbatim at `POST /v1/push/subscriptions` so the client can forward
/// `subscription.toJSON()` without reshaping it.
#[derive(Deserialize)]
pub struct SubscriptionJson {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
}

#[derive(Deserialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

impl From<SubscriptionJson> for Subscription {
    fn from(json: SubscriptionJson) -> Self {
        Self {
            endpoint: json.endpoint,
            p256dh: json.keys.p256dh,
            auth: json.keys.auth,
        }
    }
}

/// What a push message says: the two lines a notification shows, plus the client
/// path a click opens.
#[derive(Serialize, Clone, Debug)]
pub struct Message {
    pub title: String,
    pub body: String,
    /// Where `notificationclick` navigates, relative to the origin the
    /// subscription was created on: the session's web path (docs/web-bind.md) —
    /// `/` on the session's own bind, `/sessions/<id>/` under a host. The
    /// sender names it (docs/web-push.md); the service worker opens whatever
    /// it is named.
    pub path: String,
}

impl Message {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            path: "/".into(),
        }
    }
}

/// The subscriptions this machine will send to.
///
/// Machine-scoped for the same reason the enrolled devices are user-scoped
/// (`devices.rs`): the VAPID key is derived from the machine root, so every daemon
/// under it can send to every subscription, and a per-repo store would hide a
/// subscription from the session that ought to reach it. Writes are rare —
/// subscribe, unsubscribe, and a prune when a push service reports the endpoint
/// gone — and each one touches one row.
pub struct Store {
    path: PathBuf,
}

/// This store's half of the user state database ([`crate::platform::state`],
/// docs/state-store.md). The endpoint is the primary key: the browser mints a
/// fresh one per subscribe, so it is what an upsert replaces on and what an
/// unsubscribe names.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS push_subscriptions (
    endpoint TEXT PRIMARY KEY,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL
);";

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The real location: the user state database's `push_subscriptions` table.
    pub fn machine_scoped() -> Self {
        Self::new(crate::platform::state::user_scoped())
    }

    fn connect(&self) -> Result<rusqlite::Connection, crate::platform::state::Error> {
        crate::platform::state::open(&self.path, SCHEMA)
    }

    /// Every stored subscription. An empty table is an empty list, not an error:
    /// "nobody has subscribed" is the ordinary state, and every caller would
    /// otherwise have to spell it. A store that cannot be opened or read reads the
    /// same way, with a log line — a push that does not go out must never be able
    /// to fail a turn.
    pub fn load(&self) -> Vec<Subscription> {
        let Ok(db) = self.connect() else {
            log::warn!("push store {} unreadable", self.path.display());
            return Vec::new();
        };
        let Ok(mut rows) =
            db.prepare("SELECT endpoint, p256dh, auth FROM push_subscriptions ORDER BY endpoint")
        else {
            log::warn!("push store {} unreadable", self.path.display());
            return Vec::new();
        };
        // A row that does not read is a row that is not there, which is how the
        // file store read an entry it could not parse.
        rows.query_map([], |row| {
            Ok(Subscription {
                endpoint: row.get(0)?,
                p256dh: row.get(1)?,
                auth: row.get(2)?,
            })
        })
        .map_or_default(|rows| rows.filter_map(Result::ok).collect())
    }

    /// Record a subscription, replacing any row with the same endpoint. The
    /// browser mints a new endpoint per subscribe, so this is an upsert rather
    /// than an append mostly to keep a repeated POST idempotent.
    pub fn upsert(&self, subscription: &Subscription) -> Result<(), crate::platform::state::Error> {
        self.connect()?.execute(
            "INSERT OR REPLACE INTO push_subscriptions (endpoint, p256dh, auth)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![
                subscription.endpoint,
                subscription.p256dh,
                subscription.auth
            ],
        )?;
        Ok(())
    }

    /// Forget one endpoint. Silent when it is not there — an unsubscribe is a
    /// statement about the end state, and a browser may unsubscribe twice.
    pub fn remove(&self, endpoint: &str) -> Result<(), crate::platform::state::Error> {
        self.connect()?.execute(
            "DELETE FROM push_subscriptions WHERE endpoint = ?1",
            [endpoint],
        )?;
        Ok(())
    }
}

/// A short, stable name for a subscription that does not carry its secret: the
/// endpoint's host and the first eight hex digits of a SHA-256 over the whole
/// endpoint. The host says which vendor's service answered; the hash tells two
/// browser profiles on one vendor apart (docs/web-push.md).
pub fn device_label(endpoint: &str) -> (String, String) {
    use sha2::Digest;
    let host = vapid::audience(endpoint)
        .and_then(|audience| audience.split_once("://").map(|(_, host)| host.to_string()))
        .unwrap_or_else(|| "unparsed".to_string());
    let digest = sha2::Sha256::digest(endpoint.as_bytes());
    let hash = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    (host, hash)
}

/// What a push service said to one push (RFC 8030 §7.3). The session records it in its
/// log as a `DeliveryOutcome` (docs/web-push.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceAnswer {
    /// A 2xx.
    Delivered { status: u16 },
    /// 404 or 410: the subscription no longer exists and was pruned.
    Gone { status: u16 },
    /// Any other status (`status` set) or a failure before an answer.
    Failed { status: Option<u16>, reason: String },
}

/// One subscription's answer to one push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceAnswer {
    /// The endpoint's host (`web.push.apple.com`), which names the browser vendor.
    pub host: String,
    /// A short stable hash of the endpoint, so two profiles on one vendor differ
    /// without the log holding the endpoint's secret path.
    pub device: String,
    /// What its push service said.
    pub answer: ServiceAnswer,
}

/// The most characters of a failure reason the log keeps.
const REASON_MAX_CHARS: usize = 120;

/// A failure reason fit for the log: the endpoint (whose path is a secret) is
/// replaced and the text is cut short.
fn log_reason(reason: &str, endpoint: &str) -> String {
    let reason = reason.replace(endpoint, "<endpoint>");
    reason.chars().take(REASON_MAX_CHARS).collect()
}

fn failed(reason: impl AsRef<str>, endpoint: &str) -> ServiceAnswer {
    ServiceAnswer::Failed {
        status: None,
        reason: log_reason(reason.as_ref(), endpoint),
    }
}

/// What a status code from a push service means (RFC 8030 §7.3): 2xx delivered,
/// 404 and 410 gone (the subscription is pruned; the browser resubscribes on its
/// own next page open), anything else a failure that is logged and dropped — a
/// push is a courtesy, and no retry schedule would make it more timely than the
/// next event.
fn classify(status: u16) -> ServiceAnswer {
    match status {
        200..300 => ServiceAnswer::Delivered { status },
        404 | 410 => ServiceAnswer::Gone { status },
        _ => ServiceAnswer::Failed {
            status: Some(status),
            reason: format!("status {status}"),
        },
    }
}

/// Send one message to one subscription (RFC 8030). `now` is a parameter so the
/// token's expiry is not read inside the send.
async fn send_one(
    key: &p256::SecretKey,
    subscription: &Subscription,
    message: &Message,
    now: u64,
) -> ServiceAnswer {
    let endpoint = subscription.endpoint.as_str();
    let Some(audience) = vapid::audience(endpoint) else {
        return failed("endpoint is not a URL", endpoint);
    };
    let (Ok(p256dh), Ok(auth)) = (
        B64.decode(&subscription.p256dh),
        B64.decode(&subscription.auth),
    ) else {
        return failed("subscription keys are not base64url", endpoint);
    };
    let Ok(ua_public) = p256::PublicKey::from_sec1_bytes(&p256dh) else {
        return failed("p256dh is not a P-256 point", endpoint);
    };
    let payload = serde_json::to_vec(message).expect("a Message serializes");
    let Ok(body) = encrypt::encrypt(&ua_public, &auth, &payload) else {
        return failed("payload encryption failed", endpoint);
    };

    let result = crate::platform::http::client()
        .post(endpoint)
        .with_request_header("ttl", TTL_SECS.to_string())
        .with_request_header("content-encoding", "aes128gcm")
        // Both events are the session waiting on a person, which is what
        // `high` means to a push service that batches by urgency (RFC 8030 §5.3).
        .with_request_header("urgency", "high")
        .with_request_header(
            "authorization",
            vapid::authorization(key, &audience, now + vapid::TOKEN_TTL_SECS),
        )
        .with_body(body)
        .await;
    match result {
        Ok(conn) => classify(conn.status().map_or(0, u16::from)),
        Err(e) => failed(e.to_string(), endpoint),
    }
}

/// Send one message to every stored subscription, pruning the ones the push
/// service reports gone, and return each device's answer (empty when nothing is
/// subscribed). Never fails: the caller is a spawned task beside the turn loop,
/// and a push that does not go out is not a session failure. The caller records
/// the answers in the session log (docs/web-push.md).
pub async fn send(store: &Store, key: &p256::SecretKey, message: Message) -> Vec<DeviceAnswer> {
    let subscriptions = store.load();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut devices = Vec::with_capacity(subscriptions.len());
    for subscription in subscriptions {
        let answer = send_one(key, &subscription, &message, now).await;
        let (host, device) = device_label(&subscription.endpoint);
        log::debug!("push to {host} ({device}): {answer:?}");
        if matches!(answer, ServiceAnswer::Gone { .. })
            && let Err(e) = store.remove(&subscription.endpoint)
        {
            log::warn!("could not prune a gone push subscription: {e}");
        }
        devices.push(DeviceAnswer {
            host,
            device,
            answer,
        });
    }
    if devices.is_empty() {
        log::debug!("push has no subscribed devices");
    }
    devices
}

/// The `applicationServerKey` a browser needs before it can subscribe: the
/// machine's VAPID public key, uncompressed X9.62, base64url. Answered by
/// `GET /v1/push` (docs/control-api.md).
pub fn public_key(key: &p256::SecretKey) -> String {
    vapid::public_key_param(key)
}
