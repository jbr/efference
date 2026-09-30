//! The token endpoint: exchanging an authorization code, and refreshing
//! (docs/mcp-auth.md).
//!
//! Both requests are `application/x-www-form-urlencoded` POSTs, and both carry
//! the RFC 8707 `resource` parameter naming the MCP server the token is for.
//! That parameter is sent **unconditionally** — the spec requires it whether
//! or not the authorization server claims to understand it, because a server
//! that does understand it binds the token's audience, and a server that does
//! not ignores an unknown parameter.
//!
//! No client authentication is sent: this is a public client, so `client_id`
//! identifies the request and PKCE's `code_verifier` is what proves the
//! requester is the one who started the authorization.

use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use trillium_client::Client as HttpClient;
use trillium_client::url::form_urlencoded;

use super::register::describe_error;

/// Refresh this long before the server's stated expiry, so a token does not
/// expire in flight between the check and the request that uses it.
const EXPIRY_SKEW: Duration = Duration::from_secs(30);

/// A token grant, with `expires_in` resolved to a wall-clock instant at the
/// moment the response arrived.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    access_token: String,
    /// Practically always `Bearer`; kept as sent so the `Authorization` header
    /// can be built from what the server actually said.
    token_type: String,
    refresh_token: Option<String>,
    /// The scopes actually granted, which may be narrower than those asked for.
    scope: Option<String>,
    /// Absolute, not the wire's relative `expires_in`: a duration stored as a
    /// duration would be wrong by however long the grant sat unused.
    #[serde(with = "unix_seconds")]
    expires_at: Option<SystemTime>,
}

/// `Option<SystemTime>` as optional whole seconds since the epoch.
///
/// serde's own `SystemTime` encoding is a `{secs_since_epoch, nanos_since_epoch}`
/// pair, which is noise in a credential file someone may open to see what is in
/// it. Sub-second precision on a token expiry is meaningless anyway — the skew
/// window is thirty seconds.
mod unix_seconds {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let seconds = value.and_then(|at| {
            at.duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        });
        seconds.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        Ok(Option::<u64>::deserialize(deserializer)?
            .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs)))
    }
}

/// The wire shape of a token response (RFC 6749 §5.1).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "bearer")]
    token_type: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn bearer() -> String {
    "Bearer".to_string()
}

impl Tokens {
    /// The `Authorization` header value to send on every request to the
    /// resource server.
    ///
    /// The scheme is the literal `Bearer`, **not** whatever `token_type` the
    /// server sent back. OAuth 2.1 §5.1.1 specifies that spelling and RFC 6750
    /// §2.1 makes it case-insensitive, but resource servers do a plain prefix
    /// comparison in practice: Sentry's MCP server answers a token request with
    /// `"token_type": "bearer"` and then rejects `Authorization: bearer <token>`
    /// with `invalid_token`, accepting only `Bearer`. Echoing the field back is
    /// the obvious-looking thing to do and it does not work.
    ///
    /// A non-bearer `token_type` is passed through as sent — MCP defines only
    /// bearer, so anything else is a server going beyond the spec and guessing
    /// at its capitalization would be worse than repeating it.
    pub fn authorization_header(&self) -> String {
        let scheme = if self.token_type.eq_ignore_ascii_case("bearer") {
            "Bearer"
        } else {
            &self.token_type
        };
        format!("{scheme} {}", self.access_token)
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    /// The granted scopes, space-delimited as the server sent them.
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    pub fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    /// Whether this grant is at or near its stated expiry. A grant with no
    /// stated expiry never reports expired — the resource server's `401` is
    /// the only authority on that case.
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|at| SystemTime::now() + EXPIRY_SKEW >= at)
    }

    fn from_response(response: TokenResponse) -> Self {
        Self {
            access_token: response.access_token,
            token_type: response.token_type,
            refresh_token: response.refresh_token,
            scope: response.scope,
            expires_at: response
                .expires_in
                .map(|seconds| SystemTime::now() + Duration::from_secs(seconds)),
        }
    }
}

/// Redacts both tokens. These land in error paths and log lines, and either
/// one is enough to impersonate the user at the resource server.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    http: &HttpClient,
    token_endpoint: &str,
    request: CodeExchange<'_>,
) -> anyhow::Result<Tokens> {
    post_token(
        http,
        token_endpoint,
        "application/x-www-form-urlencoded",
        request.form(),
    )
    .await
}

/// Trade a refresh token for a fresh grant.
///
/// OAuth 2.1 requires authorization servers to *rotate* refresh tokens for
/// public clients, so the response usually carries a new one — but not always,
/// and a response that omits it means the old one is still current. Carrying
/// the previous token forward in that case is what keeps a non-rotating server
/// from silently losing the ability to refresh.
pub async fn refresh(
    http: &HttpClient,
    token_endpoint: &str,
    previous: &Tokens,
    client_id: &str,
    resource: &str,
) -> anyhow::Result<Tokens> {
    let refresh_token = previous
        .refresh_token()
        .context("no refresh token was issued for this grant")?;
    let form = refresh_form(refresh_token, client_id, resource);
    let tokens = post_token(
        http,
        token_endpoint,
        "application/x-www-form-urlencoded",
        form,
    )
    .await?;
    Ok(carry_refresh_forward(previous, tokens))
}

