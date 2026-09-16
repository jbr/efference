//! `efference setup` — the interactive first-run path that writes
//! `~/.efference/config.toml` (docs/operating.md).
//!
//! A new install has no host configuration and therefore no model to ask for
//! help, so this is the one flow that has to work with nothing behind it: a
//! sequence of terminal prompts that names provider connections, records where
//! each credential comes from, checks what it can check live, and writes the
//! secret-free file the host loads at launch (`src/inference.rs`).
//!
//! The flow is written against two seams so it can be exercised without a tty or
//! a network: [`Prompter`] is how it asks (the terminal implementation is
//! `setup/terminal.rs`; tests script answers), and [`Probe`] is how it looks
//! (`setup/probe.rs` dials the providers; tests answer from a table). The
//! outcome is a [`Draft`] that `setup/render.rs` folds into a TOML document,
//! appending to an existing file rather than replacing it.

use std::collections::BTreeMap;

use crate::api::Provider;

mod flow;
mod manage;
mod probe;
mod render;
pub mod terminal;

pub use flow::run;
pub use probe::{KeyReport, LiveProbe, ModelListing, ModelReport, Probe};
pub use render::{render, write};

/// The person declined to continue: Esc or Ctrl-C at any prompt. Nothing has
/// been written when this surfaces; the verb reports it and exits non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("setup cancelled; nothing was written")
    }
}

impl std::error::Error for Cancelled {}

/// One entry in a [`Prompter::select`] menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The line the person picks by.
    pub label: String,
    /// A dimmer second clause after the label, for the consequence of picking it.
    pub detail: Option<String>,
    /// When set, the entry is shown but cannot be picked, and this is why. A
    /// visible-but-refused option records that the path exists and what it waits
    /// on, which is more honest than hiding it.
    pub unavailable: Option<String>,
}

impl Choice {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            unavailable: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn unavailable(mut self, why: impl Into<String>) -> Self {
        self.unavailable = Some(why.into());
        self
    }
}

/// A line the flow says between questions. The kind carries the tone so a
/// terminal can color it and a test can assert on it without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Plain narration.
    Plain,
    /// Something checked out.
    Good,
    /// Something to know before going on; not a failure.
    Note,
    /// Something failed; the flow goes on only if the person says so.
    Bad,
}

/// How the flow asks. Every method that waits on the person returns
/// [`Cancelled`] when they back out, and the flow propagates it without writing.
pub trait Prompter {
    /// Say a line. Multi-line text is printed as given.
    fn say(&mut self, tone: Tone, text: &str);
    /// Pick one of `choices`; the index returned is never an unavailable entry.
    fn select(&mut self, question: &str, choices: &[Choice]) -> Result<usize, Cancelled>;
    /// Free text, prefilled with `default` (submitting empty takes the default).
    fn text(&mut self, question: &str, default: Option<&str>) -> Result<String, Cancelled>;
    /// Free text that is never echoed, logged, or kept past its use.
    fn secret(&mut self, question: &str) -> Result<String, Cancelled>;
    /// Yes or no, with `default` on a bare Enter.
    fn confirm(&mut self, question: &str, default: bool) -> Result<bool, Cancelled>;
}

/// Where a connection's credential comes from — the setup-side twin of the
/// `auth` table `src/inference.rs` parses. Never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSource {
    /// `auth = { kind = "env", key = "NAME" }`: read from the host's launch environment.
    Env { key: String },
    /// `auth = { kind = "codex-cache" }`: Codex CLI's login, read at host launch.
    CodexCache,
    /// `auth = { kind = "keychain" }`: the secret store's `inference:<name>`
    /// item, which setup wrote (docs/secrets.md).
    Keychain,
}

/// One connection the flow decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionDraft {
    pub provider: Provider,
    pub auth: AuthSource,
    /// The provider-native id sessions on this connection run by default.
    pub model: String,
    /// The provider-native id for the spotter rung.
    pub spotter: String,
}

/// Everything setup decided, before it becomes TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// New connections by name, in the order they were added. A name already in
    /// the existing file is refused at the prompt, so this never overwrites one.
    pub connections: Vec<(String, ConnectionDraft)>,
    /// The connection name `[defaults]` should point at, when setup is choosing
    /// them: absent when the existing file already has a default model and the
    /// person kept it.
    pub default_connection: Option<String>,
    /// Mutations to connections that already exist, in the order decided
    /// (`src/setup/manage.rs`). Empty on the first-run add path.
    pub edits: Vec<Change>,
}

