//! The authorization request, and validation of the response that comes back
//! to the redirect URI (docs/mcp-auth.md).
//!
//! [`Pending`] is the record the client keeps between sending the user to the
//! authorization server and receiving the callback. It holds the three things
//! the response is checked against — the expected issuer, the `state`, and the
//! PKCE verifier — because a response can only be trusted relative to the
//! request that caused it.
//!
//! ## The mix-up defense
//!
//! An authorization server the client also talks to legitimately may try to
//! get the client to hand it a code issued by a *different*, honest server.
//! RFC 9207 defends against this by having the authorization server echo its
//! own identity as `iss` on the response, which the client compares against
//! the issuer it recorded before redirecting. [`Pending::validate`] implements
//! the table from the spec:
//!
//! | `authorization_response_iss_parameter_supported` | `iss` in response | action |
//! | --- | --- | --- |
//! | `true` | present | compare |
//! | `true` | absent | **reject** |
//! | `false` or absent | present | compare |
//! | `false` or absent | absent | proceed |
//!
//! The third row is deliberate: a present `iss` is compared whatever the
//! metadata claims, so a server that emits `iss` before advertising it is
//! still checked.
//!
//! The comparison is `==` on the form-decoded string, with **no**
//! normalization — no case folding, no default-port elision, no trailing
//! slash, no percent-encoding cleanup. Normalizing would make two distinct
//! issuer identifiers compare equal, which is the whole attack.
//!
//! ## Error responses are validated too
//!
//! An authorization server can fail the request, and the failure carries
//! attacker-influenced text. `iss` is checked *before* anything reads
//! `error`, `error_description`, or `error_uri`, so a mismatched response
//! cannot get its message displayed to the operator.

use trillium_client::Url;

use super::metadata::AuthorizationServer;
use super::pkce::{self, Pkce};

/// An authorization request in flight: what was sent, and what the response
/// will be checked against.
#[derive(Debug)]
pub struct Pending {
    /// The issuer recorded from the authorization server's *validated*
    /// metadata before redirecting. Comparing against anything less
    /// trustworthy would make the check decorative.
    issuer: String,
    /// Whether that metadata advertised RFC 9207 support, which decides
    /// whether a *missing* `iss` is fatal.
    iss_advertised: bool,
    state: String,
    pkce: Pkce,
    redirect_uri: String,
    /// The scopes asked for, kept so a later `insufficient_scope` challenge
    /// can be answered with the union rather than just the new scopes.
    scope: Option<String>,
}

/// What a validated callback yielded.
#[derive(Debug, Clone)]
pub struct Authorized {
    pub code: String,
    /// The verifier to send to the token endpoint, moved out of the [`Pending`].
    pub code_verifier: String,
    pub redirect_uri: String,
}

impl Pending {
    /// An authorization request to an issuer known ahead of time rather than
    /// discovered, whose authorization URL the caller builds because that
    /// issuer requires parameters beyond the standard set
    /// (`inference/chatgpt.rs`).
    ///
    /// `iss_advertised` is false: no metadata was read, so nothing claimed RFC
    /// 9207 support and a *missing* `iss` cannot be held against the response.
    /// An `iss` that is present and wrong is still refused.
    pub fn to_known_issuer(
        issuer: &str,
        redirect_uri: &str,
        state: String,
        pkce: Pkce,
        scope: Option<String>,
    ) -> Self {
        Self {
            issuer: issuer.to_string(),
            iss_advertised: false,
            state,
            pkce,
            redirect_uri: redirect_uri.to_string(),
            scope,
        }
    }

    /// Build an authorization request for `server`, and the URL to open.
    ///
    /// `resource` is the RFC 8707 resource indicator — the canonical URI of
    /// the MCP server this token will be used at — and is sent whether or not
    /// the authorization server claims to understand it.
    pub fn new(
        server: &AuthorizationServer,
        client_id: &str,
        redirect_uri: &str,
        resource: &str,
        scope: Option<String>,
    ) -> anyhow::Result<(Self, Url)> {
        let pending = Self {
            issuer: server.issuer.clone(),
            iss_advertised: server
                .authorization_response_iss_parameter_supported
                .unwrap_or(false),
            state: pkce::random_token(),
            pkce: Pkce::generate(),
            redirect_uri: redirect_uri.to_string(),
            scope,
        };
        let url = pending.url(server.authorization_endpoint()?, client_id, resource)?;
        Ok((pending, url))
    }

    /// The URL to open in a browser.
    ///
    /// Parameters are appended rather than replacing the query, because RFC
    /// 8414 permits an authorization endpoint to carry query parameters of
    /// its own and dropping them would break such a server.
    fn url(&self, endpoint: &str, client_id: &str, resource: &str) -> anyhow::Result<Url> {
        let mut url = Url::parse(endpoint)
            .map_err(|e| anyhow::anyhow!("authorization_endpoint {endpoint} is not a URL: {e}"))?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", client_id)
                .append_pair("redirect_uri", &self.redirect_uri)
                .append_pair("state", &self.state)
                .append_pair("code_challenge", self.pkce.challenge())
                .append_pair("code_challenge_method", pkce::S256)
                .append_pair("resource", resource);
            if let Some(scope) = &self.scope {
                query.append_pair("scope", scope);
            }
        }
        Ok(url)
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The scopes this request asked for, for computing the union on a
    /// step-up.
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// Validate an authorization response and extract the code.
    ///
    /// `params` is the decoded query of the redirect. Checks run in the order
    /// the spec requires: issuer identity first — so a mismatched response
    /// never gets its error text read — then `state`, then the error
    /// response, and only then the code.
    pub fn validate(self, params: &[(String, String)]) -> anyhow::Result<Authorized> {
        let get = |name: &str| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };

        match get("iss") {
            // Simple string comparison per RFC 3986 §6.2.1 — no normalization.
            Some(iss) => anyhow::ensure!(
                iss == self.issuer,
                "the authorization response came from issuer {iss:?}, not {:?}; discarding it \
                 without reading its contents",
                self.issuer
            ),
            None => anyhow::ensure!(
                !self.iss_advertised,
                "{} advertises authorization_response_iss_parameter_supported but its response \
                 carried no iss; discarding it",
                self.issuer
            ),
        }

        let state = get("state").unwrap_or_default();
        anyhow::ensure!(
            state == self.state,
            "the authorization response carried the wrong state; it does not belong to this \
             request"
        );

        // Only now is the server's own text safe to surface.
        if let Some(error) = get("error") {
            let description = get("error_description").unwrap_or("no description");
            anyhow::bail!("authorization was refused: {error} ({description})");
        }

        let code = get("code")
            .filter(|code| !code.is_empty())
            .ok_or_else(|| anyhow::anyhow!("the authorization response carried no code"))?;

        Ok(Authorized {
            code: code.to_string(),
            code_verifier: self.pkce.verifier().to_string(),
            redirect_uri: self.redirect_uri,
        })
    }
}

/// The union of already-requested scopes and those a challenge demands,
/// space-delimited and sorted for a stable request.
///
/// Re-authorizing with only the challenged scopes would drop permissions
/// already granted for other operations, so a step-up sends both — the
/// client-side half of the spec's scope accumulation requirement.
pub fn union_scopes(previous: Option<&str>, challenged: Option<&str>) -> Option<String> {
    let mut scopes: Vec<&str> = previous
        .into_iter()
        .chain(challenged)
        .flat_map(str::split_whitespace)
        .collect();
    scopes.sort_unstable();
    scopes.dedup();
    Some(scopes.join(" ")).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod test;
