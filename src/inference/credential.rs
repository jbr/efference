//! Reading a connection's credential from its declared source (docs/transport.md).

use super::*;

pub(super) fn load_credential(
    name: &str,
    provider: Provider,
    base: Option<String>,
    auth: AuthConfig,
    host_cwd: &Path,
    host_env: &EnvSnapshot,
    secrets: &Arc<Secrets>,
) -> Result<Credential, String> {
    // Only a source that states its own expiry sets this; the rest are re-read
    // when a request is rejected (docs/transport.md).
    let mut renew_at = None;
    let (token, account, label, reload) = match auth {
        AuthConfig::Keychain { name: item } => {
            let item = crate::secrets::inference_name(item.as_deref().unwrap_or(name));
            let token = secrets
                .get(&item)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{} has no `{item}`", secrets.describe()))?;
            // The store is outside this process, so an operator who replaces the
            // item repairs a running host (docs/transport.md).
            let (store, key) = (secrets.clone(), item.clone());
            let reload: Option<Reload> = Some(crate::api::sync_reload(move || {
                match store.get(&key).map_err(|error| error.to_string())? {
                    Some(token) => Ok(provider.auth_header_value(&token)),
                    None => Err(format!("{} has no `{key}`", store.describe())),
                }
            }));
            (token, None, "keychain".to_string(), reload)
        }
        // No reloader: the value comes from the environment captured when the
        // host started, which nothing outside the process can change, so a
        // re-read would return the same bytes forever (docs/transport.md).
        AuthConfig::Env { key } => {
            let token = host_env
                .get(&key)
                .ok_or_else(|| format!("host environment has no {key}"))?
                .to_string();
            (token, None, key, None)
        }
        AuthConfig::Oauth { name: item } => {
            if provider != Provider::OpenAICodex {
                return Err(format!(
                    "auth kind oauth has no issuer for provider {provider}; only openai-codex \
                     has one today"
                ));
            }
            let item = item.unwrap_or_else(|| name.to_string());
            let grant = chatgpt::Grant::load(secrets, &item)?.ok_or_else(|| {
                format!("no ChatGPT login stored for {item:?}; run `efference setup` to log in")
            })?;
            // The grant is refreshed by the reloader below rather than here:
            // loading a registry is synchronous and happens in offline verbs
            // too, and a launch is not the place to discover the issuer is
            // unreachable. The first request past `renew_at` refreshes it
            // (`AuthValue::current`), and a host renews it idle
            // (docs/transport.md).
            let account = grant.account_id.clone();
            let token = grant.tokens.access_token().to_string();
            renew_at = grant.renew_at();
            let (store, connection) = (secrets.clone(), item.clone());
            let reload: Option<Reload> = Some(Box::new(move || {
                let (store, connection) = (store.clone(), connection.clone());
                Box::pin(async move {
                    // Re-read first: another host process, or `efference setup`,
                    // may have stored a fresh grant already, and refreshing
                    // would rotate a token that did not need it.
                    let stored = chatgpt::Grant::load(&store, &connection)?
                        .ok_or_else(|| format!("the stored grant for {connection:?} is gone"))?;
                    let grant = if stored.needs_refresh() {
                        chatgpt::refresh(&store, &connection, &stored).await?
                    } else {
                        stored
                    };
                    Ok(Reread {
                        value: provider.auth_header_value(grant.tokens.access_token()),
                        renew_at: grant.renew_at(),
                    })
                })
            }));
            (token, Some(account), format!("oauth:{item}"), reload)
        }
        AuthConfig::CodexCache => {
            if provider != Provider::OpenAICodex {
                return Err("auth kind codex-cache requires provider openai-codex".to_string());
            }
            let read = |name: &str| host_env.get(name).map(str::to_string);
            let (token, account) = crate::api::codex_cached_credentials(host_cwd, &read)
                .map_err(|error| format!("could not load Codex cache: {error}"))?;
            // Codex CLI rewrites the file on its own refresh, so re-reading it is
            // how a host outlives an expired token: the operator runs `codex
            // login` and the next attempt carries the new one. The account id is
            // not re-read — it rides `extra_headers` and is stable across a
            // refresh of the same login.
            let (cwd, env) = (host_cwd.to_path_buf(), host_env.clone());
            let reload: Option<Reload> = Some(crate::api::sync_reload(move || {
                let read = |name: &str| env.get(name).map(str::to_string);
                crate::api::codex_cached_credentials(&cwd, &read)
                    .map_err(|error| format!("could not re-read the Codex cache: {error}"))
                    .map(|(token, _)| provider.auth_header_value(&token))
            }));
            (token, Some(account), "Codex cache".to_string(), reload)
        }
    };
    let mut endpoint = Endpoint::from_credentials(provider, token.clone(), account, base)
        .map_err(|error| error.to_string())?;
    if let Some(reload) = reload {
        endpoint.auth_value =
            AuthValue::renewing(provider.auth_header_value(&token), renew_at, reload);
    }
    Ok(Credential {
        endpoint,
        secret_values: vec![(format!("{name} ({label})"), token)],
    })
}
