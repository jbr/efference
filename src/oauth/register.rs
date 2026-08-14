//! Obtaining a `client_id` (docs/mcp-auth.md).
//!
//! Three mechanisms, in the spec's priority order:
//!
//! 1. **Pre-registered** credentials the operator already has for this server.
//! 2. **A Client ID Metadata Document** — the `client_id` *is* an HTTPS URL
//!    the authorization server fetches and validates. Nothing is registered
//!    and nothing is stored, so the identifier is portable across every
//!    authorization server that supports it. Offered iff the server sets
//!    `client_id_metadata_document_supported`.
//! 3. **Dynamic client registration** (RFC 7591) — deprecated by the spec,
//!    and still the only mechanism most deployed servers accept. Offered iff
//!    the server advertises a `registration_endpoint`.
//!
//! ## Why registered credentials are keyed by issuer
//!
//! A `client_id` is issued by, and unique to, one authorization server
//! (RFC 6749 §2.2). Replaying one at a different server is not a degraded
//! request, it is a request with someone else's identity — so [`Registration`]
//! carries the issuer that minted it, and [`Registration::is_for`] is what
//! gates reuse. A protected resource that starts naming a different
//! authorization server needs a fresh registration, not a retry.
//!
//! A Client ID Metadata Document has no such binding, which is exactly its
//! advantage: it is self-hosted and resolved on demand, so one identifier
//! works everywhere with nothing persisted.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use trillium_client::{Client as HttpClient, Url};

/// What this client sends about itself — to a registration endpoint as the
/// RFC 7591 request body, or hosted verbatim as a Client ID Metadata Document.
///
/// The same shape serves both because they describe the same thing; the
/// document adds `client_id` (pointing at its own URL) and the registration
/// request does not.
#[derive(Debug, Clone, Serialize)]
pub struct ClientMetadata {
    /// Shown on the authorization server's consent screen.
    pub client_name: String,
    /// Every loopback redirect this client might use. Exact-match validated
    /// by the authorization server, so the list must cover the whole port
    /// ladder and both host spellings.
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    /// Always `none`: a distributed binary is a public client and has no
    /// secret to authenticate with.
    pub token_endpoint_auth_method: String,
    /// Required rather than defaulted. Under OIDC an omitted `application_type`
    /// means `web`, which rejects the loopback redirect URIs a CLI needs.
    pub application_type: String,
    /// Space-delimited, when the scopes to request are known up front.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl ClientMetadata {
    /// A public native client using the authorization-code grant.
    ///
    /// `refresh_token` is in `grant_types` because a client that wants refresh
    /// tokens should say so — though the authorization server keeps the
    /// discretion not to issue one.
    pub fn native(client_name: &str, redirect_uris: Vec<String>) -> Self {
        Self {
            client_name: client_name.to_string(),
            redirect_uris,
            grant_types: vec!["authorization_code".into(), "refresh_token".into()],
            response_types: vec!["code".into()],
            token_endpoint_auth_method: "none".into(),
            application_type: "native".into(),
            scope: None,
        }
    }

    pub fn with_scope(mut self, scope: Option<String>) -> Self {
        self.scope = scope;
        self
    }

    /// Drop grant types the authorization server does not advertise.
    ///
    /// Asking for `refresh_token` at a server whose `grant_types_supported` is
    /// `["authorization_code"]` is a registration a strict server may reject
    /// outright — and a rejected registration fails the whole dance, to save a
    /// grant type that server was never going to issue anyway. Real servers do
    /// this: OpenRouter's authorization server advertises exactly that.
    ///
    /// A server that advertises nothing gets the full list unchanged; absence
    /// of the field is not a claim about what is unsupported.
    pub fn restricted_to(mut self, supported: Option<&[String]>) -> Self {
        if let Some(supported) = supported {
            self.grant_types
                .retain(|wanted| supported.iter().any(|s| s == wanted));
        }
        self
    }

