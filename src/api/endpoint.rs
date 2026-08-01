//! The upstream endpoint, derived from the declared provider (docs/transport.md).

use super::*;

/// The provider endpoint (docs/transport.md). The request *body* is always the
/// Anthropic Messages wire — `project(log)` output — because that shape is what
/// every supported provider serves (Anthropic directly; OpenRouter via its
/// `/api/v1/messages` skin). Which provider fronts it is **declared** via
/// `EFFERENCE_PROVIDER` and everything else — the URL, the auth header name,
/// the scheme prefix — is derived from that identity by [`Provider`], so
/// switching providers is one word plus the matching key. Resolved once from
/// the environment at boot and shared by the agent [`Client`] and the fork's
/// spotter client, so both hit the same endpoint over the same cached prefix.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// The declared API family — also the source of the model-metadata query
    /// (`Endpoint::fetch_capabilities`, docs/transport.md).
    pub provider: Provider,
    /// Full messages URL, e.g. `https://api.anthropic.com/v1/messages` or
    /// `https://openrouter.ai/api/v1/messages`. Derived from `provider`.
    pub url: String,
    /// The auth header **name** — `x-api-key` for Anthropic, `authorization`
    /// for OpenRouter's `Bearer` scheme. Derived from `provider`.
    pub auth_header: String,
    /// The auth header **value**, already assembled — the raw key for
    /// `x-api-key`, or `Bearer sk-or-…` once the provider's scheme prefix is
    /// prepended. The prefix lives on the provider and the secret arrives
    /// alone, so the secret stays a *whole* value (an `op://…` reference that
    /// `op run` substitutes — it only resolves a reference that is the entire
    /// value, never one embedded mid-string).
    pub auth_value: String,
}

/// The env knobs the provider enum replaced. Set ones are a hard error naming
/// the replacement — silently ignoring them would post to a different URL than
/// the operator's environment says (docs/transport.md).
const RETIRED_KNOBS: [&str; 3] = [
    "EFFERENCE_ENDPOINT",
    "EFFERENCE_AUTH_HEADER",
    "EFFERENCE_AUTH_PREFIX",
];

impl Endpoint {
    /// Resolve the endpoint from the environment:
    ///
    /// - `EFFERENCE_PROVIDER` — `anthropic` (default when unset) or `openrouter`.
    /// - `EFFERENCE_API_KEY` — the **raw secret**, nothing else; header name and
    ///   scheme prefix come from the provider.
    ///
    /// When `EFFERENCE_API_KEY` is absent, an `ANTHROPIC_API_KEY` fallback
    /// covers the Anthropic provider only — an Anthropic key is the wrong
    /// secret for any other provider, so failing early beats a confusing 401.
    pub fn from_env() -> Result<Self, Error> {
        for knob in RETIRED_KNOBS {
            if std::env::var_os(knob).is_some() {
                return Err(format!(
                    "{knob} is no longer read: the provider is declared, not assembled — \
                     set EFFERENCE_PROVIDER=anthropic|openrouter and unset {knob} \
                     (docs/transport.md)"
                )
                .into());
            }
        }
        Self::resolve(
            std::env::var("EFFERENCE_PROVIDER").ok(),
            std::env::var("EFFERENCE_API_KEY").ok(),
            std::env::var("ANTHROPIC_API_KEY").ok(),
        )
    }

    /// The pure core of [`Self::from_env`], factored out so provider parsing,
    /// key precedence, and header assembly are unit-testable without touching
    /// process env.
    pub(super) fn resolve(
        provider: Option<String>,
        api_key: Option<String>,
        anthropic_key: Option<String>,
    ) -> Result<Self, Error> {
        let provider = Provider::parse(provider.as_deref())?;
        let key = match (provider, api_key, anthropic_key) {
            (_, Some(key), _) => key,
            (Provider::Anthropic, None, Some(key)) => key,
            (Provider::OpenRouter, None, Some(_)) => {
                return Err("EFFERENCE_PROVIDER=openrouter needs EFFERENCE_API_KEY (an \
                            OpenRouter `sk-or-…` secret); ANTHROPIC_API_KEY is the wrong \
                            credential for OpenRouter"
                    .into());
            }
            (_, None, None) => {
                return Err("no API credentials: set EFFERENCE_API_KEY (with \
                            EFFERENCE_PROVIDER=anthropic|openrouter), or ANTHROPIC_API_KEY \
                            for the default Anthropic provider"
                    .into());
            }
        };
        Ok(Self {
            provider,
            url: provider.messages_url().to_string(),
            auth_header: provider.auth_header().to_string(),
            auth_value: format!("{}{}", provider.auth_prefix(), key),
        })
    }
}
