//! The user configuration, `~/.efference/config.toml`: settings for every session on this
//! machine, as against the project's `efference.toml` ([`crate::config`])
//! (docs/config-schemas.md, docs/transport.md).
//!
//! It names provider connections and where each credential comes from, never the
//! credential. The host's registry resolves the connections and the defaults
//! ([`crate::inference`]); each other table is read by the process it concerns, through
//! the `load` on its type: `[host]`, `[tui]` and `[updates]`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::log::{Provider, WireDialect};

mod host;
pub use host::HostConfig;
mod tui;
pub use tui::{ImageMode, TuiConfig};
mod updates;
pub use updates::UpdatesConfig;

#[cfg(test)]
mod test;

/// Where the user configuration lives under `HOME`; `efference setup` writes
/// it and every host and offline verb reads it.
pub const CONFIG_RELATIVE_PATH: &str = ".efference/config.toml";

/// Settings for every session on this machine: connections to model providers, role
/// defaults, the web interface, how the terminal UI draws, where efference keeps the
/// secrets it writes, and whether it checks for newer versions. It names where each
/// credential comes from, never the credential itself. `efference setup` writes it, and
/// a host reads it when it starts.
// Secret-free user configuration at `~/.efference/config.toml`. The `///` docs on it and the
// types below are the published user schema's hover text (docs/config-schemas.md). `[defaults]`,
// `[host]`, `[tui]`, `[updates]`, and `[secrets]` carry none on their fields: Taplo shows a
// `$ref` target's description and ignores one beside it.
#[derive(Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "User")]
pub struct InferenceConfig {
    #[serde(default)]
    pub(crate) defaults: Defaults,
    /// Connections to model providers, by name. A session names its model as
    /// `<connection>/<model>`, as in `"anthropic/claude-sonnet-4-5"`.
    #[serde(default)]
    pub(crate) connections: BTreeMap<String, ConnectionConfig>,
    // `[host]`: the web bind (`user_config/host.rs`). Parsed here so the one file keeps its
    // unknown-key refusal whole; read by `efference --host`.
    #[serde(default)]
    host: HostConfig,
    // `[tui]`: how a terminal client draws (`user_config/tui.rs`), read by whichever process
    // draws a TUI, not by the host.
    #[serde(default)]
    tui: TuiConfig,
    // `[updates]`: whether a host checks for a newer efference (`user_config/updates.rs`),
    // read by `efference --host`. Its hover text is `UpdatesConfig`'s.
    #[serde(default)]
    updates: UpdatesConfig,
    /// A program and its arguments whose output is a session's starting environment, as
    /// in `["/bin/zsh", "-l", "-c", "env -0"]`. It runs directly, without a shell, in the
    /// session's project directory, and must print NUL-separated `KEY=value` records
    /// (what `env -0` writes); they replace the host's environment rather than adding to
    /// it. Omitted, sessions inherit the host's environment.
    // docs/env.md. A hook is opt-in because reconstructing a user's shell environment is a
    // decision only the user can make, and the harness neither discovers a shell nor chooses
    // login/interactive flags.
    #[serde(default)]
    pub(crate) env_hook: Option<Vec<String>>,
    // `[secrets]`: the command backend of the secret store (`secrets/commands.rs`,
    // docs/secrets.md), read by every process that opens the store. Its hover text is
    // `SecretCommands`'s.
    #[serde(default)]
    pub(crate) secrets: Option<crate::secrets::SecretCommands>,
}

impl InferenceConfig {
    /// The user configuration under `home`, or `None` when there is no file.
    /// An unreadable or unparsable file is an error naming its path.
    pub(crate) fn read_user(
        home: &Path,
    ) -> Result<Option<Self>, Box<dyn std::error::Error + Send + Sync>> {
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

    /// The `[secrets]` table of the user configuration under `home`, which names the
    /// secret store (docs/secrets.md); `None` when there is no file or no table. An
    /// unreadable or unparsable file is an error naming its path.
    pub fn secret_commands(
        home: &Path,
    ) -> Result<Option<crate::secrets::SecretCommands>, Box<dyn std::error::Error + Send + Sync>>
    {
        Ok(Self::read_user(home)?.and_then(|config| config.secrets))
    }

    /// [`secret_commands`](Self::secret_commands) under this process's home directory.
    pub fn user_secret_commands()
    -> Result<Option<crate::secrets::SecretCommands>, Box<dyn std::error::Error + Send + Sync>>
    {
        let home = std::env::home_dir()
            .ok_or("no home directory to read ~/.efference/config.toml under")?;
        Self::secret_commands(&home)
    }

    /// The `[tui]` table as written, before `~/` expansion.
    pub fn tui(&self) -> &TuiConfig {
        &self.tui
    }

    /// Parse the host config text. Unknown keys are errors: the file has one
    /// job, and a misspelled `[connections.x]` field is a silent unavailable
    /// connection otherwise.
    pub fn from_toml(text: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let config: Self = toml_edit::de::from_str(text).map_err(|error| error.to_string())?;
        if config.env_hook.as_ref().is_some_and(Vec::is_empty) {
            // An empty argv names no program to run. Reading it as "no hook"
            // would make a typo look like a deliberate choice to inherit the
            // host environment, which is the opposite of what the user wrote.
            return Err("env_hook = [] names no program to run; omit the key to \
                        inherit the host environment"
                .to_string()
                .into());
        }
        if let Some(commands) = &config.secrets {
            commands.validate()?;
        }
        Ok(config)
    }
}

/// `[defaults]`: role choices for sessions that make none.
#[derive(Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Defaults {
    /// The spotter model, as `<connection>/<model>`, for sessions that name none. Unset, a
    /// session uses its main connection's `spotter-model`, then the provider's default.
    pub(crate) spotter: Option<String>,
}

