//! The TCP bind: the web client's front door (docs/web-bind.md).
//!
//! An optional TCP listener beside the UDS — the same `/v1` routes, with a hardening +
//! auth gate layered in front. This module holds the listener and the credential state;
//! its submodules hold the rest: `gate.rs` (the hardening + auth gate and the anonymous
//! surface it prices), `headers.rs` (the response headers every listener stamps), and
//! `routes.rs` (the sign-in routes, the enrollment routes, and the web client handler).
//! `handler.rs` is the one place the pieces are assembled into a stack, keyed on the
//! transport, so the UDS never consults the gate and a per-conn "which listener am I
//! on?" guess (which would fail open) never exists. Every TCP credential is an enrolled
//! device (`crate::devices`) whose token is derived from the machine root
//! (`crate::auth`), and a browser becomes one by claiming a pending sign-in
//! (`crate::sign_ins`); the enrollment surfaces (`/auth/link`, approval, the
//! `/auth/devices` routes) are mounted only on the socket, because possession of the
//! host machine is the enrollment authority.

use super::*;
use crate::auth::{DeviceTokens, Root};
use crate::devices::{self, Enrolled, Joined, Kind};
use crate::sign_ins;
use std::time::SystemTime;
use trillium::KnownHeaderName;

mod gate;
mod headers;
mod routes;

pub(crate) use gate::{Surface, gate, plain_id};
pub(crate) use headers::SecurityHeaders;
pub(crate) use routes::{enrollment_routes, frontend, pair_routes};

/// What `--bind` resolved to — built by the CLI from flags, consumed
/// by [`arm`]. Deliberately not part of the session config: the config file is
/// model-proposable and fs-watched live, and network exposure must not be requestable
/// from inside the session (docs/web-bind.md).
pub struct TcpBind {
    /// The listen address (`host:port`). Loopback may be plaintext; anything else
    /// requires `tls` — the CLI enforces this before constructing one of these.
    pub addr: String,
    /// PEM cert chain + key (the `tailscale cert` output). `None` = plaintext loopback.
    pub tls: Option<TlsPem>,
    /// The address browsers open, `scheme://name[:port]` with no path: the base of
    /// the printed sign-in link, and — by its host — the Host allowlist entry. It
    /// differs from the listener when something in front terminates TLS (a
    /// `tailscale serve`), which is why it is given rather than derived.
    pub public_url: String,
    /// The machine root every credential derives from.
    pub root: Root,
    /// Launch the operator's browser at the sign-in link once bound.
    pub open: bool,
}

/// A PEM cert chain and private key, read by the CLI so a bad path fails at flag
/// resolution rather than mid-boot.
pub struct TlsPem {
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
}

/// The derived credential state the gate and the auth routes share. One per daemon,
/// built at boot from the injected root; holds no secret that can't be re-derived.
pub struct TcpAuth {
    /// The key device tokens are minted and verified under.
    tokens: DeviceTokens,
    /// Host values (no port, lowercase) a request may address the daemon by — the
    /// DNS-rebinding check. The configured public name, plus the loopback spellings
    /// on a plaintext dev bind.
    hosts: Vec<String>,
    /// `scheme://host:port` for minted links. Behind a lock because a bind to
    /// port 0 learns its port only once listening ([`TcpAuth::bound_port`]).
    public_base: Mutex<String>,
    /// Whether the listener terminates TLS ([`TcpAuth::is_tls`]).
    tls: bool,
    /// The enrolled devices, one row per TCP credential — user-scoped, so a device
    /// revoked anywhere is refused here on its next request.
    devices: devices::Store,
    /// The sign-ins pending here — user-scoped, so the socket of any daemon under the
    /// user approves what this listener took.
    sign_ins: sign_ins::Store,
}

/// The listener half of a [`TcpBind`], separated from the credential half by [`arm`]
/// so the UDS router can hold the [`TcpAuth`] before the TCP server spawns.
pub(crate) struct Listener {
    pub(crate) addr: String,
    pub(crate) tls: Option<TlsPem>,
    pub(crate) open: bool,
}

