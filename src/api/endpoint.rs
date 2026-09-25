//! The upstream endpoint, derived from the declared provider (docs/transport.md).

use super::*;

/// The provider endpoint (docs/transport.md): the URL, auth header name, and
/// scheme prefix all derived from the declared [`Provider`] rather than
/// configured separately. Built once per host connection from resolved
/// credentials and cloned per session and role.
///
/// `session_id` is attached by boot, per session and role, and is absent in
/// offline verbs that have no conversation to pin. Whether it actually
/// rides a request is the *provider's* call ([`Provider::session_header`]) — the
/// field carries this session's key regardless, so the derivation stays in one
/// place.
#[derive(Clone)]
pub struct Endpoint {
    /// The declared API family — also the source of the model-metadata query
    /// (`Endpoint::fetch_capabilities`, docs/transport.md).
    pub provider: Provider,
    /// Full messages URL, e.g. `https://api.anthropic.com/v1/messages` or
    /// `https://openrouter.ai/api/v1/messages`. Derived from `provider`, and
    /// `None` for a provider that serves no turns (`Provider::messages_url`) —
    /// a decision-model connection has only `Provider::decisions_url`. Resolution
    /// refuses such a provider for every role that would build a
    /// [`Client`](crate::api::Client), so a client's url is always present.
    pub url: Option<String>,
    /// The auth header **name** — `x-api-key` for Anthropic, `authorization`
    /// for OpenRouter's `Bearer` scheme. Derived from `provider`.
    pub auth_header: String,
    /// The auth header **value**, already assembled — the raw key for
    /// `x-api-key`, or `Bearer sk-or-…` once the provider's scheme prefix is
    /// prepended. The prefix lives on the provider and the secret arrives
    /// alone, so the secret stays a *whole* value (an `op://…` reference that
    /// `op run` substitutes — it only resolves a reference that is the entire
    /// value, never one embedded mid-string).
    ///
    /// Held in a shared cell rather than as a string, so a credential that
    /// expires under a running host can be replaced for every session at once
    /// ([`AuthValue`], docs/transport.md).
    pub auth_value: AuthValue,
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
    /// Provider-specific identity headers beyond auth — the Codex backend's
    /// `ChatGPT-Account-ID`. Empty for every other provider.
    pub extra_headers: Vec<(String, String)>,
    /// The configured base URL, present only for [`Provider::Custom`] and `None`
    /// for every named provider, which owns its own URLs (docs/transport.md).
    ///
    /// Kept beside [`Self::url`] rather than derived back out of it because the
    /// metadata and credential-check routes are siblings of the turn route, not
    /// suffixes of it: trimming a path segment off a URL to rebuild them would
    /// guess at a structure the operator supplied whole.
    pub base: Option<String>,
}

/// One authenticated GET's whole answer, whatever its status
/// ([`Endpoint::probe`], docs/transport.md). A listing probe reads a
/// non-2xx response as evidence about the route, so the status, the
/// revalidation tag, and the body as sent all survive to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResponse {
    /// The status line's code, or `None` when the response carried none.
    pub status: Option<u16>,
    /// The `ETag` header, when the provider sent one.
    pub etag: Option<String>,
    /// The response body, undecoded.
    pub body: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("provider", &self.provider)
            .field("url", &self.url)
            .field("auth_header", &self.auth_header)
            .field("base", &self.base)
            .field("auth_value", &"[redacted]")
            .field("session_id", &self.session_id)
            .field("extra_headers", &"[redacted]")
            .finish()
    }
}

/// Where Codex CLI caches its OAuth tokens: `$CODEX_HOME/auth.json`, default
/// `~/.codex/auth.json`. Read, never written — refresh is Codex CLI's job.
fn codex_auth_path(
    cwd: &std::path::Path,
    read: &impl Fn(&str) -> Option<String>,
) -> Option<std::path::PathBuf> {
    if let Some(home) = read("CODEX_HOME") {
        return Some(cwd.join(home).join("auth.json"));
    }
    read("HOME").map(|home| cwd.join(home).join(".codex").join("auth.json"))
}

/// The access token and account id from Codex CLI's cache.
pub(crate) fn codex_cached_credentials(
    cwd: &std::path::Path,
    read: &impl Fn(&str) -> Option<String>,
) -> Result<(String, String), Error> {
    let path = codex_auth_path(cwd, read).ok_or(
        "the host environment has neither CODEX_HOME nor HOME to locate Codex credentials",
    )?;
    let text = std::fs::read_to_string(&path).map_err(|e| -> Error {
        format!(
            "could not read {}: {e}. Run `codex login` first",
            path.display()
        )
        .into()
    })?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let tokens = json.get("tokens").unwrap_or(&json);
    let token = tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{}: no tokens.access_token", path.display()))?;
    let account = tokens
        .get("account_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{}: no tokens.account_id", path.display()))?;
    Ok((token.to_string(), account.to_string()))
}

/// The cap OpenRouter puts on a session key. `--name` is arbitrary user input,
/// so the derivation below has to fit inside it rather than assume it does.
const SESSION_ID_MAX: usize = 256;

/// What distinguishes the spotter's key from the agent's ([`Endpoint::for_spotter`]).
const SPOTTER_SUFFIX: &str = "-spot";

/// What distinguishes the separate-model peek's key from the agent's
/// ([`Endpoint::for_triage`]). The longer of the two suffixes is what
/// [`sticky_session_id`] reserves room for.
const TRIAGE_SUFFIX: &str = "-triage";

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
    let budget = SESSION_ID_MAX - SPOTTER_SUFFIX.len().max(TRIAGE_SUFFIX.len()) - hash.len() - 1;
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

impl Endpoint {
    /// Construct an endpoint from credentials the host resolved. Every route and
    /// header shape stays provider-derived; `base` is the one input a caller
    /// supplies, and only a [`Provider::Custom`] connection has one to give.
    pub fn from_credentials(
        provider: Provider,
        token: String,
        account_id: Option<String>,
        base: Option<String>,
    ) -> Result<Self, Error> {
        match (provider, &base) {
            (Provider::Custom(_), None) => {
                return Err("a custom connection needs a base URL".into());
            }
            (provider, Some(_)) if !matches!(provider, Provider::Custom(_)) => {
                return Err(format!("{provider} has its own URLs and takes no base URL").into());
            }
            _ => {}
        }
        let mut endpoint = Self {
            provider,
            url: provider.messages_url(base.as_deref()),
            auth_header: provider.auth_header().to_string(),
            auth_value: AuthValue::from(provider.auth_header_value(&token)),
            session_id: None,
            extra_headers: Vec::new(),
            base,
        };
        match (provider, account_id) {
            (Provider::OpenAICodex, Some(account)) => endpoint
                .extra_headers
                .push(("ChatGPT-Account-ID".to_string(), account)),
            (Provider::OpenAICodex, None) => {
                return Err("openai-codex credentials need a ChatGPT account ID".into());
            }
            (_, Some(_)) => {
                return Err("only openai-codex credentials accept a ChatGPT account ID".into());
            }
            (_, None) => {}
        }
        Ok(endpoint)
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

    /// The endpoint the separate-model fork peek dials (docs/redaction-fork.md): the
    /// same provider and credentials, under a sticky key the agent lineage never
    /// shares. A triage model is a wholly separate cache lineage over the same
    /// transcript, so routing it with the agent's key would only pin two models to
    /// one route.
    pub fn for_triage(&self) -> Self {
        Self {
            session_id: self
                .session_id
                .as_ref()
                .map(|id| format!("{id}{TRIAGE_SUFFIX}")),
            ..self.clone()
        }
    }
}
