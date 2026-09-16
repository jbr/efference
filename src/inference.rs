//! Host-owned inference connections and role selection (docs/transport.md).
//!
//! The user configuration names provider connections and credential *sources*;
//! resolved credentials live only in the host process. Session creation resolves
//! model selections through this registry before boot touches project configuration
//! or mints a session directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::api::{AuthValue, Endpoint, Error, Provider, Reload};
use crate::distill::TriageModel;
use crate::env_table::EnvSnapshot;
use crate::secrets::Secrets;

/// Where the user configuration lives under `HOME`; `efference setup` writes
/// it and every host and offline verb reads it.
pub const CONFIG_RELATIVE_PATH: &str = ".efference/config.toml";
const BUILTIN_CONNECTION_NAMES: [&str; 5] =
    ["anthropic", "openrouter", "openai", "openai-codex", "codex"];

/// Secret-free user configuration at `~/.efference/config.toml`.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceConfig {
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    connections: BTreeMap<String, ConnectionConfig>,
}

impl InferenceConfig {
    /// Parse the host config text. Unknown keys are errors: the file has one
    /// job, and a misspelled `[connections.x]` field is a silent unavailable
    /// connection otherwise.
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        toml_edit::de::from_str(text).map_err(|error| error.to_string().into())
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    model: Option<String>,
    spotter: Option<String>,
    triage: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionConfig {
    provider: String,
    auth: AuthConfig,
}

/// The location of an inference credential, never its value.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum AuthConfig {
    Env {
        key: String,
    },
    CodexCache,
    /// `auth = { kind = "keychain" }`: the OS secret store (docs/secrets.md),
    /// item `inference:<name>`, `name` defaulting to the connection's.
    Keychain {
        name: Option<String>,
    },
}

/// A selected connection plus the provider-native model ID sent upstream.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ModelSelection {
    pub connection: String,
    pub provider: Provider,
    pub model: String,
}

/// The roles a hosted session was created with, for `GET /state`'s
/// `session_identity` and the boot report (docs/control-api.md). Display only:
/// upstream requests carry the native `model` alone.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct InferenceIdentity {
    pub main: ModelSelection,
    pub spotter: ModelSelection,
    pub triage: TriageIdentity,
}

/// Which client triages tool results: the main client (`shared`), or its own,
/// whose selection is then reported like the other roles. A shared triage reports
/// the main selection so two accounts on one model stay distinguishable.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TriageIdentity {
    pub shared: bool,
    #[serde(flatten)]
    pub selection: ModelSelection,
}

/// The ready/unavailable host connection catalog. It intentionally has no Debug
/// implementation: its ready entries retain resolved credential material.
pub struct InferenceRegistry {
    connections: BTreeMap<String, Connection>,
    defaults: ResolvedDefaults,
}

struct Connection {
    provider: Provider,
    credential: Result<Credential, String>,
}

struct Credential {
    endpoint: Endpoint,
    secret_values: Vec<(String, String)>,
}

#[derive(Default)]
struct ResolvedDefaults {
    model: Option<ModelReference>,
    spotter: Option<ModelReference>,
    triage: Option<ModelReference>,
}

#[derive(Clone)]
struct ModelReference {
    connection: String,
    model: String,
}

/// Resolved, session-local inference inputs. Each endpoint is cloned from the
/// host registry; its sticky key is attached by boot for this session and role.
pub struct SessionInference {
    main: ResolvedRole,
    spotter: ResolvedRole,
    triage: Option<ResolvedTriage>,
    secret_values: Vec<(String, String)>,
}

struct ResolvedRole {
    selection: ModelSelection,
    endpoint: Endpoint,
    secret_values: Vec<(String, String)>,
}

struct ResolvedTriage {
    role: ResolvedRole,
    options: TriageModel,
}

impl InferenceRegistry {
    /// Load the host's user configuration and credentials from the environment
    /// captured at host startup. A missing or malformed file is a launch error
    /// naming the path: with no connections there is nothing a session could use.
    pub fn load(
        host_cwd: &Path,
        host_env: EnvSnapshot,
        secrets: &Arc<Secrets>,
    ) -> Result<Self, Error> {
        let home = host_env
            .get("HOME")
            .ok_or("host environment has no HOME for ~/.efference/config.toml")?;
        Self::load_at(
            &Path::new(home).join(CONFIG_RELATIVE_PATH),
            host_cwd,
            host_env,
            secrets,
        )
    }