/// Split a bind into its credential state and its listener parameters.
pub(crate) fn arm(bind: TcpBind) -> (Arc<TcpAuth>, Listener) {
    arm_with_state(bind, crate::state::user_scoped())
}

/// [`arm`] with the user state database injected — the interface a test uses to keep
/// enrolled devices and pending sign-ins out of the real user-scoped file. Both tables
/// the bind keeps are in that one file (docs/state-store.md).
pub(crate) fn arm_with_state(bind: TcpBind, state: PathBuf) -> (Arc<TcpAuth>, Listener) {
    let TcpBind {
        addr,
        tls,
        public_url,
        root,
        open,
    } = bind;
    let public_base = public_url.trim_end_matches('/').to_string();
    let authority = public_base
        .split_once("://")
        .map_or(public_base.as_str(), |(_, rest)| rest);
    let mut hosts = vec![host_only(authority).to_ascii_lowercase()];
    if tls.is_none() {
        // A plaintext bind is loopback-only (CLI-enforced); accept its spellings.
        for loopback in ["localhost", "127.0.0.1", "[::1]"] {
            if !hosts.iter().any(|h| h == loopback) {
                hosts.push(loopback.to_string());
            }
        }
    }
    let auth = Arc::new(TcpAuth {
        tokens: root.device_tokens(),
        hosts,
        public_base: Mutex::new(public_base),
        tls: tls.is_some(),
        devices: devices::Store::new(state.clone()),
        sign_ins: sign_ins::Store::new(state),
    });
    (auth, Listener { addr, tls, open })
}

/// Register a [`Listener`]'s address on `config`: plaintext, or TLS paired with a QUIC
/// listener on the same port. One function because a host listener adopts the same
/// shape and a second copy of the pairing would drift from this one.
///
/// Generic over the server type only because `trillium-tokio` does not export the one
/// it uses; the bounds say what the body needs of it.
pub(crate) fn bind<S>(
    listener: &Listener,
    config: trillium_tokio::ListenerConfig<S>,
) -> io::Result<trillium_tokio::ListenerConfig<S>>
where
    S: trillium_server_common::Server,
    trillium_rustls::RustlsAcceptor: trillium_server_common::Acceptor<S::Transport>,
    trillium_quinn::QuicConfig: trillium_server_common::QuicConfig<S>,
{
    match &listener.tls {
        // TLS also serves HTTP/3: a QUIC listener on the same port (UDP), which the
        // builder auto-pairs with the TLS listener and advertises via `alt-svc:
        // h3` — browsers upgrade on their next request. The gate is
        // protocol-agnostic (the authority read handles h2/h3's pseudo-header).
        Some(pem) => config
            .bind_tls(
                listener.addr.as_str(),
                trillium_rustls::RustlsAcceptor::from_single_cert(&pem.cert, &pem.key),
            )?
            .bind_quic(
                listener.addr.as_str(),
                trillium_quinn::QuicConfig::from_single_cert(&pem.cert, &pem.key),
            ),
        None => config.bind_tcp(listener.addr.as_str()),
    }
}

/// Spawn a TCP listener with `handler`, sharing the UDS server's swansong (one shutdown
/// domain) and its server-global state — a session bind's `Arc<Session>`, a host's
/// `Arc<Host>`. `without_signals` because the UDS server already registered the
/// signal handlers.
pub(crate) fn spawn(
    listener: &Listener,
    handler: impl trillium::Handler,
    swansong: trillium_tokio::Swansong,
    shared: impl Send + Sync + 'static,
) -> io::Result<trillium_server_common::ServerHandle> {
    let config = trillium_tokio::config()
        .with_swansong(swansong)
        .with_shared_state(shared)
        .without_signals()
        .listeners();
    Ok(bind(listener, config)?.spawn(handler))
}

