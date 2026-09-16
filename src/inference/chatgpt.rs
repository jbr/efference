//! The ChatGPT subscription grant: logging in, storing it, and refreshing it
//! (docs/transport.md, docs/secrets.md).
//!
//! `auth = { kind = "codex-cache" }` reads the token Codex CLI keeps, which
//! makes Codex CLI the only thing that can renew it. This is the other half:
//! efference runs the authorization dance itself and holds the result, so a
//! user with no Codex install — or one whose Codex keeps its login in the
//! keyring, where there is no file to read — has a way in, and so does a host
//! that has to renew unattended.
//!
//! The grant is efference's own. A refresh may return a **new** refresh token,
//! so refreshing *Codex's* grant would leave its `auth.json` holding a dead
//! one: efference would work and `codex` would break. Two grants under the same
//! client id are the ordinary "logged in from another computer" case, so
//! establishing a separate one keeps the two independent.
//!
//! Every fixed value here is read from Codex CLI's own login server
//! (`codex-rs/login/`), because OpenAI issues no client id to third parties —
//! the public one below is what Codex, opencode, and pi all present.
//!
//! [`crate::oauth`] holds the protocol; this holds one issuer's choices.

use std::time::{Duration, SystemTime};

use crate::oauth::token::Tokens;
use crate::oauth::{authorize, loopback, pkce, token};
use crate::secrets::Secrets;

/// The issuer every request here goes to.
const ISSUER: &str = "https://auth.openai.com";

/// OpenAI issues no client id to third-party harnesses; this is Codex CLI's
/// public one, which is what the redirect URIs below are registered against.
/// A public client, so there is no secret and PKCE carries the whole burden.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Both ports are *registered* redirect URIs, not a preference and a
/// workaround: Codex falls back from the first to the second and builds its
/// `redirect_uri` from whichever it bound, so the issuer exact-matches either.
const PORTS: [u16; 2] = [1455, 1457];

/// Dictated by the registration, not chosen here.
const CALLBACK_PATH: &str = "/auth/callback";

/// The registered URIs say `localhost` while the listener binds `127.0.0.1`.
/// The issuer compares the *spelling*, so this is not interchangeable with the
/// address (docs/transport.md).
const REDIRECT_HOST: &str = "localhost";

/// What Codex asks for, and what the granted scopes on a live token were
/// measured to be. Narrower would be a different grant than the one the
/// registered redirect URIs are for.
const SCOPE: &str = "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// Refresh once the access token is within this of expiring, rather than after
/// a rejection: the token carries its own `exp` (ten days, as issued), so the
/// deadline is known without asking anyone.
const REFRESH_WITHIN: Duration = Duration::from_secs(60 * 60);

/// A stored ChatGPT grant (docs/secrets.md): the tokens plus the account the
/// Codex backend requires on every request.
///
/// Held under `oauth:<connection>` rather than `inference:<connection>`,
/// because that name means a bare secret and this is a JSON record — a
/// distinction a person reading the store should not have to guess at.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Grant {
    /// Checked before the grant is used: a refresh token is only good at the
    /// issuer that minted it.
    pub issuer: String,
    pub tokens: Tokens,
    /// `chatgpt_account_id`, read from the access token's claims at login.
    /// Rides `ChatGPT-Account-ID` on every request and is stable across a
    /// refresh of the same login, so it is stored rather than re-derived.
    pub account_id: String,
    /// `chatgpt_plan_type` at the time of login, for the boot line. Advisory:
    /// a plan can change without a new login.
    pub plan: Option<String>,
}

/// Never the tokens.
impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("issuer", &self.issuer)
            .field("tokens", &self.tokens)
            .field("account_id", &self.account_id)
            .field("plan", &self.plan)
            .finish()
    }
}

impl Grant {
    /// Load the grant stored for `connection`, or `None` if there is none.
    pub fn load(secrets: &Secrets, connection: &str) -> Result<Option<Self>, String> {
        let name = crate::secrets::oauth_name(connection);
        let Some(json) = secrets.get(&name).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        // A record that does not parse is an error, not "no grant": the latter
        // would quietly start a browser dance and overwrite it.
        serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("`{name}` is not readable as a stored grant: {e}"))
    }

    pub fn save(&self, secrets: &Secrets, connection: &str) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        secrets
            .set(&crate::secrets::oauth_name(connection), &json)
            .map_err(|e| e.to_string())
    }

    /// Whether the access token is at or past the point where it should be
    /// renewed. A token with no stated expiry is taken as current: the issuer
    /// said nothing, and guessing would refresh on every request.
    pub fn needs_refresh(&self) -> bool {
        self.tokens
            .expires_at()
            .is_some_and(|at| at <= SystemTime::now() + REFRESH_WITHIN)
    }
}

/// The `https://api.openai.com/auth` claim block, which carries the account and
/// plan. Read from the access token rather than the id token: the id token
/// lives one hour where the access token lives days, so a stored grant's id
/// token is expired long before its access token is.
#[derive(serde::Deserialize)]
struct AuthClaims {
    chatgpt_account_id: String,
    chatgpt_plan_type: Option<String>,
}