/// One mutation the management flow makes to an existing document (render.rs
/// applies these against the parsed `DocumentMut`, after which `connections`/
/// `default_connection` above are appended as usual).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Rename a connection's table, and rewrite any `[defaults]` value that
    /// names it as `<old>/<model>` to `<new>/<model>`. A secret-store item move
    /// (only when the item name was implicit) happens beforehand, as a side
    /// effect at decision time, not represented here.
    Rename { from: String, to: String },
    /// Replace a connection's `auth` table outright.
    SetAuth { name: String, auth: AuthSource },
    /// Write or clear `disabled = true` on a connection.
    SetDisabled { name: String, disabled: bool },
    /// Remove a connection's table entirely.
    Delete { name: String },
    /// Set `[defaults].<role>` to a fully qualified `<connection>/<model>`, or
    /// clear it when `None`.
    SetDefaultRole {
        role: DefaultRole,
        value: Option<String>,
    },
}

/// Which `[defaults]` key a [`Change::SetDefaultRole`] targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultRole {
    Model,
    Spotter,
    Triage,
}

impl DefaultRole {
    pub(crate) fn key(self) -> &'static str {
        match self {
            DefaultRole::Model => "model",
            DefaultRole::Spotter => "spotter",
            DefaultRole::Triage => "triage",
        }
    }
}

impl std::fmt::Display for DefaultRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.key())
    }
}

/// Where an existing connection's credential comes from, as read back out of
/// the TOML (the setup-side twin of `src/inference.rs`'s `AuthConfig`, minus
/// the value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingAuth {
    Env {
        key: String,
    },
    CodexCache,
    /// `name` is the pinned item name, when the operator (or a previous
    /// rename) wrote one explicitly; `None` when it defaults to the
    /// connection's own name.
    Keychain {
        name: Option<String>,
    },
}

impl ExistingAuth {
    /// The secret-store item this auth reads, when it is keychain-backed.
    pub(crate) fn item_name(&self, connection: &str) -> Option<String> {
        match self {
            ExistingAuth::Keychain { name } => Some(crate::secrets::inference_name(
                name.as_deref().unwrap_or(connection),
            )),
            _ => None,
        }
    }
}

/// One connection already in an existing config file, enough for the
/// management list and its edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingConnection {
    pub provider: String,
    pub auth: ExistingAuth,
    pub disabled: bool,
}

/// What an existing config file already holds, read before the flow starts so
/// it can refuse name collisions, leave standing defaults alone, and draw the
/// management list. Parsed leniently: setup runs before anything else can, so
/// a file this build cannot fully read is still one it can append to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Existing {
    pub connections: BTreeMap<String, ExistingConnection>,
    pub default_model: Option<String>,
    pub default_spotter: Option<String>,
    pub default_triage: Option<String>,
}

impl Existing {
    /// Read the connections and defaults out of config text. Errors only when
    /// the text is not TOML at all; a missing table is simply empty, and a
    /// connection table this build does not recognize is skipped rather than
    /// refused — setup only needs enough to list and edit it, not to run it.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let doc: toml_edit::DocumentMut = text
            .parse()
            .map_err(|error: toml_edit::TomlError| error.to_string())?;
        let mut connections = BTreeMap::new();
        if let Some(table) = doc.get("connections").and_then(|item| item.as_table_like()) {
            for (name, item) in table.iter() {
                // Anything unreadable here is reported, never skipped. A
                // connection omitted from the list is one the operator cannot
                // repair — and repairing what the host rejects is what this
                // flow is for, so a shape this does not understand has to say
                // so rather than leave a shorter list that looks complete
                // (docs/operating.md).
                let where_ = |what: &str| format!("[connections.{name}] {what}");
                let connection = item
                    .as_table_like()
                    .ok_or_else(|| where_("is not a table"))?;
                let provider = connection
                    .get("provider")
                    .ok_or_else(|| where_("has no `provider`"))?
                    .as_str()
                    .ok_or_else(|| where_("has a non-string `provider`"))?
                    .to_string();
                let disabled = match connection.get("disabled") {
                    None => false,
                    Some(value) => value
                        .as_bool()
                        .ok_or_else(|| where_("has a non-boolean `disabled`"))?,
                };
                let auth = parse_auth(
                    connection
                        .get("auth")
                        .ok_or_else(|| where_("has no `auth`"))?,
                )
                .map_err(|why| where_(&why))?;
                connections.insert(
                    name.to_string(),
                    ExistingConnection {
                        provider,
                        auth,
                        disabled,
                    },
                );
            }
        }
        let defaults = doc.get("defaults").and_then(|item| item.as_table_like());
        let default_of = |key: &str| {
            defaults
                .and_then(|defaults| defaults.get(key))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        Ok(Self {
            connections,
            default_model: default_of("model"),
            default_spotter: default_of("spotter"),
            default_triage: default_of("triage"),
        })
    }

