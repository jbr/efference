//! A generic OAuth 2.1 public client (docs/mcp-auth.md).
//!
//! Nothing in this module knows about MCP. That is the point: the MCP
//! authorization spec is "OAuth 2.1 with a selected subset of features", and
//! the subset is all standards-track — RFC 9728 protected-resource metadata,
//! RFC 8414 / OIDC authorization-server metadata, RFC 7636 PKCE, RFC 7591
//! dynamic registration, RFC 8707 resource indicators, RFC 9207 issuer
//! identification. What is MCP-specific is *when* the dance runs and what the
//! resource indicator points at, and that lives in `crate::mcp::auth`.
//!
//! The seam is drawn so this half can be lifted into a standalone crate once a
//! second consumer exists; the eventual home is a `trillium-oauth-client`
//! alongside the rest of the ecosystem. Keep MCP types out of it.
//!
//! ## Public client
//!
//! efference is a *public* OAuth client: it is a distributed binary, so any
//! secret it shipped would be in every copy on every machine and would not be
//! a secret. So `token_endpoint_auth_method` is `none`, there is no client
//! authentication at the token endpoint, and PKCE carries the entire
//! per-authorization security burden — which is why [`pkce`] refuses to
//! proceed against a server that does not advertise `S256` support rather
//! than falling back to anything weaker.

use trillium_client::Client as HttpClient;
use trillium_redirect::client::FollowRedirects;
use trillium_rustls::RustlsConfig;
use trillium_tokio::ClientConfig;

/// The HTTP client every step of the dance uses.
///
/// Redirects are followed because discovery endpoints commonly `30x` and
/// trillium-client does not follow them itself. `FollowRedirects` drops
/// `Authorization` and `Cookie` on a cross-origin hop, which matters more here
/// than usual: these requests chase URLs named by a server that has not been
/// authenticated yet, and a bearer token following one of them to an
/// attacker-chosen origin is the leak the spec's threat model is about.
pub fn http_client() -> HttpClient {
    HttpClient::new(RustlsConfig::<ClientConfig>::default()).with_handler(FollowRedirects::new())
}

pub mod authorize;
pub mod loopback;
pub mod metadata;
pub mod pkce;
pub mod register;
pub mod token;