/// Trade a refresh token for a fresh grant at an issuer that takes **JSON**
/// rather than the form encoding RFC 6749 specifies.
///
/// `auth.openai.com` is one: Codex CLI posts `{client_id, grant_type,
/// refresh_token}` as `application/json` to refresh, while still form-encoding
/// its authorization-code exchange. The asymmetry is the issuer's, not ours.
pub async fn refresh_json(
    http: &HttpClient,
    token_endpoint: &str,
    previous: &Tokens,
    client_id: &str,
) -> anyhow::Result<Tokens> {
    let refresh_token = previous
        .refresh_token()
        .context("no refresh token was issued for this grant")?;
    let tokens = post_token(
        http,
        token_endpoint,
        "application/json",
        refresh_json_body(refresh_token, client_id),
    )
    .await?;
    Ok(carry_refresh_forward(previous, tokens))
}

/// The JSON refresh body. Split out like [`refresh_form`] so the encoding can
/// be asserted without a server — the two encodings sitting side by side is the
/// kind of asymmetry a later reader corrects into a bug.
fn refresh_json_body(refresh_token: &str, client_id: &str) -> String {
    serde_json::json!({
        "client_id": client_id,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    })
    .to_string()
}

/// A response that omits `refresh_token` leaves the previous one current
/// ([`refresh`]). Shared so the two encodings cannot drift on the one piece of
/// this that is not wire format.
fn carry_refresh_forward(previous: &Tokens, mut fresh: Tokens) -> Tokens {
    if fresh.refresh_token.is_none() {
        fresh.refresh_token = previous.refresh_token.clone();
    }
    fresh
}

/// The refresh request body. Split out from [`refresh`] so the encoding can be
/// asserted without a server.
fn refresh_form(refresh_token: &str, client_id: &str, resource: &str) -> String {
    form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", client_id)
        .append_pair("resource", resource)
        .finish()
}

/// The parameters of an authorization-code token request.
///
/// A struct rather than a long argument list because every field is a string
/// and transposing two of them would produce a request that fails at the
/// server with a generic `invalid_grant`.
#[derive(Debug, Clone, Copy)]
pub struct CodeExchange<'a> {
    pub code: &'a str,
    /// Must be byte-identical to the `redirect_uri` sent on the authorization
    /// request; the server compares them.
    pub redirect_uri: &'a str,
    pub client_id: &'a str,
    /// The PKCE verifier whose challenge went out on the authorization request.
    pub code_verifier: &'a str,
    /// The RFC 8707 resource indicator — the canonical URI of the MCP server.
    /// `None` for an issuer that has no such concept: the parameter is
    /// *unconditional* for MCP, where a server that understands it binds the
    /// token's audience, but `auth.openai.com` issues one audience and takes no
    /// resource at all.
    pub resource: Option<&'a str>,
}

impl CodeExchange<'_> {
    fn form(&self) -> String {
        let mut form = form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", "authorization_code")
            .append_pair("code", self.code)
            .append_pair("redirect_uri", self.redirect_uri)
            .append_pair("client_id", self.client_id)
            .append_pair("code_verifier", self.code_verifier);
        if let Some(resource) = self.resource {
            form.append_pair("resource", resource);
        }
        form.finish()
    }
}

/// The token endpoint answered, and refused. Typed apart from a transport
/// failure because the remedies differ: a refused refresh token needs a new
/// login, where an unreachable issuer needs only a later attempt.
#[derive(Debug)]
pub struct Rejected {
    endpoint: String,
    pub status: u16,
    detail: String,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "token endpoint {} returned {}: {}",
            self.endpoint, self.status, self.detail
        )
    }
}

impl std::error::Error for Rejected {}

async fn post_token(
    http: &HttpClient,
    token_endpoint: &str,
    content_type: &'static str,
    body: String,
) -> anyhow::Result<Tokens> {
    let mut conn = http
        .post(token_endpoint)
        .with_request_header("content-type", content_type)
        .with_request_header("accept", "application/json")
        .with_body(body)
        .await
        .with_context(|| format!("POST {token_endpoint}"))?;
    let status = conn.status().map_or(0, u16::from);
    let body = conn.response_body().read_string().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(Rejected {
            endpoint: token_endpoint.to_string(),
            status,
            detail: describe_error(&body),
        }
        .into());
    }
    let response: TokenResponse = serde_json::from_str(&body)
        .with_context(|| format!("unparseable token response: {body}"))?;
    Ok(Tokens::from_response(response))
}

#[cfg(test)]
mod test;
