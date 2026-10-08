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

use crate::api::{AuthValue, Endpoint, Error, Provider, Reload, Reread, WireDialect};
use crate::distill::TriageModel;
use crate::env_table::EnvSnapshot;
use crate::secrets::Secrets;

/// The triage choice a session creator makes. Omission inherits the configured
/// default; `Shared` explicitly bypasses a configured separate default; `Separate`
/// carries the existing independent-client options.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TriageChoice {
    Shared,
    Separate {
        /// Names the connection, making `model` a provider-native id. Absent, the
        /// `model` string is the `connection/model` slug.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        model: String,
        #[serde(default)]
        thinking: bool,
        #[serde(default)]
        auto_choice: bool,
        #[serde(default)]
        effort: Option<String>,
    },
}

impl TriageChoice {
    /// The slug form: `model` is `connection/model`, or a native id resolved
    /// against `[defaults].model`'s connection.
    pub fn separate(
        model: String,
        thinking: bool,
        auto_choice: bool,
        effort: Option<String>,
    ) -> Self {
        Self::Separate {
            connection: None,
            model,
            thinking,
            auto_choice,
            effort,
        }
    }
}

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
    /// `[host]`: the web bind (`inference/host.rs`). Parsed here so the one
    /// file keeps its unknown-key refusal whole; read by `efference host`.
    #[serde(default)]
    host: HostConfig,
    /// `env_hook`: the argv a session's launch environment is established from
    /// (docs/env.md). A program and its arguments, invoked directly — shell
    /// syntax needs an explicit shell in the array, and the harness neither
    /// discovers one nor chooses login/interactive flags. Absent means a
    /// session inherits the host environment, which is what a session does
    /// today; a hook is opt-in because reconstructing a user's shell
    /// environment is a decision only the user can make.
    #[serde(default)]
    env_hook: Option<Vec<String>>,
}

impl InferenceConfig {
    /// Parse the host config text. Unknown keys are errors: the file has one
    /// job, and a misspelled `[connections.x]` field is a silent unavailable
    /// connection otherwise.
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        let config: Self =
            toml_edit::de::from_str(text).map_err(|error| Error::from(error.to_string()))?;
        if config.env_hook.as_ref().is_some_and(|argv| argv.is_empty()) {
            // An empty argv names no program to run. Reading it as "no hook"
            // would make a typo look like a deliberate choice to inherit the
            // host environment, which is the opposite of what the user wrote.
            return Err("env_hook = [] names no program to run; omit the key to \
                        inherit the host environment"
                .to_string()
                .into());
        }
        Ok(config)
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    spotter: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ConnectionConfig {
    /// A provider this crate knows by name. Mutually exclusive with [`Self::dialect`]:
    /// naming a provider and describing one are two ways of saying the same thing
    /// ([`resolve_declaration`]).
    #[serde(default)]
    provider: Option<String>,
    auth: AuthConfig,
    /// The wire dialect — `"messages"` or `"responses"` — which describes an
    /// endpoint this crate knows no name for (docs/transport.md). Mutually
    /// exclusive with [`Self::provider`], whose every value derives its own
    /// dialect, so a second spelling of it could only disagree.
    #[serde(default)]
    dialect: Option<String>,
    /// The endpoint's base URL — the path the dialect's route hangs off, as in
    /// `https://api.example.com/v1`. Required beside a `dialect` and refused
    /// beside a `provider`, which owns its URLs.
    #[serde(default)]
    base_url: Option<String>,
    /// The model this connection's sessions use as their **spotter** when nothing
    /// else names one (docs/permissions.md) — the cheap model beside the main
    /// one. It answers for this connection alone, which is what a per-connection
    /// answer is for: an account's cheap band is its own, and the arm's built-in
    /// default is a guess about a catalog the harness does not compile.
    ///
    /// It is a fallback, not a selection: an explicit `--spotter-model` and
    /// `[defaults].spotter` both come first (docs/operating.md).
    #[serde(default)]
    spotter_model: Option<String>,
    /// Kept in the catalog with its credential source intact, never a
    /// candidate for a session (docs/operating.md). `load_credential` is not
    /// even called: the connection's credential is unconditionally
    /// unavailable, by the same mechanism as a credential that fails to load.
    #[serde(default)]
    disabled: bool,
}

/// The location of an inference credential, never its value.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum AuthConfig {
    Env {
        key: String,
    },
    CodexCache,
    /// `auth = { kind = "oauth" }`: a grant efference obtained and maintains,
    /// in the secret store under `oauth:<name>` (docs/secrets.md). Names the
    /// mechanism as the other kinds do; which issuer is the *provider's* to
    /// say, so no issuer is configured here.
    ///
    /// Distinct from `keychain`, whose contract is "the operator placed a
    /// secret and we read it": this one is obtained by a login flow, rewritten
    /// on refresh, and carries an issuer and an expiry.
    Oauth {
        name: Option<String>,
    },
    /// `auth = { kind = "keychain" }`: the OS secret store (docs/secrets.md),
    /// item `inference:<name>`, `name` defaulting to the connection's.
    Keychain {
        name: Option<String>,
    },
}

