//! The upstream endpoint, derived from the declared provider (docs/transport.md).

use super::*;

/// The provider endpoint (docs/transport.md): the URL, auth header name, and
/// scheme prefix all derived from the declared [`Provider`] rather than
/// configured separately. Resolved once at boot and shared by the agent
/// [`Client`] and the fork's spotter client, so both hit the same endpoint over
/// the same cached prefix.
///
/// `session_id` is attached after resolution, by the daemon's `serve`, and is
/// absent in offline verbs that have no conversation to pin. Whether it actually
/// rides a request is the *provider's* call ([`Provider::session_header`]) — the
/// field carries this session's key regardless, so the derivation stays in one
/// place.
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
    /// This session's sticky-routing key (docs/transport.md), or `None` where
    /// there is no conversation to pin. Set by the daemon's `serve` from
    /// [`sticky_session_id`]; carried on every request the provider declares a
    /// [`session_header`] for. Derived once at boot from the session dir, which
    /// is fixed for the session's life — it must not move when the projection
    /// does, since a key that tracked the projection would re-route exactly when
    /// consolidation rewrites the opening messages.
    ///
    /// [`session_header`]: Provider::session_header
    pub session_id: Option<String>,
}

/// The cap OpenRouter puts on a session key. `--name` is arbitrary user input,
/// so the derivation below has to fit inside it rather than assume it does.
const SESSION_ID_MAX: usize = 256;

/// What distinguishes the spotter's key from the agent's ([`Endpoint::for_spotter`]).
const SPOTTER_SUFFIX: &str = "-spot";

/// This session's sticky-routing key: `<session id>-<hash of the session dir's
/// absolute path>` (docs/transport.md).
///
/// The path is canonicalized through the *parent*, not the dir itself: on the first
/// turn the session dir does not exist yet, and taking the same branch either way is
/// what keeps a fresh session and a resumed one on one key. `cwd.join(dir)` is not
/// enough — it is absolute but not canonical, so the same session reached through a
/// symlinked path (`/tmp` vs `/private/tmp` on macOS) would hash differently.
pub fn sticky_session_id(session_dir: &std::path::Path) -> String {
    let name = session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed");
    let hash = fnv1a_hex(&canonical_display(session_dir));
    // Truncate the *name*, never the tail: the hash is the disambiguating half, so a
    // cap that dropped it would defeat the thing the key exists for. The spotter's
    // suffix is reserved here so its derived key fits the same cap.
    let budget = SESSION_ID_MAX - SPOTTER_SUFFIX.len() - hash.len() - 1;
    format!("{}-{hash}", truncate_on_char_boundary(name, budget))
}

/// The session dir as an absolute, symlink-resolved string — see
/// [`sticky_session_id`] for why the parent is what gets canonicalized. Falls back
/// through `cwd`-joining and then the path as given; the key only has to be stable
/// within one session, so a fallback that stays constant is enough.
fn canonical_display(session_dir: &std::path::Path) -> String {
    let parent = match session_dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    let resolved = match (std::fs::canonicalize(&parent), session_dir.file_name()) {
        (Ok(parent), Some(name)) => Some(parent.join(name)),
        _ => std::env::current_dir()
            .ok()
            .map(|cwd| cwd.join(session_dir)),
    };
    resolved
        .unwrap_or_else(|| session_dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// FNV-1a over `s`, hex. Spelled out rather than reached for from `std`:
/// `DefaultHasher`'s output is explicitly not stable across Rust releases, so a
/// rebuild mid-session would move the key — the one property this whole mechanism
/// is for. Not a cryptographic hash and doesn't need to be; it disambiguates two
/// paths, and nothing downstream trusts it.
fn fnv1a_hex(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The longest prefix of `s` that fits in `max` **bytes** and ends on a character
/// boundary. `--name` is arbitrary user input, so a plain `&s[..max]` is a panic
/// waiting for the first multibyte name.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
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
        Self::from_env_with(&std::collections::BTreeMap::new())
    }

    /// [`from_env`](Self::from_env) with the config's resolved `[env]` values
    /// consulted **first** (docs/env.md): the harness's own credential can be an
    /// `op://…` reference in `efference.toml` rather than a variable the process
    /// was launched with, and an explicit config entry beats an ambient
    /// variable. The value is masked either way — by construction for a resolved
    /// reference, and by `env_table::HARNESS_KEY_VARS` for the plain-variable
    /// path, which this does not retire.
    pub fn from_env_with(
        resolved: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self, Error> {
        let read = |name: &str| {
            resolved
                .get(name)
                .cloned()
                .or_else(|| std::env::var(name).ok())
        };
        for knob in RETIRED_KNOBS {
            if read(knob).is_some() {
                return Err(format!(
                    "{knob} is no longer read: the provider is declared, not assembled — \
                     set EFFERENCE_PROVIDER=anthropic|openrouter and unset {knob} \
                     (docs/transport.md)"
                )
                .into());
            }
        }
        Self::resolve(
            read("EFFERENCE_PROVIDER"),
            read("EFFERENCE_API_KEY"),
            read("ANTHROPIC_API_KEY"),
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
            session_id: None,
        })
    }

    /// The same endpoint under a **distinct** sticky key, for the spotter's client
    /// (docs/permissions.md, docs/transport.md).
    ///
    /// An explicit key overrides the message-hash derivation that would otherwise
    /// have separated the spotter's conversation from the agent's for free, so the
    /// separation has to be re-stated here.
    pub fn for_spotter(&self) -> Self {
        Self {
            session_id: self
                .session_id
                .as_ref()
                .map(|id| format!("{id}{SPOTTER_SUFFIX}")),
            ..self.clone()
        }
    }
}
