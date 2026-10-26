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

use crate::api::{
    AuthValue, Endpoint, Error, Provider, Reload, RequestOptions, Reread, WireDialect,
};
use crate::env_table::EnvSnapshot;
use crate::secrets::Secrets;
use crate::triage::TriageModel;

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

/// Settings for every session on this machine: connections to model providers, role
/// defaults, the web interface, and how the terminal UI draws. It names where each
/// credential comes from, never the credential itself. `efference setup` writes it, and
/// a host reads it when it starts.
// Secret-free user configuration at `~/.efference/config.toml`. The `///` docs on it and the
// types below are the published user schema's hover text (docs/config-schemas.md). `[defaults]`,
// `[host]`, and `[tui]` carry none on their fields: Taplo shows a `$ref` target's description
// and ignores one beside it.
#[derive(Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "User")]
pub struct InferenceConfig {
    #[serde(default)]
    defaults: Defaults,
    /// Connections to model providers, by name. A session names its model as
    /// `<connection>/<model>`, as in `"anthropic/claude-sonnet-4-5"`.
    #[serde(default)]
    connections: BTreeMap<String, ConnectionConfig>,
    // `[host]`: the web bind (`inference/host.rs`). Parsed here so the one file keeps its
    // unknown-key refusal whole; read by `efference --host`.
    #[serde(default)]
    host: HostConfig,
    // `[tui]`: how a terminal client draws (`inference/tui.rs`), read by whichever process
    // draws a TUI, not by the host.
    #[serde(default)]
    tui: TuiConfig,
    /// A program and its arguments whose output is a session's starting environment, as
    /// in `["/bin/zsh", "-l", "-c", "env -0"]`. It runs directly, without a shell, in the
    /// session's project directory, and must print NUL-separated `KEY=value` records
    /// (what `env -0` writes); they replace the host's environment rather than adding to
    /// it. Omitted, sessions inherit the host's environment.
    // docs/env.md. A hook is opt-in because reconstructing a user's shell environment is a
    // decision only the user can make, and the harness neither discovers a shell nor chooses
    // login/interactive flags.
    #[serde(default)]
    env_hook: Option<Vec<String>>,
}

impl InferenceConfig {
    /// The user configuration under `home`, or `None` when there is no file.
    /// An unreadable or unparsable file is an error naming its path.
    pub(crate) fn read_user(home: &Path) -> Result<Option<Self>, Error> {
        let path = home.join(CONFIG_RELATIVE_PATH);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!("could not read {}: {error}", path.display()).into());
            }
        };
        Self::from_toml(&text)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display()).into())
    }

    /// The `[tui]` table as written, before `~/` expansion.
    pub fn tui(&self) -> &TuiConfig {
        &self.tui
    }

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

/// `[defaults]`: role choices for sessions that make none.
#[derive(Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Defaults {
    /// The spotter model, as `<connection>/<model>`, for sessions that name none. Unset, a
    /// session uses its main connection's `spotter-model`, then the provider's default.
    spotter: Option<String>,
}

/// A connection to a model provider: a `provider` efference knows by name, or a
/// `dialect` and `base-url` describing another endpoint.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[schemars(rename = "Connection")]
struct ConnectionConfig {
    /// A provider efference knows by name, which sets the endpoint, the credential's
    /// shape, and the wire format. Takes no `dialect` or `base-url`.
    // Mutually exclusive with `dialect`: naming a provider and describing one are two ways of
    // saying the same thing ([`resolve_declaration`]).
    #[serde(default)]
    #[schemars(schema_with = "Provider::name_schema")]
    provider: Option<String>,
    // Where the credential comes from. Omitted on a described connection, it sends none (a
    // local server); a named provider must have one. Its hover text is `AuthConfig`'s.
    #[serde(default)]
    auth: Option<AuthConfig>,
    /// The wire format of an endpoint efference has no name for. Requires `base-url`;
    /// takes no `provider`.
    // docs/transport.md. Mutually exclusive with `provider`, whose every value derives its own
    // dialect, so a second spelling of it could only disagree.
    #[serde(default)]
    #[schemars(schema_with = "WireDialect::name_schema")]
    dialect: Option<String>,
    /// The endpoint's base URL, as in `https://api.example.com/v1`. Required with
    /// `dialect`; refused with `provider`, which has its own.
    #[serde(default)]
    base_url: Option<String>,
    /// The spotter model for sessions whose main model is on this connection, when
    /// neither the session nor `[defaults].spotter` names one.
    // The cheap model beside the main one (docs/permissions.md). It answers for this
    // connection alone: an account's cheap band is its own, and the arm's built-in default is a
    // guess about a catalog the harness does not compile (docs/operating.md).
    #[serde(default)]
    spotter_model: Option<String>,
    /// Keep the connection written down without offering it to any session. Default
    /// `false`.
    // Kept in the catalog with its credential source intact (docs/operating.md).
    // `load_credential` is not even called: the credential is unconditionally unavailable, by
    // the same mechanism as a credential that fails to load.
    #[serde(default)]
    disabled: bool,
    /// Fields merged into every request body, for a vendor parameter with a fixed value,
    /// as in `{ parallel_tool_calls = false }`. Read on a `completions` connection only;
    /// a field efference writes itself is refused.
    // `RequestOptions`, docs/transport.md.
    #[serde(default)]
    body: Option<serde_json::Map<String, serde_json::Value>>,
    /// The request field that carries the output cap. Read on a `completions` connection
    /// only. Default `"max_tokens"`.
    #[serde(default)]
    #[schemars(schema_with = "crate::api::OutputCapField::name_schema")]
    output_cap_field: Option<String>,
}