    /// The connection names `[defaults]` points at, for the three roles that
    /// name `connection`.
    pub(crate) fn defaults_naming<'a>(
        &'a self,
        connection: &'a str,
    ) -> impl Iterator<Item = DefaultRole> + 'a {
        [
            (DefaultRole::Model, &self.default_model),
            (DefaultRole::Spotter, &self.default_spotter),
            (DefaultRole::Triage, &self.default_triage),
        ]
        .into_iter()
        .filter_map(move |(role, value)| {
            let value = value.as_deref()?;
            let (name, _) = value.split_once('/')?;
            (name == connection).then_some(role)
        })
    }
}

/// `auth = { kind = "env", key = "X" }` and friends, as written by
/// `src/setup/render.rs` and read by `src/inference.rs`. `None` for a shape
/// this build does not recognize.
/// The `auth` table, or why it could not be read. An unrecognized `kind` is an
/// error rather than a skipped connection: the kinds grow (an OAuth grant is
/// one), and a newer file read by an older binary must say it does not
/// understand the connection instead of hiding it.
fn parse_auth(item: &toml_edit::Item) -> Result<ExistingAuth, String> {
    let table = item
        .as_table_like()
        .ok_or_else(|| "has a non-table `auth`".to_string())?;
    let kind = table
        .get("kind")
        .ok_or_else(|| "has an `auth` with no `kind`".to_string())?
        .as_str()
        .ok_or_else(|| "has a non-string `auth.kind`".to_string())?;
    let string = |field: &str| -> Result<String, String> {
        table
            .get(field)
            .ok_or_else(|| format!("has an `auth` of kind {kind:?} with no `{field}`"))?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("has a non-string `auth.{field}`"))
    };
    match kind {
        "env" => Ok(ExistingAuth::Env {
            key: string("key")?,
        }),
        "codex-cache" => Ok(ExistingAuth::CodexCache),
        "keychain" => Ok(ExistingAuth::Keychain {
            name: match table.get("name") {
                None => None,
                Some(_) => Some(string("name")?),
            },
        }),
        other => Err(format!(
            "has an `auth` of unrecognized kind {other:?}; this build understands \
             env, keychain, and codex-cache"
        )),
    }
}

/// The catalog the flow offers per provider: how to describe it, the variable
/// its key conventionally lives in, and a starting model when nothing live can
/// be listed.
pub(crate) struct ProviderCard {
    pub(crate) provider: Provider,
    pub(crate) label: &'static str,
    pub(crate) detail: &'static str,
    /// The conventional variable name, `None` for a provider whose credential is
    /// not an API key.
    pub(crate) env_key: Option<&'static str>,
    /// The main-model id to prefill when no live listing is available. The
    /// spotter prefill is `Provider::default_spotter_model`.
    pub(crate) model_hint: &'static str,
}

pub(crate) const PROVIDERS: [ProviderCard; 4] = [
    ProviderCard {
        provider: Provider::Anthropic,
        label: "Anthropic",
        detail: "an API key for api.anthropic.com",
        env_key: Some("ANTHROPIC_API_KEY"),
        model_hint: "claude-sonnet-4-5",
    },
    ProviderCard {
        provider: Provider::OpenRouter,
        label: "OpenRouter",
        detail: "one key, many models",
        env_key: Some("OPENROUTER_API_KEY"),
        model_hint: "anthropic/claude-sonnet-4-5",
    },
    ProviderCard {
        provider: Provider::OpenAI,
        label: "OpenAI",
        detail: "an API key for the platform API",
        env_key: Some("OPENAI_API_KEY"),
        model_hint: "gpt-5.6",
    },
    ProviderCard {
        provider: Provider::OpenAICodex,
        label: "ChatGPT subscription via Codex",
        detail: "the login Codex CLI keeps",
        env_key: None,
        model_hint: "gpt-5.6",
    },
];

#[cfg(test)]
mod test;
