//! Metadata discovery: RFC 9728 protected-resource metadata, and RFC 8414 /
//! OpenID Connect Discovery authorization-server metadata (docs/mcp-auth.md).
//!
//! Two documents, fetched in sequence. The protected resource (the server that
//! answered `401`) names its authorization servers; each authorization server
//! describes its own endpoints and capabilities. Neither is trusted on
//! arrival — see "validation" below.
//!
//! ## Why there are ladders
//!
//! Both documents have more than one place they might live, and the client is
//! required to try each in a fixed order. For the protected resource, the
//! `WWW-Authenticate` challenge's `resource_metadata` parameter wins if
//! present; otherwise the well-known URI is probed with the server's own path
//! inserted, then at the root. For the authorization server, RFC 8414 and OIDC
//! Discovery disagree about where a path-carrying issuer's metadata goes —
//! RFC 8414 inserts the path after the well-known segment, OIDC appends the
//! well-known segment after the path — so all three spellings get tried.
//!
//! A rung that does not answer `2xx` with parseable JSON is not an error; it
//! is a rung. Only running out of rungs is an error.
//!
//! ## Validation
//!
//! Three checks here are load-bearing and are done at fetch time rather than
//! left to callers, because forgetting any is silent:
//!
//! - **Issuer identity.** The `issuer` in an authorization-server document
//!   must be byte-identical to the issuer identifier the URL was built from
//!   (RFC 8414 §3.3). Without it, a document fetched from
//!   `https://attacker.example/.well-known/oauth-authorization-server` could
//!   claim `"issuer": "https://honest.example"` and redirect the whole dance.
//! - **PKCE support.** A missing `code_challenge_methods_supported` means the
//!   server does not support PKCE, and a public client MUST refuse to proceed
//!   rather than fall back to an unprotected authorization code.
//! - **Transport.** Every URL the dance contacts or hands to the browser —
//!   the issuer, the protected-resource document, and the authorization,
//!   token, and registration endpoints — must be `https`, or `http` on a
//!   loopback host ([`require_transport`]). The authorization code, the PKCE
//!   verifier, and the tokens cross the token endpoint; a document that
//!   names it over plaintext is refused rather than followed.

use anyhow::Context as _;
use serde::Deserialize;
use trillium_client::{Client as HttpClient, Url};

use super::pkce;

/// The well-known suffix for protected-resource metadata (RFC 9728 §3).
const PROTECTED_RESOURCE: &str = "/.well-known/oauth-protected-resource";
/// The well-known suffix for authorization-server metadata (RFC 8414 §3.1).
/// MCP uses the default suffix and defines no application-specific one.
const AUTHORIZATION_SERVER: &str = "/.well-known/oauth-authorization-server";
/// The OpenID Connect Discovery 1.0 equivalent.
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";

/// RFC 9728 protected-resource metadata: what the resource server says about
/// itself. Unknown fields are ignored — the RFC is extensible and servers do
/// extend it.
#[derive(Debug, Clone, Deserialize)]
pub struct ProtectedResource {
    /// The resource identifier. This — not the URL we happened to connect
    /// to — is what goes in the RFC 8707 `resource` parameter, since it is the
    /// audience the authorization server will mint the token for.
    pub resource: String,
    /// The issuer identifiers of the authorization servers that can issue
    /// tokens for this resource. More than one is legal; each is independent,
    /// and credentials are never portable between them.
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    /// The scopes the resource advertises. Intended as the minimal set for
    /// basic functionality, and used only when a challenge carried no `scope`.
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
}

/// RFC 8414 / OIDC Discovery authorization-server metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizationServer {
    /// The issuer identifier. Validated against the URL this was fetched from.
    pub issuer: String,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    /// Present iff the server supports RFC 7591 dynamic client registration.
    pub registration_endpoint: Option<String>,
    /// The PKCE methods offered. Absence means no PKCE (see module docs).
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    #[serde(default)]
    pub grant_types_supported: Option<Vec<String>>,
    /// Whether the server accepts a URL-shaped `client_id` resolved as a
    /// Client ID Metadata Document, which is preferred over dynamic
    /// registration when offered.
    #[serde(default)]
    pub client_id_metadata_document_supported: Option<bool>,
    /// Whether the server puts `iss` on authorization responses (RFC 9207).
    /// Drives whether a *missing* `iss` is fatal; a present one is always
    /// compared.
    #[serde(default)]
    pub authorization_response_iss_parameter_supported: Option<bool>,
}