    /// The injected form: a host launch supplies its own path and environment
    /// (`load`), and tests supply a temporary one.
    pub fn load_at(
        path: &Path,
        host_cwd: &Path,
        host_env: EnvSnapshot,
        secrets: &Arc<Secrets>,
    ) -> Result<Self, Error> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "no inference config at {}: run `efference setup` to create it \
                     (docs/operating.md)",
                    path.display()
                )
                .into());
            }
            Err(error) => {
                return Err(format!(
                    "could not read inference config {}: {error}",
                    path.display()
                )
                .into());
            }
        };
        let config = InferenceConfig::from_toml(&text)
            .map_err(|error| format!("inference config {}: {error}", path.display()))?;
        Self::from_config(config, host_cwd, host_env, secrets)
    }

    pub fn from_config(
        config: InferenceConfig,
        host_cwd: &Path,
        host_env: EnvSnapshot,
        secrets: &Arc<Secrets>,
    ) -> Result<Self, Error> {
        let mut connections = BTreeMap::new();
        for (name, config) in config.connections {
            validate_connection_name(&name)?;
            let provider = Provider::from_name(&config.provider)?;
            let credential =
                load_credential(&name, provider, config.auth, host_cwd, &host_env, secrets);
            connections.insert(
                name,
                Connection {
                    provider,
                    credential,
                },
            );
        }
        let names = connections.keys().cloned().collect();
        let defaults = ResolvedDefaults {
            model: resolve_config_default(config.defaults.model, &names, "model")?,
            spotter: resolve_config_default(config.defaults.spotter, &names, "spotter")?,
            triage: resolve_config_default(config.defaults.triage, &names, "triage")?,
        };
        Ok(Self {
            connections,
            defaults,
        })
    }

    pub fn empty() -> Self {
        Self {
            connections: BTreeMap::new(),
            defaults: ResolvedDefaults::default(),
        }
    }

    /// Resolve all role selections before preparation starts. Main is required;
    /// an omitted spotter takes its configured default or the main connection's
    /// existing cheap-band default. Omitted triage shares the main client.
    pub fn resolve(
        &self,
        model: Option<&str>,
        spotter: Option<&str>,
        triage: Option<TriageModel>,
    ) -> Result<SessionInference, Error> {
        let main = self.resolve_role(model, self.defaults.model.as_ref(), "main")?;
        let spotter = match spotter {
            Some(value) => self.resolve_role(Some(value), None, "spotter")?,
            None => match self.defaults.spotter.as_ref() {
                Some(default) => self.resolve_reference(default, "spotter")?,
                None => self.resolve_reference(
                    &ModelReference {
                        connection: main.selection.connection.clone(),
                        model: main.endpoint.provider.default_spotter_model().to_string(),
                    },
                    "spotter",
                )?,
            },
        };
        let triage = match triage {
            Some(mut options) => {
                let role = self.resolve_role(
                    Some(&options.model),
                    self.defaults.triage.as_ref(),
                    "triage",
                )?;
                options.model = role.selection.model.clone();
                Some(ResolvedTriage { role, options })
            }
            None => match self.defaults.triage.as_ref() {
                Some(default) => {
                    let role = self.resolve_reference(default, "triage")?;
                    Some(ResolvedTriage {
                        options: TriageModel {
                            model: role.selection.model.clone(),
                            thinking: false,
                            auto_choice: false,
                            effort: None,
                        },
                        role,
                    })
                }
                None => None,
            },
        };
        if let Some(triage) = &triage
            && triage.role.endpoint.provider.dialect() != main.endpoint.provider.dialect()
        {
            return Err(format!(
                "triage {} ({}) cannot replay main {} ({}) because their wire dialects differ",
                triage.role.selection.model,
                triage.role.endpoint.provider.name(),
                main.selection.model,
                main.endpoint.provider.name(),
            )
            .into());
        }
        let mut secret_values = Vec::new();
        for role in [&main, &spotter] {
            secret_values.extend(role_secret_values(role));
        }
        if let Some(triage) = &triage {
            secret_values.extend(role_secret_values(&triage.role));
        }
        secret_values.sort();
        secret_values.dedup();
        Ok(SessionInference {
            main,
            spotter,
            triage,
            secret_values,
        })
    }

    fn resolve_role(
        &self,
        value: Option<&str>,
        default: Option<&ModelReference>,
        role: &str,
    ) -> Result<ResolvedRole, Error> {
        match value {
            Some(value) => self.resolve_reference(&self.parse_model(value, role)?, role),
            None => match default {
                Some(default) => self.resolve_reference(default, role),
                None => {
                    // The main role's config key and CLI flag are both spelled `model`.
                    let key = if role == "main" { "model" } else { role };
                    Err(format!(
                        "no {role} model configured: add [defaults].{key} to \
                         ~/.efference/config.toml or pass --{key}"
                    )
                    .into())
                }
            },
        }
    }

    fn parse_model(&self, value: &str, role: &str) -> Result<ModelReference, Error> {
        if value.is_empty() {
            return Err(format!("{role} model cannot be empty").into());
        }
        let (prefix, rest) = value.split_once('/').unwrap_or((value, ""));
        if self.connections.contains_key(prefix) || BUILTIN_CONNECTION_NAMES.contains(&prefix) {
            if rest.is_empty() {
                return Err(format!(
                    "{role} model {value:?} names a connection but no native model"
                )
                .into());
            }
            return Ok(ModelReference {
                connection: prefix.to_string(),
                model: rest.to_string(),
            });
        }
        if self.connections.contains_key(value) {
            return Err(format!(
                "{role} model {value:?} collides with a connection name; write it as <connection>/{value}"
            )
            .into());
        }
        let default = self.defaults.model.as_ref().ok_or_else(|| {
            format!(
                "unqualified {role} model {value:?} needs [defaults].model to choose a connection"
            )
        })?;
        Ok(ModelReference {
            connection: default.connection.clone(),
            model: value.to_string(),
        })
    }

    fn resolve_reference(
        &self,
        reference: &ModelReference,
        role: &str,
    ) -> Result<ResolvedRole, Error> {
        let connection = self.connections.get(&reference.connection).ok_or_else(|| {
            format!(
                "{role} model selects connection {:?}, but it is not configured in ~/.efference/config.toml",
                reference.connection
            )
        })?;
        let credential = connection.credential.as_ref().map_err(|error| {
            format!(
                "{role} model selects connection {:?}, but it is unavailable: {error}",
                reference.connection
            )
        })?;
        Ok(ResolvedRole {
            selection: ModelSelection {
                connection: reference.connection.clone(),
                provider: connection.provider,
                model: reference.model.clone(),
            },
            endpoint: credential.endpoint.clone(),
            secret_values: credential.secret_values.clone(),
        })
    }
}