/// A selected connection plus the provider-native model ID sent upstream.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// One connection a host can disclose to an inference-options client. It reports
/// configuration status only; it never carries an endpoint or credential source.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConnectionInventory {
    pub name: String,
    pub provider: Provider,
    pub available: bool,
}

/// The ready/unavailable host connection catalog. It intentionally has no Debug
/// implementation: its ready entries retain resolved credential material.
pub struct InferenceRegistry {
    connections: BTreeMap<String, Connection>,
    defaults: ResolvedDefaults,
    /// Every environment variable a connection reads its credential from
    /// (`auth = { kind = "env", key = … }`), disabled connections included: the
    /// host's credentials, which sessions strip from their tools' environment
    /// by default (docs/env.md).
    credential_variables: BTreeSet<String>,
    /// Past explicit model picks (docs/transport.md): the store behind the
    /// omitted-branch default and the recent menus. Opened from the config
    /// file's own directory, so a test config names no store.
    history: crate::history::History,
    /// The configured `env_hook`, retained as written (docs/env.md). User-wide
    /// because the command that reconstructs an operator's shell environment is
    /// a property of the operator, not of any one project; it travels with the
    /// registry so session creation reads one user configuration rather than
    /// re-reading the file, and sits beside the credential-variable set because
    /// both describe what a host carries into a session's environment.
    env_hook: Option<Vec<String>>,
}

struct Connection {
    provider: Provider,
    credential: Result<Credential, String>,
    /// The connection's own spotter answer, or `None` to leave the arm's default
    /// in place (`ConnectionConfig::spotter_model`).
    spotter_model: Option<String>,
    /// Turned off in the config rather than failing to load. Kept apart from
    /// the credential's `Err` because the two are different facts: this one the
    /// config declares about itself, and a host refuses to start over the other
    /// (docs/operating.md).
    disabled: bool,
}

struct Credential {
    endpoint: Endpoint,
    secret_values: Vec<(String, String)>,
}