/// A connection to a model provider: a `provider` efference knows by name, or a
/// `dialect` and `base-url` describing another endpoint.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[schemars(rename = "Connection")]
pub(crate) struct ConnectionConfig {
    /// A provider efference knows by name, which sets the endpoint, the credential's
    /// shape, and the wire format. Takes no `dialect` or `base-url`.
    // Mutually exclusive with `dialect`: naming a provider and describing one are two ways of
    // saying the same thing (`inference::resolve_declaration`).
    #[serde(default)]
    #[schemars(schema_with = "Provider::name_schema")]
    pub(crate) provider: Option<String>,
    // Where the credential comes from. Omitted on a described connection, it sends none (a
    // local server); a named provider must have one. Its hover text is `AuthConfig`'s.
    #[serde(default)]
    pub(crate) auth: Option<AuthConfig>,
    /// The wire format of an endpoint efference has no name for. Requires `base-url`;
    /// takes no `provider`.
    // docs/transport.md. Mutually exclusive with `provider`, whose every value derives its own
    // dialect, so a second spelling of it could only disagree.
    #[serde(default)]
    #[schemars(schema_with = "WireDialect::name_schema")]
    pub(crate) dialect: Option<String>,
    /// The endpoint's base URL, as in `https://api.example.com/v1`. Required with
    /// `dialect`; refused with `provider`, which has its own.
    #[serde(default)]
    pub(crate) base_url: Option<String>,
    /// The spotter model for sessions whose main model is on this connection, when
    /// neither the session nor `[defaults].spotter` names one.
    // The cheap model beside the main one (docs/permissions.md). It answers for this
    // connection alone: an account's cheap band is its own, and the arm's built-in default is a
    // guess about a catalog the harness does not compile (docs/operating.md).
    #[serde(default)]
    pub(crate) spotter_model: Option<String>,
    /// Keep the connection written down without offering it to any session. Default
    /// `false`.
    // Kept in the catalog with its credential source intact (docs/operating.md).
    // `inference::load_credential` is not even called: the credential is unconditionally
    // unavailable, by the same mechanism as a credential that fails to load.
    #[serde(default)]
    pub(crate) disabled: bool,
    /// Fields merged into every request body, for a vendor parameter with a fixed value,
    /// as in `{ parallel_tool_calls = false }`. Read on a `completions` connection only;
    /// a field efference writes itself is refused.
    // `RequestOptions`, docs/transport.md.
    #[serde(default)]
    pub(crate) body: Option<serde_json::Map<String, serde_json::Value>>,
    /// The request field that carries the output cap. Read on a `completions` connection
    /// only. Default `"max_tokens"`.
    #[serde(default)]
    #[schemars(schema_with = "OutputCapField::name_schema")]
    pub(crate) output_cap_field: Option<String>,
}

/// Where the connection's credential comes from, by `kind`; never the credential
/// itself. Required with `provider`. On a described endpoint it may be omitted, and
/// then no credential is sent.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(rename = "Auth")]
pub(crate) enum AuthConfig {
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ModelSelection {
    pub connection: String,
    pub provider: Provider,
    pub model: String,
}

/// The output cap's spelling: `max_tokens` on most servers, `max_completion_tokens`
/// on OpenAI's own Chat Completions endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputCapField {
    #[default]
    MaxTokens,
    MaxCompletionTokens,
}

impl OutputCapField {
    /// The published user schema for a connection's `output-cap-field`: the spellings
    /// [`RequestOptions::new`](crate::api::RequestOptions::new) accepts
    /// (docs/config-schemas.md).
    pub fn name_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        crate::config_schemas::documented_enum([
            ("max_tokens", "The default, which most servers read."),
            (
                "max_completion_tokens",
                "OpenAI's own Chat Completions endpoint.",
            ),
        ])
    }
}
