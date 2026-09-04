//! Web push (docs/web-push.md): the sender half of "step away from the keyboard".
//!
//! Two events mean the session needs its operator — a pending approval gate and a
//! settled turn — and both happen inside a live daemon, so each daemon sends push for
//! its own events. There is no watcher process and no per-session state: the VAPID
//! keypair is derived from the machine root (`auth.rs`), and the subscriptions the
//! browser produced live in one machine-scoped file every daemon reads.
//!
//! The three pieces here: [`Subscription`] and [`Store`] (what the browser handed over,
//! and where it is kept), `encrypt` (RFC 8291 payload encryption), and `vapid`
//! (RFC 8292 self-identification). [`send`] is the RFC 8030 request that carries them.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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
    /// subscription was created on. `/` today — the session bind's own root; when
    /// a host fronts sessions this carries the session-prefixed path instead, and
    /// only this sender changes (the service worker opens whatever it is named).
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
/// Machine-scoped for the same reason the TOTP burn ledger is user-scoped
/// (`auth.rs`): the VAPID key is derived from the machine root, so every daemon
/// under it can send to every subscription, and a per-repo file would hide a
/// subscription from the session that ought to reach it. Writes are rare —
/// subscribe, unsubscribe, and a prune when a push service reports the endpoint
/// gone — so the whole file is rewritten each time and the last writer wins;
/// there is no lock, and a lost concurrent write costs one resubscribe.
pub struct Store {
    path: PathBuf,
}

/// The file's shape: an object with one key, so a later field can be added
/// without the file changing kind.
#[derive(Serialize, Deserialize, Default)]
struct StoreFile {
    subscriptions: Vec<Subscription>,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The real location: `~/.efference/push.json`, beside the burn ledger. Falls
    /// back to the cwd's session base when the platform reports no home directory
    /// — weaker (a per-repo store) but never silently absent.
    pub fn machine_scoped() -> Self {
        match std::env::home_dir() {
            Some(home) => Self::new(home.join(".efference").join("push.json")),
            None => Self::new(Path::new(".efference").join("push.json")),
        }
    }

    /// Every stored subscription. A missing file is an empty list, not an error:
    /// "nobody has subscribed" is the ordinary state, and every caller would
    /// otherwise have to spell it. An unreadable or unparseable file reads the
    /// same way, with a log line — a push that does not go out must never be able
    /// to fail a turn.
    pub fn load(&self) -> Vec<Subscription> {
        match std::fs::read_to_string(&self.path) {
            Ok(contents) => match serde_json::from_str::<StoreFile>(&contents) {
                Ok(file) => file.subscriptions,
                Err(e) => {
                    log::warn!("push store {} did not parse: {e}", self.path.display());
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                log::warn!("push store {} unreadable: {e}", self.path.display());
                Vec::new()
            }
        }
    }

    fn write(&self, subscriptions: Vec<Subscription>) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = StoreFile { subscriptions };
        std::fs::write(&self.path, serde_json::to_vec_pretty(&file)?)
    }

    /// Record a subscription, replacing any entry with the same endpoint. The
    /// browser mints a new endpoint per subscribe, so this is an upsert rather
    /// than an append mostly to keep a repeated POST idempotent.
    pub fn upsert(&self, subscription: Subscription) -> std::io::Result<()> {
        let mut subscriptions = self.load();
        subscriptions.retain(|s| s.endpoint != subscription.endpoint);
        subscriptions.push(subscription);
        self.write(subscriptions)
    }

    /// Forget one endpoint. Silent when it is not there — an unsubscribe is a
    /// statement about the end state, and a browser may unsubscribe twice.
    pub fn remove(&self, endpoint: &str) -> std::io::Result<()> {
        let mut subscriptions = self.load();
        let before = subscriptions.len();
        subscriptions.retain(|s| s.endpoint != endpoint);
        if subscriptions.len() == before {
            return Ok(());
        }
        self.write(subscriptions)
    }
}

/// What a push service said about one subscription.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Outcome {
    Delivered,
    /// The push service says the subscription no longer exists (404 or 410,
    /// RFC 8030 §7.3). The entry is pruned: it will never deliver again, and the
    /// browser resubscribes on its own next page open.
    Gone,
    /// Anything else — a transient service error, a network failure, a rejected
    /// token. Logged and dropped: a push is a courtesy, and no retry schedule
    /// would make it more timely than the next event.
    Failed,
}

/// Send one message to one subscription (RFC 8030). `now` is a parameter so the
/// token's expiry is not read inside the send.
async fn send_one(
    key: &p256::SecretKey,
    subscription: &Subscription,
    message: &Message,
    now: u64,
) -> Outcome {
    let Some(audience) = vapid::audience(&subscription.endpoint) else {
        log::warn!("push endpoint is not a URL: {}", subscription.endpoint);
        return Outcome::Failed;
    };
    let (Ok(p256dh), Ok(auth)) = (
        B64.decode(&subscription.p256dh),
        B64.decode(&subscription.auth),
    ) else {
        log::warn!("push subscription keys are not base64url: {audience}");
        return Outcome::Failed;
    };
    let Ok(ua_public) = p256::PublicKey::from_sec1_bytes(&p256dh) else {
        log::warn!("push subscription p256dh is not a P-256 point: {audience}");
        return Outcome::Failed;
    };
    let payload = serde_json::to_vec(message).expect("a Message serializes");
    let Ok(body) = encrypt::encrypt(&ua_public, &auth, &payload) else {
        log::warn!("push payload encryption failed for {audience}");
        return Outcome::Failed;
    };

    let result = crate::http::client()
        .post(subscription.endpoint.as_str())
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
        Ok(conn) => match conn.status().map_or(0, u16::from) {
            200..300 => Outcome::Delivered,
            404 | 410 => Outcome::Gone,
            status => {
                log::warn!("push to {audience} returned {status}");
                Outcome::Failed
            }
        },
        Err(e) => {
            log::warn!("push to {audience} failed: {e}");
            Outcome::Failed
        }
    }
}

/// Send one message to every stored subscription, pruning the ones the push
/// service reports gone. Never returns an error: the caller is a spawned task
/// beside the turn loop, and a push that does not go out is not a session
/// failure.
pub async fn send(store: &Store, key: &p256::SecretKey, message: Message) {
    let subscriptions = store.load();
    if subscriptions.is_empty() {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for subscription in subscriptions {
        if send_one(key, &subscription, &message, now).await == Outcome::Gone
            && let Err(e) = store.remove(&subscription.endpoint)
        {
            log::warn!("could not prune a gone push subscription: {e}");
        }
    }
}

/// The `applicationServerKey` a browser needs before it can subscribe: the
/// machine's VAPID public key, uncompressed X9.62, base64url. Answered by
/// `GET /v1/push` (docs/control-api.md).
pub fn public_key(key: &p256::SecretKey) -> String {
    vapid::public_key_param(key)
}
