//! Resolving connections and role selections against the host's configuration
//! (docs/transport.md).

use super::*;
use crate::config::user_config::{AuthConfig, CONFIG_RELATIVE_PATH, InferenceConfig};

impl InferenceRegistry {
    /// Load the host's user configuration and credentials from the environment
    /// captured at host startup. A missing or malformed file is a launch error
    /// naming the path: with no connections there is nothing a session could use.
    pub fn load(
        host_cwd: &Path,
        host_env: &EnvSnapshot,
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
        host_env: &EnvSnapshot,
        secrets: &Arc<Secrets>,
    ) -> Result<Self, Error> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "no inference config at {}: run `efference setup` to create it",
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
        let mut registry = Self::from_config(config, host_cwd, host_env, secrets)?;
        // The state database lives beside the config file that named it
        // (`~/.efference/state.db`), so a relocated config carries its history
        // along rather than keying it off wherever the host happened to start.
        if let Some(dir) = path.parent() {
            registry.history = crate::config::history::History::at(dir.join("state.db"));
        }
        Ok(registry)
    }

    /// Point this registry at a history store, for the recent ranks in the model catalog and
    /// for tests that seed past picks. Production registries open the store
    /// beside their config file in [`load_at`](Self::load_at).
    pub fn with_history(mut self, history: crate::config::history::History) -> Self {
        self.history = history;
        self
    }

    /// Read the spotter default from the host's catalog (docs/model-catalog.md).
    pub fn with_catalog(mut self, catalog: catalog::Catalog) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// The spotter a session on `main` gets when nothing names one: the newest
    /// model in the provider's cheap band per its cached listing, else the
    /// compiled-in default (`Provider::newest_spotter_model`). A listing not yet
    /// fetched answers nothing here; the picker's read fetches it.
    pub(crate) fn default_spotter(&self, main: &ModelSelection) -> Option<String> {
        self.catalog
            .as_ref()
            .and_then(|catalog| {
                catalog
                    .snapshot(
                        &main.connection,
                        main.provider,
                        crate::config::history::now(),
                    )
                    .rows
            })
            .and_then(|rows| main.provider.newest_spotter_model(&rows))
            .or_else(|| main.provider.default_spotter_model().map(str::to_string))
    }

    pub fn from_config(
        config: InferenceConfig,
        host_cwd: &Path,
        host_env: &EnvSnapshot,
        secrets: &Arc<Secrets>,
    ) -> Result<Self, Error> {
        let mut connections = BTreeMap::new();
        // A disabled connection stays keyed in `connections` (its models and
        // credential source are still what the operator wrote) but is left out
        // of `enabled_names`, so a [defaults] entry naming it fails here exactly
        // as one naming a connection that was never configured does, rather
        // than waiting for a session to resolve that role (docs/operating.md).
        let mut enabled_names = BTreeSet::new();
        let mut disabled_names = BTreeSet::new();
        let mut credential_variables = BTreeSet::new();
        for (name, config) in config.connections {
            validate_connection_name(&name)?;
            if let Some(AuthConfig::Env { key }) = &config.auth {
                credential_variables.insert(key.clone());
            }
            let (provider, base) = resolve_declaration(&name, &config)?;
            let request = resolve_request_options(&name, provider, &config)?;
            let credential = match (config.disabled, config.auth, base) {
                (true, _, _) => Err(format!("connection {name:?} is disabled")),
                (false, Some(auth), base) => {
                    load_credential(&name, provider, base, auth, host_cwd, host_env, secrets)
                }
                // A described connection with no `auth` line sends no credential.
                (false, None, Some(base)) => Ok(Credential {
                    endpoint: Endpoint::without_credential(provider, base),
                    secret_values: Vec::new(),
                }),
                (false, None, None) => {
                    return Err(format!(
                        "connection {name:?}: {provider} needs an `auth` line; only a \
                         described connection (`dialect` and `base-url`) may omit it, \
                         which sends no credential"
                    )
                    .into());
                }
            }
            .map(|mut credential| {
                credential.endpoint.request = request;
                credential
            });
            if config.disabled {
                disabled_names.insert(name.clone());
            } else {
                enabled_names.insert(name.clone());
            }
            connections.insert(
                name,
                Connection {
                    provider,
                    credential,
                    spotter_model: config.spotter_model,
                    disabled: config.disabled,
                },
            );
        }
        let defaults = ResolvedDefaults {
            spotter: resolve_config_default(
                config.defaults.spotter,
                &enabled_names,
                &disabled_names,
                "spotter",
            )?,
        };
        Ok(Self {
            connections,
            defaults,
            credential_variables,
            history: crate::config::history::History::disabled(),
            catalog: None,
            env_hook: config.env_hook,
            secrets: Arc::clone(secrets),
        })
    }

    /// Connections that are configured, not disabled, and whose credential did
    /// not load, with the reason each gave. Empty is the ordinary case; a host
    /// refuses to start otherwise (docs/operating.md), because a host that
    /// starts and then fails every session touching that provider hides the
    /// problem at the moment someone could fix it.
    pub fn unavailable(&self) -> Vec<(&str, &str)> {
        self.connections
            .iter()
            .filter(|(_, connection)| !connection.disabled)
            .filter_map(|(name, connection)| {
                let error = connection.credential.as_ref().err()?;
                Some((name.as_str(), error.as_str()))
            })
            .collect()
    }

    /// The auth values that renew on a deadline, one per connection, for a
    /// host to keep renewed while idle ([`AuthValue::keep_renewed`]).
    pub fn renewing(&self) -> Vec<(&str, AuthValue)> {
        self.connections
            .iter()
            .filter(|(_, connection)| !connection.disabled)
            .filter_map(|(name, connection)| {
                let auth = &connection.credential.as_ref().ok()?.endpoint.auth_value;
                auth.renew_at()?;
                Some((name.as_str(), auth.clone()))
            })
            .collect()
    }

    /// The provider and a ready endpoint for one named connection, credential
    /// attached — the same resolution a session's roles get, for a caller that
    /// wants a single connection rather than a set of roles. The registry holds
    /// credentials in process and never discloses them, so this hands out the
    /// endpoint to use rather than the secret behind it.
    ///
    /// The three refusals are the three ways a configured connection is not
    /// usable, each naming the remedy: no such name (the configured ones are
    /// listed), turned off in the configuration, or a credential that failed to
    /// load with the reason its source gave.
    pub fn endpoint(&self, name: &str) -> Result<(Provider, Endpoint), Error> {
        let connection = self.connections.get(name).ok_or_else(|| -> Error {
            let configured = self
                .connections
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if configured.is_empty() {
                format!("no connection named {name:?}: the configuration declares none").into()
            } else {
                format!("no connection named {name:?}: configured connections are {configured}")
                    .into()
            }
        })?;
        if connection.disabled {
            return Err(format!("connection {name:?} is disabled in the configuration").into());
        }
        let credential = connection.credential.as_ref().map_err(|error| -> Error {
            format!("connection {name:?} is unusable: {error}").into()
        })?;
        Ok((connection.provider, credential.endpoint.clone()))
    }

    /// The configured connection rows a host may disclose. `BTreeMap` iteration
    /// keeps the response sorted by connection name.
    pub fn inventory(&self) -> Vec<ConnectionInventory> {
        self.connections
            .iter()
            .map(|(name, connection)| ConnectionInventory {
                name: name.clone(),
                provider: connection.provider,
                available: !connection.disabled && connection.credential.is_ok(),
            })
            .collect()
    }

    /// No connections, and a secret store in process memory.
    pub fn empty() -> Self {
        Self {
            connections: BTreeMap::new(),
            defaults: ResolvedDefaults::default(),
            credential_variables: BTreeSet::new(),
            history: crate::config::history::History::disabled(),
            catalog: None,
            env_hook: None,
            secrets: Arc::new(Secrets::in_memory()),
        }
    }

    /// Resolve a session's roles from the slug spellings the CLI and offline
    /// verbs carry, then attach credentials. One call through the shared flow
    /// (`options.rs`), so these surfaces and the control routes cannot drift.
    /// Offline verbs name no working root, so no history scope applies: an
    /// omitted model there is a refusal, never a remembered pick from some
    /// other directory.
    pub fn resolve(
        &self,
        model: Option<&str>,
        spotter: Option<&str>,
        triage: Option<TriageChoice>,
    ) -> Result<SessionInference, Error> {
        self.prepare(self.resolve_options(
            &SessionOptions {
                model: model.map(str::to_string),
                spotter_model: spotter.map(str::to_string),
                triage,
                ..Default::default()
            },
            None,
            None,
        ))
    }

    /// The history store behind the omitted-branch default and the recent
    /// menus. The host reads it for preview and creation; the write path
    /// records explicit picks through it after a session starts.
    pub fn history(&self) -> &crate::config::history::History {
        &self.history
    }

    /// The configured `env_hook` argv, or `None` when the user declared none and
    /// a session's launch environment is the host environment (docs/env.md).
    pub fn env_hook(&self) -> Option<&[String]> {
        self.env_hook.as_deref()
    }

    /// Split a `connection/model` slug. A native id may itself contain a slash,
    /// so a prefix only names a connection when one by that name exists; the
    /// unambiguous spelling is a separate `connection` field (`options.rs`).
    /// A bare id with no connection prefix is refused: with no default model
    /// there is nothing to resolve it against (docs/transport.md).
    pub(super) fn parse_model(&self, value: &str, role: &str) -> Result<ModelReference, Error> {
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
        Err(format!(
            "unqualified {role} model {value:?} needs a connection prefix; write it as <connection>/{value}"
        )
        .into())
    }
}