    /// This metadata as a Client ID Metadata Document to be hosted at
    /// `client_id_url` — the same fields plus a `client_id` naming the
    /// document's own URL, which the authorization server checks matches the
    /// URL it fetched exactly.
    ///
    /// Rendering it here rather than hand-maintaining the hosted JSON keeps
    /// the document and the registration request from drifting apart: the
    /// redirect URIs a server validates against are the ones this binary
    /// actually listens on.
    pub fn as_metadata_document(&self, client_id_url: &str) -> anyhow::Result<String> {
        let mut document = serde_json::to_value(self)?;
        let object = document
            .as_object_mut()
            .expect("client metadata serializes as an object");
        object.insert("client_id".into(), client_id_url.into());
        // `application_type` is a registration-request parameter; it has no
        // meaning in a hosted document.
        object.remove("application_type");
        Ok(serde_json::to_string_pretty(&document)?)
    }
}

/// A `client_id` obtained from a specific authorization server, plus the
/// issuer that minted it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub client_id: String,
    /// Present only if the server issued one despite `token_endpoint_auth_method: none`.
    /// Some servers do; it is stored so it can be sent back, not relied upon.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// The issuer this registration belongs to, and the thing that makes reuse
    /// safe to decide ([`Registration::is_for`]).
    ///
    /// `default` rather than `skip` because this struct is deserialized from
    /// two places: an RFC 7591 registration response, which has no such field
    /// (it is filled in afterwards from the metadata document that led here),
    /// and the credential store, which does. `skip` would silently drop the
    /// binding on the way back in from storage.
    #[serde(default)]
    pub issuer: String,
}

impl Registration {
    /// Whether this registration may be used with `issuer`. The one guard
    /// against sending a client identity to a server that did not issue it.
    pub fn is_for(&self, issuer: &str) -> bool {
        self.issuer == issuer
    }
}

/// RFC 7591 dynamic client registration: POST the metadata, get a `client_id`.
///
/// Deprecated by the spec in favor of Client ID Metadata Documents, and kept
/// because it is what deployed authorization servers actually accept.
pub async fn register_dynamically(
    http: &HttpClient,
    registration_endpoint: &str,
    issuer: &str,
    metadata: &ClientMetadata,
) -> anyhow::Result<Registration> {
    let url = Url::parse(registration_endpoint)
        .with_context(|| format!("registration_endpoint {registration_endpoint} is not a URL"))?;
    let mut conn = http
        .post(url.as_str())
        .with_request_header("content-type", "application/json")
        .with_request_header("accept", "application/json")
        .with_body(serde_json::to_string(metadata)?)
        .await
        .with_context(|| format!("POST {registration_endpoint}"))?;

    let status = conn.status().map_or(0, u16::from);
    let body = conn.response_body().read_string().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        // OIDC servers reject registrations whose redirect URIs conflict with
        // the declared application_type, so say what was rejected rather than
        // just that something was.
        anyhow::bail!(
            "dynamic client registration at {registration_endpoint} returned {status}: {}",
            describe_error(&body)
        );
    }
    let mut registration: Registration = serde_json::from_str(&body)
        .with_context(|| format!("unparseable registration response: {body}"))?;
    registration.issuer = issuer.to_string();
    Ok(registration)
}

/// Render an OAuth error body (RFC 6749 §5.2 / RFC 7591 §3.2.2) as a sentence,
/// falling back to the raw body when it is not one.
pub fn describe_error(body: &str) -> String {
    #[derive(Deserialize)]
    struct OauthError {
        error: String,
        error_description: Option<String>,
    }
    match serde_json::from_str::<OauthError>(body) {
        Ok(OauthError {
            error,
            error_description: Some(description),
        }) => format!("{error} ({description})"),
        Ok(OauthError { error, .. }) => error,
        Err(_) if body.trim().is_empty() => "no response body".to_string(),
        Err(_) => body.to_string(),
    }
}

#[cfg(test)]
mod test;