/// The claims of a JWT, **without verifying its signature**.
///
/// Verification would be the issuer's job and is not what this is for: the
/// token is not a credential being *accepted* here, it is one this process just
/// received over TLS from the issuer and is about to send back. What is read
/// out of it — the account id, the expiry — is used to address and schedule,
/// and a forged value would only break this client's own requests.
fn claims<T: serde::de::DeserializeOwned>(jwt: &str, at: &str) -> Result<T, String> {
    let payload = jwt.split('.').nth(1).ok_or("not a JWT")?;
    let bytes = base64_url_decode(payload)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("unreadable JWT claims: {e}"))?;
    let claim = value.get(at).ok_or_else(|| format!("no `{at}` claim"))?;
    serde_json::from_value(claim.clone()).map_err(|e| format!("unreadable `{at}` claim: {e}"))
}

/// Base64url without padding, which is what a JWT segment is.
fn base64_url_decode(segment: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|e| format!("undecodable JWT segment: {e}"))
}

/// The account and plan an access token names.
fn account_of(access_token: &str) -> Result<(String, Option<String>), String> {
    let auth: AuthClaims = claims(access_token, "https://api.openai.com/auth")?;
    Ok((auth.chatgpt_account_id, auth.chatgpt_plan_type))
}

/// Run the authorization dance and return the grant, without storing it.
///
/// `open_browser` is handed the authorization URL. It is a parameter rather
/// than a call so that a caller with a terminal can print the URL instead, and
/// so a test never opens anything.
pub async fn log_in(
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Grant, String> {
    // Bound before the URL is built: the redirect URI has to name the port that
    // is actually listening, and both requests must carry it byte-identically.
    //
    // Deliberately not Codex's contention handling, which sends a *cancel* to
    // whatever holds 1455 and takes it over. That is one Codex login server
    // displacing another; aimed at a `codex login` the operator is running, it
    // would cancel their real login.
    let listener = loopback::listen_at(&PORTS, CALLBACK_PATH)
        .await
        .map_err(|e| format!("{e}. Close whatever is using port 1455 or 1457 and try again"))?;
    let redirect_uri = listener.redirect_uri_for(REDIRECT_HOST);

    let pkce = pkce::Pkce::generate();
    let state = pkce::random_token();
    let url = authorize_url(&redirect_uri, &pkce, &state);
    open_browser(&url)?;

    let params = listener
        .wait()
        .await
        .map_err(|e| format!("waiting for the browser redirect: {e}"))?;
    let authorized = authorize::Pending::to_known_issuer(
        ISSUER,
        &redirect_uri,
        state,
        pkce,
        Some(SCOPE.to_string()),
    )
    .validate(&params)
    .map_err(|e| format!("the authorization response was rejected: {e}"))?;

    let http = crate::oauth::http_client();
    let tokens = token::exchange_code(
        &http,
        &format!("{ISSUER}/oauth/token"),
        token::CodeExchange {
            code: &authorized.code,
            redirect_uri: &redirect_uri,
            client_id: CLIENT_ID,
            code_verifier: &authorized.code_verifier,
            // This issuer has one audience and takes no resource indicator.
            resource: None,
        },
    )
    .await
    .map_err(|e| format!("exchanging the authorization code: {e}"))?;

    let (account_id, plan) = account_of(tokens.access_token())?;
    Ok(Grant {
        issuer: ISSUER.to_string(),
        tokens,
        account_id,
        plan,
    })
}

/// The authorization URL, with the parameters this issuer requires beyond the
/// standard set. Split out so the query can be asserted without a browser.
fn authorize_url(redirect_uri: &str, pkce: &pkce::Pkce, state: &str) -> String {
    let query = trillium_client::url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", pkce.challenge())
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .finish();
    format!("{ISSUER}/oauth/authorize?{query}")
}

/// Trade the stored grant's refresh token for a fresh one and store it.
///
/// This issuer takes **JSON** here, though its authorization-code exchange is
/// form-encoded ([`token::refresh_json`]). A response may carry a new refresh
/// token, which is why this grant is efference's own and never Codex's.
pub async fn refresh(secrets: &Secrets, connection: &str, grant: &Grant) -> Result<Grant, String> {
    if grant.issuer != ISSUER {
        return Err(format!(
            "the stored grant was issued by {}, not {ISSUER}; log in again",
            grant.issuer
        ));
    }
    let http = crate::oauth::http_client();
    let tokens = token::refresh_json(
        &http,
        &format!("{ISSUER}/oauth/token"),
        &grant.tokens,
        CLIENT_ID,
    )
    .await
    .map_err(|e| format!("refreshing the ChatGPT grant: {e}"))?;

    // Re-read from the new token rather than carried over: a refresh is where
    // a plan change would show up, and the account is asserted afresh instead
    // of assumed.
    let (account_id, plan) = account_of(tokens.access_token())?;
    let refreshed = Grant {
        issuer: grant.issuer.clone(),
        tokens,
        account_id,
        plan,
    };
    refreshed.save(secrets, connection)?;
    Ok(refreshed)
}

#[cfg(test)]
mod test;