/// Refuse a URL the dance would contact or open that is not `https` and not
/// `http` on a loopback host (`localhost`, `127.0.0.1`, `[::1]`). `what` names
/// the URL's role in the error, which also states the rule.
pub fn require_transport(url: &Url, what: &str) -> anyhow::Result<()> {
    let allowed = match url.scheme() {
        "https" => true,
        "http" => url.host_str().is_some_and(is_loopback_host),
        _ => false,
    };
    anyhow::ensure!(
        allowed,
        "{what} {url} is refused: OAuth URLs must be https, or http on a loopback host \
         (localhost, 127.0.0.1, [::1])"
    );
    Ok(())
}

/// Whether a URL's host string names the local machine: the `localhost` name
/// or a loopback IP literal (an IPv6 literal arrives bracketed).
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

impl AuthorizationServer {
    /// The authorization endpoint, or an error naming what is missing. Split
    /// out because a metadata document that parses but omits an endpoint is a
    /// real deployment state, not a bug to unwrap through.
    pub fn authorization_endpoint(&self) -> anyhow::Result<&str> {
        self.authorization_endpoint
            .as_deref()
            .with_context(|| format!("{} advertises no authorization_endpoint", self.issuer))
    }

    pub fn token_endpoint(&self) -> anyhow::Result<&str> {
        self.token_endpoint
            .as_deref()
            .with_context(|| format!("{} advertises no token_endpoint", self.issuer))
    }

    /// Apply [`require_transport`] to each endpoint the document names. An
    /// endpoint the document omits is not checked here; the caller that needs
    /// it reports its absence.
    fn require_transport(&self) -> anyhow::Result<()> {
        for (what, endpoint) in [
            ("authorization_endpoint", &self.authorization_endpoint),
            ("token_endpoint", &self.token_endpoint),
            ("registration_endpoint", &self.registration_endpoint),
        ] {
            let Some(endpoint) = endpoint else { continue };
            let url = Url::parse(endpoint)
                .with_context(|| format!("{} names a {what} that is not a URL", self.issuer))?;
            require_transport(&url, what)?;
        }
        Ok(())
    }

    /// Whether this server prefers a Client ID Metadata Document `client_id`.
    pub fn supports_client_id_metadata_document(&self) -> bool {
        self.client_id_metadata_document_supported.unwrap_or(false)
    }

    /// Refuse a server that does not advertise `S256` PKCE support.
    ///
    /// The spec makes this unconditional for both discovery mechanisms — and
    /// explicitly for OIDC too, even though OIDC does not formally define the
    /// field. A public client without PKCE has nothing protecting its
    /// authorization code.
    fn require_pkce(&self) -> anyhow::Result<()> {
        let methods = self
            .code_challenge_methods_supported
            .as_deref()
            .unwrap_or_default();
        anyhow::ensure!(
            !methods.is_empty(),
            "{} does not advertise code_challenge_methods_supported, so it does not support \
             PKCE; refusing to authorize without it",
            self.issuer
        );
        anyhow::ensure!(
            methods.iter().any(|m| m == pkce::S256),
            "{} supports PKCE methods {methods:?} but not {}; refusing to use a weaker method",
            self.issuer,
            pkce::S256
        );
        Ok(())
    }
}

/// An HTTP client for metadata fetches.
///
/// Redirects are followed (discovery endpoints commonly 30x, and
/// trillium-client does not follow them on its own). `FollowRedirects` drops
/// `Authorization` on a cross-origin hop, which matters more here than
/// usual — these fetches chase URLs named by a server that has not been
/// authenticated yet.
pub struct Discovery {
    client: HttpClient,
}

impl Default for Discovery {
    fn default() -> Self {
        Self::new()
    }
}

impl Discovery {
    pub fn new() -> Self {
        Self {
            client: super::http_client(),
        }
    }

    /// The client this discovery is using, so the registration and token steps
    /// share one connection pool and one redirect policy.
    pub fn http(&self) -> &HttpClient {
        &self.client
    }

    /// Fetch protected-resource metadata for `resource_url`.
    ///
    /// `advertised` is the challenge's `resource_metadata` parameter when the
    /// `401` carried one; it takes precedence over probing, per RFC 9728 §5.1.
    pub async fn protected_resource(
        &self,
        resource_url: &Url,
        advertised: Option<&str>,
    ) -> anyhow::Result<ProtectedResource> {
        let mut attempted = Vec::new();
        require_transport(resource_url, "protected resource")?;
        for url in protected_resource_urls(resource_url, advertised) {
            require_transport(&url, "protected resource metadata")?;
            attempted.push(url.to_string());
            if let Some(metadata) = self.fetch::<ProtectedResource>(&url).await {
                return Ok(metadata);
            }
        }
        anyhow::bail!(
            "no protected resource metadata for {resource_url}; tried {}",
            attempted.join(", ")
        )
    }