impl SessionInference {
    pub fn main(&self) -> (&ModelSelection, Endpoint) {
        (&self.main.selection, self.main.endpoint.clone())
    }

    pub fn spotter(&self) -> (&ModelSelection, Endpoint) {
        (&self.spotter.selection, self.spotter.endpoint.clone())
    }

    /// `None` means the main client is deliberately shared for triage.
    pub fn triage(&self) -> Option<(&ModelSelection, Endpoint, &TriageModel)> {
        self.triage.as_ref().map(|triage| {
            (
                &triage.role.selection,
                triage.role.endpoint.clone(),
                &triage.options,
            )
        })
    }

    pub fn identity(&self) -> InferenceIdentity {
        let triage = match &self.triage {
            Some(triage) => TriageIdentity {
                shared: false,
                selection: triage.role.selection.clone(),
            },
            None => TriageIdentity {
                shared: true,
                selection: self.main.selection.clone(),
            },
        };
        InferenceIdentity {
            main: self.main.selection.clone(),
            spotter: self.spotter.selection.clone(),
            triage,
        }
    }

    /// Mask this session's credentials, now and after any re-read: a credential
    /// that expires mid-session is replaced in place (`AuthValue`), and the
    /// replacement has to be masked in the session that was protected from the
    /// old one (docs/secrets.md).
    pub fn absorb_secrets(&self, masker: &Arc<crate::env_table::Masker>) {
        masker.absorb_secret(&self.secret_values);
        for role in self.roles() {
            for (name, _) in &role.secret_values {
                role.endpoint.auth_value.mask_replacements_in(name, masker);
            }
        }
    }

    fn roles(&self) -> impl Iterator<Item = &ResolvedRole> {
        [Some(&self.main), Some(&self.spotter)]
            .into_iter()
            .chain([self.triage.as_ref().map(|t| &t.role)])
            .flatten()
    }
}

fn role_secret_values(role: &ResolvedRole) -> Vec<(String, String)> {
    role.secret_values.clone()
}

fn load_credential(
    name: &str,
    provider: Provider,
    auth: AuthConfig,
    host_cwd: &Path,
    host_env: &EnvSnapshot,
    secrets: &Arc<Secrets>,
) -> Result<Credential, String> {
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
            let reload: Option<Reload> = Some(Box::new(move || {
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
            let reload: Option<Reload> = Some(Box::new(move || {
                let read = |name: &str| env.get(name).map(str::to_string);
                crate::api::codex_cached_credentials(&cwd, &read)
                    .map_err(|error| format!("could not re-read the Codex cache: {error}"))
                    .map(|(token, _)| provider.auth_header_value(&token))
            }));
            (token, Some(account), "Codex cache".to_string(), reload)
        }
    };
    let mut endpoint = Endpoint::from_credentials(provider, token.clone(), account)
        .map_err(|error| error.to_string())?;
    if let Some(reload) = reload {
        endpoint.auth_value = AuthValue::reloadable(provider.auth_header_value(&token), reload);
    }
    Ok(Credential {
        endpoint,
        secret_values: vec![(format!("{name} ({label})"), token)],
    })
}

fn resolve_config_default(
    value: Option<String>,
    names: &BTreeSet<String>,
    role: &str,
) -> Result<Option<ModelReference>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some((connection, model)) = value.split_once('/') else {
        return Err(format!(
            "[defaults].{role} must be a qualified <connection>/<native-model>, got {value:?}"
        )
        .into());
    };
    if connection.is_empty() || model.is_empty() {
        return Err(format!("[defaults].{role} is not a qualified model: {value:?}").into());
    }
    if !names.contains(connection) {
        return Err(format!(
            "[defaults].{role} selects connection {connection:?}, but it is not configured"
        )
        .into());
    }
    Ok(Some(ModelReference {
        connection: connection.to_string(),
        model: model.to_string(),
    }))
}

fn validate_connection_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.contains('/') {
        return Err(format!("connection name {name:?} must be nonempty and contain no '/'").into());
    }
    Ok(())
}

#[cfg(test)]
mod test;
