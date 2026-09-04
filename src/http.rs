//! The process-wide HTTP client and this harness's `User-Agent` (docs/transport.md).
//!
//! Every outbound request that speaks TCP — upstream inference, provider
//! metadata, MCP over HTTP, the OAuth dance, web push, the dev-proxy — goes
//! through the one [`client`] here. The two Unix-socket callers (the control
//! client in `client.rs`, the host's per-session proxy in `host/proxy.rs`) build
//! their own, because a connector is bound at construction and a UDS connector
//! dials one socket path.
//!
//! Sharing is the point. `trillium_client::Client::new` allocates a fresh
//! connection pool *and* spawns a background reaper task; only **clones** share a
//! pool, which is `Arc`-backed. A per-subsystem client therefore meant a separate
//! TCP+TLS+ALPN handshake to an origin another subsystem already had open — most
//! visibly the turn stream and the generation-cost lookups, which are the same
//! OpenRouter host and could be multiplexed onto one HTTP/2 connection instead.
//!
//! Two properties make one client enough:
//!
//! - **TLS is per-destination, not per-client.** `RustlsConfig` performs the
//!   handshake only for a secure destination and otherwise delegates to the plain
//!   TCP connector, so an `http://` caller (the dev-proxy) shares this client
//!   rather than needing a plaintext one.
//! - **Handlers are per-clone.** `with_handler` *replaces* rather than stacks, so
//!   the shared client installs none and each caller adds its own — the retry and
//!   status handlers in `api/client.rs`, redirect-following in `oauth.rs`. The
//!   clone keeps the pool; only the handler and the (copy-on-write) default
//!   headers differ.
//!
//! Default headers are for what every recipient should see, which is the
//! `User-Agent` and nothing else. Request-scoped identity — credentials, the
//! sticky-routing key, OpenRouter's attribution headers — is set per request by
//! the caller that knows which provider it is talking to, so it cannot reach a
//! host it was not meant for.
//!
//! **Test caveat.** A pooled connection belongs to the reactor it was opened on,
//! so a process-wide client is only safe across `#[tokio::test]` runtimes because
//! the pool is keyed by origin and each test binds a fresh ephemeral port. A test
//! that reused a fixed port across runtimes could be handed a connection whose
//! reactor is gone.

use std::sync::{LazyLock, OnceLock};

use trillium_client::{Client, KnownHeaderName};
use trillium_rustls::RustlsConfig;
use trillium_tokio::ClientConfig;

/// This harness's `User-Agent`: its own product token followed by the transport's,
/// e.g. `efference/0.1.0 trillium-client/0.9.18`. Sent to every host, which is what
/// the header is for — it names the software making the request and nothing about
/// who is making it.
///
/// Returned as `&'static str` rather than a `HeaderValue` so it reads as the
/// string it is at a call site that wants to print or log it, and because
/// `HeaderValue`'s `&'static str` conversion borrows where its `String` one
/// allocates.
pub fn user_agent() -> &'static str {
    static USER_AGENT: LazyLock<String> = LazyLock::new(|| {
        format!(
            "efference/{} {}",
            env!("CARGO_PKG_VERSION"),
            trillium_client::USER_AGENT
        )
    });
    &USER_AGENT
}

/// The shared client, built on first use and cloned out. See the module docs for
/// why one is enough and what a caller may change on its clone.
pub fn client() -> Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::new(RustlsConfig::<ClientConfig>::default())
                .with_default_header(KnownHeaderName::UserAgent, user_agent())
        })
        .clone()
}

#[cfg(test)]
mod test;