    /// Fetch and validate authorization-server metadata for `issuer`.
    ///
    /// The returned document has passed the issuer-identity, PKCE, and transport
    /// checks, so callers may use it without re-validating.
    pub async fn authorization_server(&self, issuer: &str) -> anyhow::Result<AuthorizationServer> {
        let issuer_url = Url::parse(issuer)
            .with_context(|| format!("authorization server {issuer} is not a URL"))?;
        require_transport(&issuer_url, "authorization server issuer")?;
        let mut attempted = Vec::new();
        for url in authorization_server_urls(&issuer_url) {
            attempted.push(url.to_string());
            let Some(metadata) = self.fetch::<AuthorizationServer>(&url).await else {
                continue;
            };
            // RFC 8414 §3.3 / OIDC Discovery §4.3: simple string comparison
            // against the issuer the URL was built from. A mismatch is fatal
            // for the whole discovery, not a reason to try the next rung —
            // the document answered, it just answered for someone else.
            anyhow::ensure!(
                metadata.issuer == issuer,
                "metadata at {url} declares issuer {:?}, not {issuer:?}; refusing to use it",
                metadata.issuer
            );
            metadata.require_pkce()?;
            metadata.require_transport()?;
            return Ok(metadata);
        }
        anyhow::bail!(
            "no authorization server metadata for {issuer}; tried {}",
            attempted.join(", ")
        )
    }

    /// One rung. `None` means "did not answer usefully, try the next" — a
    /// transport failure, a non-2xx, or a body that is not the document we
    /// asked for. The distinction between those does not change what the
    /// caller does, so it is logged rather than typed.
    async fn fetch<T: serde::de::DeserializeOwned>(&self, url: &Url) -> Option<T> {
        let mut conn = match self.client.get(url.as_str()).await {
            Ok(conn) => conn,
            Err(e) => {
                log::debug!("metadata GET {url} failed: {e}");
                return None;
            }
        };
        if !conn.status().is_some_and(|status| status.is_success()) {
            log::debug!("metadata GET {url} returned {:?}", conn.status());
            return None;
        }
        let body = match conn.response_body().read_string().await {
            Ok(body) => body,
            Err(e) => {
                log::debug!("metadata GET {url} body unreadable: {e}");
                return None;
            }
        };
        match serde_json::from_str(&body) {
            Ok(metadata) => Some(metadata),
            Err(e) => {
                log::debug!("metadata GET {url} was not the expected document: {e}");
                None
            }
        }
    }
}

/// Where protected-resource metadata might be, in the order to try
/// (RFC 9728 §3.1, and the MCP discovery requirements).
///
/// The advertised URL comes first and is used verbatim. The probes insert the
/// resource's own path after the well-known segment, then fall back to the
/// root — so `https://example.com/public/mcp` looks under
/// `/.well-known/oauth-protected-resource/public/mcp` before
/// `/.well-known/oauth-protected-resource`.
fn protected_resource_urls(resource_url: &Url, advertised: Option<&str>) -> Vec<Url> {
    let mut urls: Vec<Url> = advertised
        .and_then(|u| Url::parse(u).ok())
        .into_iter()
        .collect();
    let path = resource_url.path().trim_end_matches('/');
    if !path.is_empty() {
        urls.push(well_known(
            resource_url,
            &format!("{PROTECTED_RESOURCE}{path}"),
        ));
    }
    urls.push(well_known(resource_url, PROTECTED_RESOURCE));
    urls
}

/// Where authorization-server metadata might be, in the order to try.
///
/// An issuer with a path component gets three spellings, because RFC 8414 and
/// OIDC Discovery place the path on opposite sides of the well-known segment
/// and deployed servers do both. An issuer without one gets two.
fn authorization_server_urls(issuer: &Url) -> Vec<Url> {
    let path = issuer.path().trim_end_matches('/');
    if path.is_empty() {
        vec![
            well_known(issuer, AUTHORIZATION_SERVER),
            well_known(issuer, OPENID_CONFIGURATION),
        ]
    } else {
        vec![
            well_known(issuer, &format!("{AUTHORIZATION_SERVER}{path}")),
            well_known(issuer, &format!("{OPENID_CONFIGURATION}{path}")),
            well_known(issuer, &format!("{path}{OPENID_CONFIGURATION}")),
        ]
    }
}

/// `base` with its path replaced and any query or fragment dropped — a
/// well-known URI is a path on an origin, and carrying the originating URL's
/// query into it would be nonsense.
fn well_known(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    url
}

#[cfg(test)]
mod test;
