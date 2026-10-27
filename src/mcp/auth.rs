//! The MCP half of the authorization dance (docs/mcp-auth.md).
//!
//! Thin by design. Everything standards-track lives in [`crate::oauth`]; what
//! is here is what MCP decides: efference's own client identity, the sequence
//! that turns a `401` into a token, and the rule that the resource indicator
//! names the MCP server.
//!
//! ## The sequence
//!
//! 1. The `401`'s `WWW-Authenticate` challenge names where the protected
//!    resource metadata lives (or it is probed).
//! 2. That document names the authorization servers.
//! 3. The authorization server's own metadata gives the endpoints and the
//!    capabilities, and is validated on arrival.
//! 4. A `client_id` is obtained by the first mechanism that applies.
//! 5. A loopback listener binds *before* the authorization URL is built,
//!    because the port is part of the URL.
//! 6. The operator authorizes in a browser; the response is validated against
//!    the recorded issuer, state, and PKCE verifier.
//! 7. The code is exchanged for a token.
//!
//! ## What the operator does, and what this module does
//!
//! Step 6 needs a human and a browser, and how to reach one differs by
//! context — a terminal can open a browser, a log-captured boot can only print
//! a URL. So this module does not decide: it hands the URL to a `present`
//! callback and waits. That keeps the sequence testable and puts the
//! presentation where it belongs, at the call site.

mod credentials;
pub mod store;

pub use credentials::{Credentials, stored_credentials};

use std::sync::Arc;

use anyhow::Context as _;
use trillium_client::Url;

use crate::oauth::authorize::Pending;
use crate::oauth::loopback;
use crate::oauth::metadata::{AuthorizationServer, Discovery, ProtectedResource};
use crate::oauth::register::{self, ClientMetadata, Registration};
use crate::oauth::token::{self, CodeExchange, Tokens};
use crate::www_authenticate::Challenge;

/// A `401` from a connected provider: the signal that starts the dance.
///
/// A typed error rather than a message because the boot step has to *act* on
/// it — recognize it among all the other ways a mount fails, and read the
/// challenge out of it — and matching on error text is how that quietly stops
/// working.
///
/// A transport holding stored credentials renews them before giving up
/// ([`Credentials::renew`]), so from one of those this also means the renewal
/// failed. A mount runs the dance on it whichever it means; a call fails with
/// it as its result, which after a failed renewal says a session restart signs
/// in again.
#[derive(Debug, Clone)]
pub struct AuthorizationRequired {
    /// The `Bearer` challenge, when the server sent a parseable one. It carries
    /// where the protected-resource metadata lives and which scopes are wanted;
    /// both have fallbacks, so its absence is survivable (Atlassian sends a
    /// challenge with no `resource_metadata` at all).
    pub challenge: Option<Challenge>,
    /// Why the transport's stored credentials could not be renewed, when it held
    /// some. `None` from a transport that was mounted without any.
    pub renewal_failed: Option<String>,
}

impl std::fmt::Display for AuthorizationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.renewal_failed {
            None => f.write_str("the server requires authorization (401)"),
            Some(why) => write!(
                f,
                "its sign-in expired and could not be renewed ({why}); a session restart \
                 signs in again"
            ),
        }
    }
}

impl std::error::Error for AuthorizationRequired {}

/// The name an authorization server shows on its consent screen.
pub const CLIENT_NAME: &str = "efference";

/// efference's Client ID Metadata Document. The `client_id` *is* this URL: an
/// authorization server that supports the mechanism fetches it and checks that
/// the document's own `client_id` field matches byte-for-byte.
///
/// It identifies the *software*, not this installation — every efference on
/// every machine sends the same value. There is no per-install secret behind
/// it; PKCE and the loopback redirect are what make one authorization distinct
/// from another.
///
/// The path is versioned because this URL is a permanent identity: changing
/// the document changes behavior for every already-shipped binary pointing at
/// it, so a change that cannot be made compatibly needs a new path rather than
/// an edit. It must be served directly, with no redirect — the specification
/// does not say whether a server compares against the URL it requested or the
/// one it landed on.
pub const CLIENT_ID_METADATA_DOCUMENT: &str = "https://efference.net/oauth/client-v1.json";

/// The loopback ports, in order of preference (docs/mcp-auth.md). Fixed rather
/// than ephemeral because redirect URIs are matched exactly; the ladder exists
/// so one occupied port is not a dead end.
pub const REDIRECT_PORTS: [u16; 3] = [50505, 50506, 50507];

