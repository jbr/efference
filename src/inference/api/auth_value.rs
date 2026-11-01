//! The auth header value, held so it can be replaced under a running host
//! (docs/transport.md).
//!
//! An [`Endpoint`] is built once per host connection and cloned per session and
//! role, so a credential copied into it at construction is fixed for the host's
//! life. That is what strands a host whose token expires mid-run: every session
//! holds its own copy of the dead value, and even a freshly created session is
//! born with it. The value therefore lives behind one shared cell that every
//! clone reads through, and a clone shares the cell rather than the string.
//!
//! [`AuthValue::reload`] re-runs whatever produced the value. Not every kind has
//! something to re-run: an `env` credential comes from the environment captured
//! when the host started, which no outside change can reach, so that kind
//! carries no reloader and reports as much. `From<&str>` and `From<String>`
//! build a cell with no reloader, which is what a literal in a test wants.
//!
//! A value whose source states when it expires carries a renewal deadline, and
//! [`AuthValue::current`] re-reads ahead of it rather than waiting for a
//! rejection. That covers every request path alike — a turn, a listing probe,
//! a decisions call — where the rejection-driven re-read is only wired into the
//! turn's retry loop.
//!
//! [`Endpoint`]: super::Endpoint

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, SystemTime};

use super::Provider;
use crate::config::env_table::Masker;

/// Re-reads a credential from wherever it came from, yielding the assembled
/// header value and when to renew it, or why it could not be read.
///
/// Async because re-reading is not always local: `codex-cache` and `keychain`
/// read a file or the OS store, but an OAuth grant's re-read is a *refresh* —
/// an HTTP round-trip to the issuer. A blocking closure doing that inside the
/// runtime would hold a worker thread for the length of a network call.
pub type Reload =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Reread, String>> + Send>> + Send + Sync>;

/// What a [`Reload`] yields: the header value, and the instant after which it
/// should be re-read before use. `None` for a source that states no expiry,
/// which is re-read only when a request is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reread {
    pub value: String,
    pub renew_at: Option<SystemTime>,
}

/// How long a failed renewal waits before the next request tries again. The
/// value it kept may still be accepted until its real expiry, so every request
/// in that window would otherwise pay an issuer round-trip that is failing.
const RENEW_RETRY: Duration = Duration::from_secs(5 * 60);

/// Wrap a synchronous re-read — a file, a store lookup — as a [`Reload`].
///
/// The read still happens on the caller's thread. That is right for the local
/// sources: a keychain read can wait on a biometric prompt, but it is bounded
/// by a person, and moving it would not make it cancellable.
pub fn sync_reload(read: impl Fn() -> Result<String, String> + Send + Sync + 'static) -> Reload {
    Box::new(move || {
        let result = read().map(|value| Reread {
            value,
            renew_at: None,
        });
        Box::pin(async move { result })
    })
}

/// The auth header value shared by every clone of one connection's endpoint.
#[derive(Clone)]
pub struct AuthValue(Arc<Cell>);

struct Cell {
    current: RwLock<String>,
    /// What precedes the secret in `current` (`Bearer `), so the secret can be
    /// masked bare. The first absorb takes the secret bare; a replacement has to
    /// match it, or a refreshed token appearing alone in a tool result is not
    /// caught (docs/secrets.md).
    prefix: &'static str,
    reload: Option<Reload>,
    /// When `current` should be re-read before it is sent, from the source's
    /// own statement of expiry. Moved by every successful re-read, and pushed
    /// back by [`RENEW_RETRY`] on a failed one.
    renew_at: RwLock<Option<SystemTime>>,
    /// The maskers of the sessions using this credential. A replacement has to
    /// join them or the new token is unmasked in every transcript that was
    /// protected from the old one (docs/secrets.md). Weak, because a session's
    /// masker dies with the session while this cell lives as long as the host;
    /// `absorb_secret` never removes, so the old value stays masked too.
    maskers: RwLock<Vec<(Weak<Masker>, String)>>,
    /// Held across a reload so concurrent callers do not each run one. A
    /// reloader may block for a while — a keychain read can wait on a biometric
    /// prompt — so this is the async mutex, not the value's lock, which is
    /// never held across the read.
    reloading: tokio::sync::Mutex<()>,
}

/// What a [`AuthValue::reload`] attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reloaded {
    /// The source yielded a different value, now in place. A request that failed
    /// on the old one is worth sending again.
    Replaced,
    /// The source yielded the same value it had. Nothing has changed upstream,
    /// so retrying would fail identically.
    Unchanged,
    /// There is nothing to re-read: this kind's value cannot change under a
    /// running host.
    NoSource,
    /// The source was there but could not be read, with its own words.
    Failed(String),
}

