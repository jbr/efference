//! The upstream endpoint and its environment-driven resolution (docs/transport.md).

use super::*;

/// The provider endpoint (docs/transport.md). The request *body* is always the
/// Anthropic Messages wire — `project(log)` output — because that shape is what
/// every supported provider serves (Anthropic directly; OpenRouter via its
/// `/api/v1/messages` skin). Switching providers therefore only changes **where**
/// the POST goes and **how** it authenticates, so those are the only two things
/// this carries. Resolved once from the environment at boot and shared by the
/// agent [`Client`] and the fork's spotter client, so both hit the same endpoint
/// over the same cached prefix.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Full messages URL, e.g. `https://api.anthropic.com/v1/messages` or
    /// `https://openrouter.ai/api/v1/messages`.
    pub url: String,
    /// The auth header **name** — `x-api-key` for Anthropic, `authorization` for
    /// OpenRouter's `Bearer` scheme. A runtime `String` (not a `&'static str`) so
    /// the provider, not the code, chooses it.
    pub auth_header: String,
    /// The auth header **value**, already assembled — the raw key for `x-api-key`,
    /// or `Bearer sk-or-...` once the scheme prefix is prepended (see [`from_env`]).
    ///
    /// [`from_env`]: Self::from_env
    pub auth_value: String,
}

impl Endpoint {
    /// Resolve the endpoint from the environment. Every knob is optional, so an
    /// existing `ANTHROPIC_API_KEY`-only setup is entirely unchanged:
    ///
    /// - `EFFERENCE_ENDPOINT` — the full messages URL. Defaults to Anthropic's.
    /// - `EFFERENCE_API_KEY` — the **raw secret**, nothing else. Kept alone (not
    ///   `header=value`) so it can be a *whole-value* 1Password reference: `op run`
    ///   only substitutes a reference that is the entire value, never one embedded
    ///   mid-string. The header **name** and any scheme **prefix** are separate:
    /// - `EFFERENCE_AUTH_HEADER` — the auth header name. Defaults to `x-api-key`.
    /// - `EFFERENCE_AUTH_PREFIX` — a literal prepended to the secret to form the
    ///   header value. Defaults to empty; set `"Bearer "` (trailing space included)
    ///   for OpenRouter's `authorization: Bearer <key>`. Splitting the prefix off
    ///   keeps the secret a whole value, so `EFFERENCE_AUTH_PREFIX="Bearer "` +
    ///   `EFFERENCE_API_KEY="op://…"` resolves cleanly under `op run`.
    ///
    /// All provider knowledge thus stays in the environment, not the code. When
    /// `EFFERENCE_API_KEY` is absent, falls back to `ANTHROPIC_API_KEY` sent bare as
    /// `x-api-key` (the historical behavior); the `EFFERENCE_AUTH_*` knobs apply
    /// only to the `EFFERENCE_API_KEY` path.
    pub fn from_env() -> Result<Self, Error> {
        Self::resolve(
            std::env::var("EFFERENCE_ENDPOINT").ok(),
            std::env::var("EFFERENCE_API_KEY").ok(),
            std::env::var("EFFERENCE_AUTH_HEADER").ok(),
            std::env::var("EFFERENCE_AUTH_PREFIX").ok(),
            std::env::var("ANTHROPIC_API_KEY").ok(),
        )
    }

    /// The pure core of [`Self::from_env`], factored out so the precedence and the
    /// header/prefix/secret assembly are unit-testable without touching process env.
    pub(super) fn resolve(
        endpoint: Option<String>,
        api_key: Option<String>,
        auth_header: Option<String>,
        auth_prefix: Option<String>,
        anthropic_key: Option<String>,
    ) -> Result<Self, Error> {
        let url = endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        match (api_key, anthropic_key) {
            // The configurable path: raw secret + optional header name + optional prefix.
            (Some(key), _) => {
                let auth_header = auth_header
                    .map(|h| h.trim().to_string())
                    .filter(|h| !h.is_empty())
                    .unwrap_or_else(|| "x-api-key".to_string());
                let auth_value = format!("{}{}", auth_prefix.unwrap_or_default(), key);
                Ok(Self {
                    url,
                    auth_header,
                    auth_value,
                })
            }
            // Backward-compat: a bare `ANTHROPIC_API_KEY` is always `x-api-key: <key>`,
            // ignoring the `EFFERENCE_AUTH_*` knobs (those belong to the path above).
            (None, Some(anthropic)) => Ok(Self {
                url,
                auth_header: "x-api-key".to_string(),
                auth_value: anthropic,
            }),
            (None, None) => Err("no API credentials: set EFFERENCE_API_KEY (plus \
                                 EFFERENCE_ENDPOINT / EFFERENCE_AUTH_HEADER / \
                                 EFFERENCE_AUTH_PREFIX for a non-Anthropic provider), or \
                                 ANTHROPIC_API_KEY for the default Anthropic endpoint"
                .into()),
        }
    }
}