/// efference's client metadata: what a registration request sends, and what
/// the hosted metadata document contains.
///
/// One function for both so they cannot drift — the redirect URIs an
/// authorization server validates against are the ones this binary listens on.
pub fn client_metadata(scope: Option<String>) -> ClientMetadata {
    ClientMetadata::native(CLIENT_NAME, loopback::all_redirect_uris(&REDIRECT_PORTS))
        .with_scope(scope)
}

/// A completed dance.
pub struct Authorization {
    pub tokens: Tokens,
    /// The authorization server that issued this grant, and the `client_id`
    /// presented to it — both needed again on every refresh.
    pub issuer: String,
    pub client_id: String,
    /// A dynamic registration worth persisting, if one was performed. Keyed by
    /// its issuer; never reusable at a different authorization server.
    pub registration: Option<Registration>,
    /// The resource indicator the token was minted for — needed again on every
    /// refresh.
    pub resource: String,
    /// The token endpoint, likewise needed for refresh.
    pub token_endpoint: String,
}

/// Run the dance for one MCP server.
///
/// `challenge` is the `Bearer` challenge from the `401`, when there was one;
/// it carries both the metadata location and the scopes the server wants.
/// `existing` is a stored registration to reuse, which is honored only if it
/// belongs to the authorization server discovery actually lands on.
pub async fn authorize<P>(
    server_url: &str,
    challenge: Option<&Challenge>,
    existing: Option<&Registration>,
    present: P,
) -> anyhow::Result<Authorization>
where
    P: FnOnce(&Url) -> anyhow::Result<()>,
{
    let url = Url::parse(server_url)
        .with_context(|| format!("MCP server url {server_url} is not a URL"))?;
    let discovery = Discovery::new();

    let resource_metadata = challenge.and_then(|c| c.param("resource_metadata"));
    let protected = discovery
        .protected_resource(&url, resource_metadata)
        .await?;
    check_resource(&url, &protected)?;

    let (server, issuer) = select_authorization_server(&discovery, &protected).await?;
    let scope = select_scope(challenge, &protected);

    // Bind before building the URL: the port is in the redirect URI, and the
    // redirect URI is in the authorization request.
    let listener = loopback::listen(&REDIRECT_PORTS).await?;
    let redirect_uri = listener.redirect_uri();

    let registration = client_id_for(&discovery, &server, &issuer, existing, scope.clone()).await?;
    let client_id = registration.client_id().to_string();

    let (pending, authorization_url) = Pending::new(
        &server,
        &client_id,
        &redirect_uri,
        &protected.resource,
        scope,
    )?;
    present(&authorization_url)?;

    let authorized = pending.validate(&listener.wait().await?)?;
    let token_endpoint = server.token_endpoint()?.to_string();
    let tokens = token::exchange_code(
        discovery.http(),
        &token_endpoint,
        CodeExchange {
            code: &authorized.code,
            redirect_uri: &authorized.redirect_uri,
            client_id: &client_id,
            code_verifier: &authorized.code_verifier,
            resource: Some(&protected.resource),
        },
    )
    .await?;

    Ok(Authorization {
        tokens,
        issuer,
        client_id,
        registration: registration.into_persistable(),
        resource: protected.resource,
        token_endpoint,
    })
}

/// A client identity for one authorization server, and whether it is worth
/// keeping.
enum ClientId {
    /// The self-hosted metadata document URL. Portable across authorization
    /// servers, so there is nothing to persist.
    MetadataDocument,
    /// Issued by, and only valid at, one authorization server.
    Registered(Registration),
}

impl ClientId {
    fn client_id(&self) -> &str {
        match self {
            Self::MetadataDocument => CLIENT_ID_METADATA_DOCUMENT,
            Self::Registered(registration) => &registration.client_id,
        }
    }

    fn into_persistable(self) -> Option<Registration> {
        match self {
            Self::MetadataDocument => None,
            Self::Registered(registration) => Some(registration),
        }
    }
}

/// The spec's client-registration priority order.
///
/// A stored registration is reused only when it belongs to *this* issuer: a
/// `client_id` is unique to the server that issued it, so replaying one
/// elsewhere is a request under another server's identity, not a degraded
/// request. A protected resource that starts naming a different authorization
/// server therefore re-registers rather than reusing.
async fn client_id_for(
    discovery: &Discovery,
    server: &AuthorizationServer,
    issuer: &str,
    existing: Option<&Registration>,
    scope: Option<String>,
) -> anyhow::Result<ClientId> {
    if let Some(registration) = existing.filter(|r| r.is_for(issuer)) {
        log::debug!("reusing the stored registration for {issuer}");
        return Ok(ClientId::Registered(registration.clone()));
    }
    if server.supports_client_id_metadata_document() {
        log::debug!("{issuer} supports client ID metadata documents");
        return Ok(ClientId::MetadataDocument);
    }
    if let Some(endpoint) = &server.registration_endpoint {
        log::debug!("registering dynamically with {issuer}");
        // Ask only for grant types this server advertises: a registration
        // rejected for requesting `refresh_token` fails the whole dance, to
        // gain a grant the server was never going to issue.
        let metadata =
            client_metadata(scope).restricted_to(server.grant_types_supported.as_deref());
        return register::register_dynamically(discovery.http(), endpoint, issuer, &metadata)
            .await
            .map(ClientId::Registered);
    }
    anyhow::bail!(
        "{issuer} supports neither client ID metadata documents nor dynamic client \
         registration, so efference cannot obtain a client_id for it; it needs a \
         pre-registered client, which is not configurable yet"
    )
}