#[derive(Default)]
struct ResolvedDefaults {
    spotter: Option<ModelReference>,
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
    /// The host's credential variables ([`InferenceRegistry`]), for the
    /// session's env table to strip (docs/env.md).
    credential_variables: Vec<String>,
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
        let mut registry = Self::from_config(config, host_cwd, host_env, secrets)?;
        // The state database lives beside the config file that named it
        // (`~/.efference/state.db`), so a relocated config carries its history
        // along rather than keying it off wherever the host happened to start.
        if let Some(dir) = path.parent() {
            registry.history = crate::history::History::at(dir.join("state.db"));
        }
        Ok(registry)
    }

    /// Point this registry at a history store, for the recent ranks in the model catalog and
    /// for tests that seed past picks. Production registries open the store
    /// beside their config file in [`load_at`](Self::load_at).
    pub fn with_history(mut self, history: crate::history::History) -> Self {
        self.history = history;
        self
    }

    pub fn from_config(
        config: InferenceConfig,
        host_cwd: &Path,
        host_env: EnvSnapshot,
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
            if let AuthConfig::Env { key } = &config.auth {
                credential_variables.insert(key.clone());
            }
            let (provider, base) = resolve_declaration(&name, &config)?;
            let credential = if config.disabled {
                Err(format!("connection {name:?} is disabled"))
            } else {
                load_credential(
                    &name,
                    provider,
                    base,
                    config.auth,
                    host_cwd,
                    &host_env,
                    secrets,
                )
            };
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
            history: crate::history::History::disabled(),
            env_hook: config.env_hook,
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

    pub fn empty() -> Self {
        Self {
            connections: BTreeMap::new(),
            defaults: ResolvedDefaults::default(),
            credential_variables: BTreeSet::new(),
            history: crate::history::History::disabled(),
            env_hook: None,
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
            &options::SessionOptions {
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
    pub fn history(&self) -> &crate::history::History {
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
        Err(format!(
            "unqualified {role} model {value:?} needs a connection prefix; write it as <connection>/{value}"
        )
        .into())
    }
}

impl SessionInference {
    /// The host's credential variables, for the session's env table to strip
    /// from its tools' environment (docs/env.md).
    pub fn credential_variables(&self) -> &[String] {
        &self.credential_variables
    }

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

/// The provider a connection declares, plus the base URL that goes with it.
///
/// A connection either **names** a provider this crate knows or **describes** one
/// it does not, and the two spellings are exclusive: a named provider derives its
/// own dialect and owns its URLs, so a `dialect` beside it could only agree
/// redundantly or disagree. There is no third spelling naming the describing case
/// — a `provider = "custom"` line would carry nothing the `dialect` and `base-url`
/// beside it do not already say (docs/transport.md).
///
/// Both halves of the describing case are required, because neither has a default
/// that would be safe to invent, and requiring them here keeps a half-declared
/// connection from loading at all rather than failing later at a request.
fn resolve_declaration(
    name: &str,
    config: &ConnectionConfig,
) -> Result<(Provider, Option<String>), String> {
    let dialect = match (config.provider.as_deref(), config.dialect.as_deref()) {
        (Some(provider), Some(_)) => {
            let provider = Provider::from_name(provider).map_err(|error| error.to_string())?;
            return Err(format!(
                "connection {name:?}: {provider} derives its own dialect — drop the \
                 `dialect` line, or drop `provider` to describe an endpoint this \
                 build knows no name for (docs/transport.md)"
            ));
        }
        (Some(provider), None) => {
            let provider = Provider::from_name(provider).map_err(|error| error.to_string())?;
            if config.base_url.is_some() {
                return Err(format!(
                    "connection {name:?}: {provider} has its own URLs and takes no base-url"
                ));
            }
            return Ok((provider, None));
        }
        (None, Some(dialect)) => dialect,
        (None, None) => {
            return Err(format!(
                "connection {name:?}: name a `provider`, or give a `dialect` \
                 (`messages`, `responses`, or `systemone`) and a `base-url` to describe \
                 an endpoint this build knows no name for (docs/transport.md)"
            ));
        }
    };
    let dialect =
        WireDialect::from_name(dialect).map_err(|error| format!("connection {name:?}: {error}"))?;
    let base = config.base_url.clone().ok_or_else(|| {
        format!(
            "connection {name:?}: a `dialect` needs a `base-url` beside it, \
             such as https://api.example.com/v1"
        )
    })?;
    if !base.starts_with("https://") && !base.starts_with("http://") {
        return Err(format!(
            "connection {name:?}: base-url {base:?} is not an http(s) URL"
        ));
    }
    Ok((Provider::Custom(dialect), Some(base)))
}

fn load_credential(
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

/// `names` is the connections a default may name; `disabled` is those the
/// operator turned off, carried separately only so the error says which of the
/// two happened. The check reads the declared flag, never a credential's load
/// result: a config describes itself, and a credential that failed to load for
/// an outside reason is not a config error (docs/operating.md).
fn resolve_config_default(
    value: Option<String>,
    names: &BTreeSet<String>,
    disabled: &BTreeSet<String>,
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
        return Err(if disabled.contains(connection) {
            format!("[defaults].{role} selects connection {connection:?}, but it is disabled")
                .into()
        } else {
            let error: Error = format!(
                "[defaults].{role} selects connection {connection:?}, but it is not configured"
            )
            .into();
            error
        });
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

pub mod chatgpt;
mod host;
pub use host::HostConfig;
pub mod options;
pub use options::{
    Effective, EffectiveOptions, EffectiveTriage, Issue, IssueCode, ResolvedOptions,
    SelectionSource, SessionOptions, TEMPERATURE,
};

#[cfg(test)]
mod test;
