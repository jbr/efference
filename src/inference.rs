//! Host-owned inference connections and role selection (docs/transport.md).
//!
//! The user configuration ([`crate::user_config`]) names provider connections and
//! credential *sources*; resolved credentials live only in the host process. Session
//! creation resolves model selections through this registry before boot touches
//! project configuration or mints a session directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::api::{AuthValue, Endpoint, Error, Reload, RequestOptions, Reread};
use crate::env_table::EnvSnapshot;
use crate::platform::secrets::Secrets;
use crate::record::log::{Provider, WireDialect};
use crate::user_config::{ConnectionConfig, ModelSelection};

/// The triage choice a session creator makes. Omission inherits the configured
/// default; `Shared` explicitly bypasses a configured separate default; `Separate`
/// carries the existing independent-client options.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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

/// `--triage-model` and its two knobs, as the CLI collects them and as a host's
/// creation body spells them. Not [`crate::record::log::TriageModel`], the record of which
/// inference a session's triage ran on.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TriageModel {
    pub model: String,
    #[serde(default)]
    /// `--triage-thinking`: let the model reason (off by default; the reasoning is
    /// discarded with the fork's tail).
    pub thinking: bool,
    #[serde(default)]
    /// `--triage-auto-choice`: `tool_choice: auto` instead of forcing the triage tool.
    pub auto_choice: bool,
    #[serde(default)]
    /// `--triage-effort`: the reasoning level, in the model's own vocabulary
    /// (validated against its metadata like `--effort`). Implies thinking on: a
    /// model that can be turned down rather than off is the case this serves.
    pub effort: Option<String>,
}

const BUILTIN_CONNECTION_NAMES: [&str; 5] =
    ["anthropic", "openrouter", "openai", "openai-codex", "codex"];

/// The roles a hosted session was created with, for `GET /state`'s
/// `session_identity` and the boot report (docs/control-api.md). Display only:
/// upstream requests carry the native `model` alone.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct InferenceIdentity {
    pub main: ModelSelection,
    pub spotter: ModelSelection,
    pub triage: TriageIdentity,
}

/// Which client triages tool results: the main client (`shared`), or its own,
/// whose selection is then reported like the other roles. A shared triage reports
/// the main selection so two accounts on one model stay distinguishable.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct TriageIdentity {
    pub shared: bool,
    #[serde(flatten)]
    pub selection: ModelSelection,
}

/// One connection a host can disclose to an inference-options client. It reports
/// configuration status only; it never carries an endpoint or credential source.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    /// The secret store the credentials were read from (docs/secrets.md), handed to
    /// each session so its MCP grants come from the same one.
    secrets: Arc<Secrets>,
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
    /// The registry's secret store, which the session's MCP grants are read from
    /// and written to (docs/mcp-auth.md).
    secrets: Arc<Secrets>,
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

    /// The secret store this session's credentials came from, where its MCP grants
    /// live too (docs/secrets.md).
    pub fn secrets(&self) -> &Arc<Secrets> {
        &self.secrets
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
                 build knows no name for"
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
                 an endpoint this build knows no name for"
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