impl TcpAuth {
    pub(crate) fn public_base(&self) -> String {
        self.public_base
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether the listener this authenticator fronts terminates TLS — false
    /// for the plaintext loopback dev bind (`arm`'s `tls: None`). HSTS rides
    /// only on a TLS listener (docs/web-bind.md).
    pub(crate) fn is_tls(&self) -> bool {
        self.tls
    }

    /// Record the port the listener actually bound, for a bind that asked for
    /// port 0: minted links then name a port a browser can reach. A base that
    /// names a port of its own is left alone — it is the configured public URL.
    pub(crate) fn bound_port(&self, port: u16) {
        let mut base = self
            .public_base
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(unbound) = base.strip_suffix(":0") {
            *base = format!("{unbound}:{port}");
        }
    }

    /// The sign-in link: a pending sign-in approved before it was asked for, its claim
    /// secret in the URL *fragment*, which never reaches server or proxy logs. The
    /// secret collects one token within the sign-in's lifetime, so the copy left in
    /// scrollback and browser history collects nothing after (docs/web-bind.md).
    pub(crate) fn mint_link(&self) -> Result<String, crate::state::Error> {
        let claim = self.sign_ins.approved_link(SystemTime::now())?;
        Ok(format!("{}/#claim={}", self.public_base(), claim.encoded()))
    }

    /// Enroll a device and answer it with its token — what a claim hands a browser
    /// whose sign-in was approved, and what the socket's `POST /auth/devices` hands a
    /// script (docs/web-bind.md). The one moment a token is spelled outside a request.
    pub(crate) fn enroll(
        &self,
        kind: Kind,
        label: &str,
        joined: Joined,
    ) -> Result<Enrolled, crate::state::Error> {
        let device = self
            .devices
            .insert(kind, label, joined, SystemTime::now())?;
        let token = self.tokens.token(&device.id);
        Ok(Enrolled { device, token })
    }

    fn host_allowed(&self, host: &str) -> bool {
        let host = host_only(host).to_ascii_lowercase();
        self.hosts.contains(&host)
    }

    /// The device token in the conn's `Authorization` header — the only place a
    /// credential is read from. Nothing a browser attaches on its own (a cookie, a
    /// URL) is consulted, so a request another site forces carries none. The tag is
    /// verified before the row is looked up, so only a token this root minted
    /// reaches the table (docs/web-bind.md).
    fn credential(&self, conn: &Conn) -> Credential {
        let Some(id) = conn
            .request_headers()
            .get_str(KnownHeaderName::Authorization)
            .and_then(|authorization| authorization.strip_prefix("Bearer "))
            .and_then(|token| self.tokens.parse(token.trim()))
        else {
            return Credential::Refused;
        };
        match self.devices.seen(&id, SystemTime::now()) {
            Ok(true) => Credential::Enrolled,
            Ok(false) => Credential::Refused,
            Err(e) => {
                log::error!("could not read the enrolled devices; refusing the request: {e}");
                Credential::Unchecked
            }
        }
    }
}

/// What the credential check found on a conn ([`TcpAuth::credential`]).
enum Credential {
    /// A device token whose tag verifies, naming a row that exists.
    Enrolled,
    /// No token, a malformed or forged one, or one whose device was revoked.
    Refused,
    /// The token verified, but the device table could not be read. The request is
    /// refused without a `401`, which a client takes as a bad token and discards
    /// (docs/web-bind.md).
    Unchecked,
}

/// The host part of an authority-shaped value: strip a port and a trailing FQDN dot,
/// keep IPv6 brackets whole.
fn host_only(value: &str) -> &str {
    if value.starts_with('[') {
        match value.find(']') {
            Some(end) => &value[..=end],
            None => value,
        }
    } else {
        let host = value.split(':').next().unwrap_or(value);
        host.strip_suffix('.').unwrap_or(host)
    }
}

/// The host part of an `Origin` header (`scheme://host[:port]`).
fn origin_host(origin: &str) -> &str {
    let rest = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    host_only(rest)
}

#[cfg(test)]
mod test;