/// The words that join the upstream error when a re-read did not produce a
/// working credential, so the stall says what was tried.
impl std::fmt::Display for Reloaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Replaced => f.write_str("replaced"),
            Self::Unchanged => f.write_str("the source holds the same credential"),
            Self::NoSource => {
                f.write_str("this credential has no source to re-read under a running host")
            }
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl AuthValue {
    /// A value with a source it can be re-read from.
    pub fn reloadable(provider: Provider, initial: String, reload: Reload) -> Self {
        Self::renewing(provider, initial, None, reload)
    }

    /// A value with a source, renewed from it once `renew_at` passes.
    ///
    /// `provider` names the auth prefix on `initial` and on every re-read value
    /// ([`Provider::auth_header_value`]).
    pub fn renewing(
        provider: Provider,
        initial: String,
        renew_at: Option<SystemTime>,
        reload: Reload,
    ) -> Self {
        Self(Arc::new(Cell {
            current: RwLock::new(initial),
            prefix: provider.auth_prefix(),
            reload: Some(reload),
            renew_at: RwLock::new(renew_at),
            reloading: tokio::sync::Mutex::new(()),
            maskers: RwLock::new(Vec::new()),
        }))
    }

    /// Mask replacements of this credential in `masker` too, under `name`.
    ///
    /// The initial value is absorbed by the caller as before; this is only about
    /// what a re-read produces later, which no session has seen at boot.
    pub fn mask_replacements_in(&self, name: &str, masker: &Arc<Masker>) {
        let mut registered = self.0.maskers.write().unwrap();
        registered.retain(|(weak, _)| weak.strong_count() > 0);
        registered.push((Arc::downgrade(masker), name.to_string()));
    }

    /// Register a replacement everywhere the old value was masked. `value` is a
    /// header value; the secret within it is what gets absorbed, matching the
    /// bare secret the first absorb took.
    fn mask(&self, value: &str) {
        let secret = value.strip_prefix(self.0.prefix).unwrap_or(value);
        let mut registered = self.0.maskers.write().unwrap();
        registered.retain(|(weak, _)| weak.strong_count() > 0);
        for (weak, name) in registered.iter() {
            if let Some(masker) = weak.upgrade() {
                masker.absorb_secret(&[(name.clone(), secret.to_string())]);
            }
        }
    }

    /// The value as it stands, without renewing it. A request should send
    /// [`current`](Self::current) instead; this is for comparison and tests.
    pub fn get(&self) -> String {
        self.0.current.read().unwrap().clone()
    }

    /// When this value is next due for renewal, if its source states one.
    pub fn renew_at(&self) -> Option<SystemTime> {
        *self.0.renew_at.read().unwrap()
    }

    fn due(&self) -> bool {
        self.renew_at().is_some_and(|at| at <= SystemTime::now())
    }

    /// The value to put on a request, renewed first if it is due.
    ///
    /// A failed renewal keeps the old value, which may still be accepted until
    /// its real expiry; the failure is logged, and if the provider does reject
    /// it, the rejection path's re-read reports the source's words.
    pub async fn current(&self) -> String {
        if self.due() {
            let _guard = self.0.reloading.lock().await;
            // Another caller may have renewed while this one waited.
            if self.due()
                && let Some(reload) = &self.0.reload
            {
                match reload().await {
                    Ok(reread) => self.replace(reread),
                    Err(why) => {
                        log::warn!("renewing a credential ahead of its expiry failed: {why}");
                        *self.0.renew_at.write().unwrap() = Some(SystemTime::now() + RENEW_RETRY);
                    }
                }
            }
        }
        self.get()
    }

    /// Renew this value whenever it comes due, for as long as it states a
    /// deadline — so a credential nobody is using stays renewed, and a grant
    /// whose refresh token has gone bad is reported at the host rather than
    /// at the next request. A value already due is renewed at once, which is
    /// what tests a stored refresh token at host launch.
    ///
    /// Waits in bounded steps rather than one sleep to the deadline: a tokio
    /// timer runs on the monotonic clock, which a sleeping laptop does not
    /// advance, so one long sleep would wake late by however long the lid was
    /// shut.
    pub async fn keep_renewed(self) {
        const STEP: Duration = Duration::from_secs(15 * 60);
        while let Some(at) = self.renew_at() {
            match at.duration_since(SystemTime::now()) {
                Ok(wait) => tokio::time::sleep(wait.min(STEP)).await,
                Err(_) => {
                    self.current().await;
                }
            }
        }
    }

    /// Put a re-read value in place. Masked before it is reachable: nothing may
    /// read the new value off the cell until it is redactable.
    fn replace(&self, reread: Reread) {
        if reread.value != self.get() {
            self.mask(&reread.value);
            *self.0.current.write().unwrap() = reread.value;
        }
        *self.0.renew_at.write().unwrap() = reread.renew_at;
    }

    /// Re-read the credential, replacing the value when it differs from
    /// `rejected` — the value whose rejection prompted this.
    ///
    /// Taking what the caller sent, rather than re-reading the current value, is
    /// what makes concurrent failures cost one read: when several sessions fail
    /// on the same token, the first replaces it and the rest find the value
    /// already moved past what they sent and retry on it without re-reading. It
    /// also removes the window where a caller re-reads a value that was already
    /// repaired between its request and its reload.
    pub async fn reload(&self, rejected: &str) -> Reloaded {
        let _guard = self.0.reloading.lock().await;
        if self.get() != rejected {
            return Reloaded::Replaced;
        }
        let Some(reload) = &self.0.reload else {
            return Reloaded::NoSource;
        };
        match reload().await {
            Err(why) => Reloaded::Failed(why),
            Ok(reread) if reread.value == rejected => Reloaded::Unchanged,
            Ok(reread) => {
                self.replace(reread);
                Reloaded::Replaced
            }
        }
    }
}

impl From<String> for AuthValue {
    fn from(value: String) -> Self {
        Self(Arc::new(Cell {
            current: RwLock::new(value),
            prefix: "",
            reload: None,
            renew_at: RwLock::new(None),
            reloading: tokio::sync::Mutex::new(()),
            maskers: RwLock::new(Vec::new()),
        }))
    }
}

impl From<&str> for AuthValue {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

/// Compares the value, so an endpoint's auth can be asserted against a literal.
impl PartialEq<&str> for AuthValue {
    fn eq(&self, other: &&str) -> bool {
        self.get() == *other
    }
}

/// Never the value itself: an endpoint is debug-printed in logs and errors.
impl std::fmt::Debug for AuthValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[cfg(test)]
mod test;