/// Pick an authorization server from the protected resource's list.
///
/// More than one is legal and each is independent. The first whose metadata
/// discovers *and validates* wins; a later one is tried only when an earlier
/// one could not be used at all, and the error from the last attempt is what
/// surfaces.
async fn select_authorization_server(
    discovery: &Discovery,
    protected: &ProtectedResource,
) -> anyhow::Result<(AuthorizationServer, String)> {
    anyhow::ensure!(
        !protected.authorization_servers.is_empty(),
        "the protected resource metadata for {} lists no authorization_servers",
        protected.resource
    );
    let mut last_error = None;
    for issuer in &protected.authorization_servers {
        match discovery.authorization_server(issuer).await {
            Ok(server) => return Ok((server, issuer.clone())),
            Err(e) => {
                log::debug!("authorization server {issuer} is unusable: {e}");
                last_error = Some(e);
            }
        }
    }
    Err(last_error.expect("the list was checked non-empty"))
}

/// The scopes to ask for, in the spec's priority order: the challenge's
/// `scope` if the server sent one — it is authoritative for the operation that
/// failed — otherwise everything the resource advertises, otherwise nothing.
fn select_scope(challenge: Option<&Challenge>, protected: &ProtectedResource) -> Option<String> {
    if let Some(scope) = challenge.and_then(|c| c.param("scope")) {
        return Some(scope.to_string());
    }
    protected
        .scopes_supported
        .as_ref()
        .filter(|scopes| !scopes.is_empty())
        .map(|scopes| scopes.join(" "))
}

/// The resource identifier must name the server actually being talked to.
///
/// RFC 9728 §3.3 requires the client to check this. Comparing origins rather
/// than whole URLs is deliberate: a resource may legitimately identify itself
/// more coarsely than the endpoint path it is reached at
/// (`https://mcp.example.com` for an endpoint at `https://mcp.example.com/mcp`),
/// but a *different origin* means the document is describing something else.
fn check_resource(url: &Url, protected: &ProtectedResource) -> anyhow::Result<()> {
    let resource = Url::parse(&protected.resource).with_context(|| {
        format!(
            "protected resource metadata declares resource {:?}, which is not a URL",
            protected.resource
        )
    })?;
    anyhow::ensure!(
        resource.origin() == url.origin(),
        "protected resource metadata for {url} declares resource {:?}, a different origin; \
         refusing to request a token for it",
        protected.resource
    );
    Ok(())
}

#[cfg(test)]
mod test;

/// Authorize `server` from scratch and persist the result, returning the
/// credentials to retry the mount with.
///
/// Any stored registration is offered to [`authorize`], which uses it only if
/// it belongs to the authorization server discovery lands on. That is what
/// keeps repeated runs from littering the authorization server with a fresh
/// anonymous client record each time — dynamic registration is not idempotent.
pub async fn authorize_and_store<P>(
    store: &Arc<dyn store::TokenStore>,
    server: &str,
    url: &str,
    challenge: Option<&Challenge>,
    present: P,
) -> anyhow::Result<Credentials>
where
    P: FnOnce(&Url) -> anyhow::Result<()>,
{
    let previous = store.load(server)?;
    let registration = previous.as_ref().and_then(|p| p.registration.as_ref());
    let authorization = authorize(url, challenge, registration, present).await?;

    let credentials = Credentials::new(store, server, authorization.tokens.clone());
    store.save(
        server,
        &store::StoredAuth {
            issuer: authorization.issuer,
            resource: authorization.resource,
            token_endpoint: authorization.token_endpoint,
            client_id: authorization.client_id,
            tokens: authorization.tokens,
            // A reused registration is not returned by `authorize` (only a
            // freshly performed one is), so fall back to what was already
            // stored rather than dropping it.
            registration: authorization
                .registration
                .or_else(|| previous.and_then(|p| p.registration)),
        },
    )?;
    Ok(credentials)
}