/// Where the connection's credential comes from, by `kind`; never the credential
/// itself. Required with `provider`. On a described endpoint it may be omitted, and
/// then no credential is sent.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(rename = "Auth")]
enum AuthConfig {
    /// `{ kind = "env", key = "ANTHROPIC_API_KEY" }`: an environment variable in the
    /// host's environment.
    Env {
        /// The environment variable that holds the credential.
        key: String,
    },
    /// `{ kind = "codex-cache" }`: the Codex CLI's ChatGPT login. For `openai-codex`.
    CodexCache,
    /// `{ kind = "oauth" }`: a ChatGPT login made in `efference setup`, which keeps it in
    /// the OS secret store and refreshes it. For `openai-codex`.
    // A grant efference obtained and maintains, in the secret store under `oauth:<name>`
    // (docs/secrets.md). Names the mechanism as the other kinds do; which issuer is the
    // *provider's* to say, so no issuer is configured here. Distinct from `keychain`, whose
    // contract is "the operator placed a secret and we read it": this one is obtained by a login
    // flow, rewritten on refresh, and carries an issuer and an expiry.
    Oauth {
        /// The login's name in the secret store. Default: the connection's name.
        name: Option<String>,
    },
    /// `{ kind = "keychain" }`: a key in the OS secret store, which `efference setup`
    /// fills when a key is pasted.
    // Item `inference:<name>` (docs/secrets.md).
    Keychain {
        /// The key's name in the secret store. Default: the connection's name.
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
    /// The host's model catalog, when this registry serves one
    /// ([`Self::with_catalog`]): where the spotter default reads the newest model
    /// in a provider's cheap band (docs/model-catalog.md). `None` for the CLI and
    /// the offline verbs, which take the compiled-in default.
    catalog: Option<crate::catalog::Catalog>,
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
                 (`messages`, `responses`, `completions`, or `systemone`) and a `base-url` \
                 to describe \
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

/// The connection's request settings (`RequestOptions`). They describe a
/// Completions body, so on any other dialect they are refused rather than ignored:
/// a setting that loads and does nothing reads as one that works.
fn resolve_request_options(
    name: &str,
    provider: Provider,
    config: &ConnectionConfig,
) -> Result<RequestOptions, String> {
    if config.body.is_none() && config.output_cap_field.is_none() {
        return Ok(RequestOptions::default());
    }
    if provider != Provider::Custom(WireDialect::Completions) {
        return Err(format!(
            "connection {name:?}: `body` and `output-cap-field` are read only on a \
             `dialect = \"completions\"` connection"
        ));
    }
    let body = config.body.clone().unwrap_or_default();
    RequestOptions::new(body, config.output_cap_field.as_deref())
        .map_err(|error| format!("connection {name:?}: {error}"))
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
mod tui;
pub use tui::TuiConfig;
pub mod options;
pub use options::{
    Effective, EffectiveOptions, EffectiveTriage, Issue, IssueCode, ResolvedOptions,
    SelectionSource, SessionOptions, TEMPERATURE,
};

mod credential;
mod switch;
pub use switch::ResumeSwitch;
mod registry;
#[cfg(test)]
mod test;
use credential::load_credential;
