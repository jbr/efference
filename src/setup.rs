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

use crate::api::{Provider, WireDialect};

mod defaults;
mod flow;
mod manage;
mod probe;
mod render;
pub mod terminal;

pub use flow::run;
pub use probe::{ChatGptLogin, KeyReport, LiveProbe, ModelListing, ModelReport, Probe, Target};
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
    /// `auth = { kind = "oauth" }`: the secret store's `oauth:<name>` item,
    /// holding the grant setup obtained by logging in (docs/secrets.md). The
    /// item name is the connection's, so the kind carries no field.
    Oauth,
}

/// One connection the flow decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionDraft {
    pub provider: Provider,
    /// The base URL for a described endpoint (`Provider::Custom`), `None` for a
    /// named provider that owns its URLs (docs/transport.md).
    pub base_url: Option<String>,
    pub auth: AuthSource,
    /// `disabled = true` on the appended table. A connection can be disabled in
    /// the management flow before the run that adds it has written anything.
    pub disabled: bool,
}

/// Everything setup decided, before it becomes TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// New connections by name, in the order they were added. A name already in
    /// the existing file is refused at the prompt, so this never overwrites one.
    pub connections: Vec<(String, ConnectionDraft)>,
    /// Mutations to connections that already exist, in the order decided
    /// (`src/setup/manage.rs`). Empty on the first-run add path. Queued through
    /// [`Draft::record`], which is what keeps every entry naming a connection
    /// the file actually has.
    pub edits: Vec<Change>,
}

impl Draft {
    /// The connections this run added, in the shape the rest of setup reads an
    /// existing file's in — so the defaults editor offers a connection decided
    /// moments ago exactly as it offers one the file already had.
    pub fn added_connections(&self) -> BTreeMap<String, ExistingConnection> {
        self.connections
            .iter()
            .map(|(name, connection)| {
                (
                    name.clone(),
                    ExistingConnection {
                        provider: connection.provider.name().to_string(),
                        base_url: connection.base_url.clone(),
                        auth: match &connection.auth {
                            AuthSource::Env { key } => ExistingAuth::Env { key: key.clone() },
                            AuthSource::CodexCache => ExistingAuth::CodexCache,
                            AuthSource::Keychain => ExistingAuth::Keychain { name: None },
                            AuthSource::Oauth => ExistingAuth::Oauth { name: None },
                        },
                        disabled: connection.disabled,
                    },
                )
            })
            .collect()
    }

    /// Record one management decision (docs/operating.md).
    ///
    /// A decision about a connection this same run added is not an edit to the
    /// file: the connection is not there yet, and `render` applies `edits`
    /// against the document before appending `connections`, so queueing one
    /// would fail the write and lose the run. Such a decision is applied to the
    /// pending [`ConnectionDraft`] instead, and only the rest become [`Change`]s.
    pub fn record(&mut self, change: Change) {
        let Some(target) = change.connection() else {
            self.edits.push(change);
            return;
        };
        let Some(index) = self.connections.iter().position(|(name, _)| name == target) else {
            self.edits.push(change);
            return;
        };
        match change {
            Change::Rename { from, to } => {
                self.connections[index].0 = to.clone();
                self.requalify_defaults(&from, Some(&to));
            }
            Change::SetAuth { auth, .. } => self.connections[index].1.auth = auth,
            Change::SetDisabled { disabled, .. } => {
                self.connections[index].1.disabled = disabled;
            }
            Change::Delete { name } => {
                self.connections.remove(index);
                self.requalify_defaults(&name, None);
            }
            Change::SetDefaultRole { .. } => unreachable!("no connection target"),
        }
    }
}

impl Draft {
    /// What this run decided `[defaults].<role>` should be: the last decision
    /// recorded for it, or `None` where the run left the role alone. `Some(None)`
    /// is a role this run *cleared*, which is not the same as one it never
    /// touched.
    pub fn decided_default(&self, role: DefaultRole) -> Option<Option<&str>> {
        self.edits.iter().rev().find_map(|edit| match edit {
            Change::SetDefaultRole {
                role: decided,
                value,
            } if *decided == role => Some(value.as_deref()),
            _ => None,
        })
    }

    /// Follow a pending connection's rename or deletion through any `[defaults]`
    /// already decided this run, whose values are qualified `<connection>/<model>`
    /// strings.
    ///
    /// Needed because both halves can be decided in one run, in either order: a
    /// default set on a connection added moments ago, and then that connection
    /// renamed. `render` applies the edits as written, so a stale qualification
    /// would name a table the file will not have.
    fn requalify_defaults(&mut self, from: &str, to: Option<&str>) {
        for edit in &mut self.edits {
            let Change::SetDefaultRole { value, .. } = edit else {
                continue;
            };
            let Some(model) = value
                .as_deref()
                .and_then(|qualified| qualified.strip_prefix(&format!("{from}/")))
            else {
                continue;
            };
            *value = to.map(|to| format!("{to}/{model}"));
        }
    }
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

impl Change {
    /// The connection this change is about, or `None` for one that is not about
    /// a connection at all.
    fn connection(&self) -> Option<&str> {
        match self {
            Change::Rename { from, .. } => Some(from),
            Change::SetAuth { name, .. }
            | Change::SetDisabled { name, .. }
            | Change::Delete { name } => Some(name),
            Change::SetDefaultRole { .. } => None,
        }
    }
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
    /// `name` as for [`ExistingAuth::Keychain`], naming the `oauth:<name>`
    /// item the grant is stored under.
    Oauth {
        name: Option<String>,
    },
}

impl ExistingAuth {
    /// The secret-store item this auth reads, for the two kinds that read one:
    /// a keychain key, or an OAuth grant. `None` for the kinds whose
    /// credential is elsewhere.
    pub(crate) fn item_name(&self, connection: &str) -> Option<String> {
        match self {
            ExistingAuth::Keychain { name } => Some(crate::secrets::inference_name(
                name.as_deref().unwrap_or(connection),
            )),
            ExistingAuth::Oauth { name } => Some(crate::secrets::oauth_name(
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
    /// The provider name, or the `custom-<dialect>` name a described endpoint
    /// reports ([`crate::api::Provider::name`]) — so a row in the management list
    /// says what it speaks either way.
    pub provider: String,
    /// The base URL a described endpoint carries, `None` for a named provider.
    pub base_url: Option<String>,
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
                let string = |key: &str| -> Result<Option<String>, String> {
                    match connection.get(key) {
                        None => Ok(None),
                        Some(value) => Ok(Some(
                            value
                                .as_str()
                                .ok_or_else(|| where_(&format!("has a non-string `{key}`")))?
                                .to_string(),
                        )),
                    }
                };
                // A connection either names a provider or describes one with a
                // dialect and a base URL (docs/transport.md). Both spellings are
                // listed, so a described endpoint is as repairable as a named one.
                let base_url = string("base-url")?;
                let provider = match (string("provider")?, string("dialect")?) {
                    (Some(provider), _) => provider,
                    (None, Some(dialect)) => crate::api::WireDialect::from_name(&dialect)
                        .map(|dialect| Provider::Custom(dialect).name().to_string())
                        .map_err(|error| where_(&error.to_string()))?,
                    (None, None) => {
                        return Err(where_("has neither a `provider` nor a `dialect`"));
                    }
                };
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
                        base_url,
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
    // Absent is the implicit item name; present-but-not-a-string is an error.
    let optional_name = || -> Result<Option<String>, String> {
        match table.get("name") {
            None => Ok(None),
            Some(_) => Ok(Some(string("name")?)),
        }
    };
    match kind {
        "env" => Ok(ExistingAuth::Env {
            key: string("key")?,
        }),
        "codex-cache" => Ok(ExistingAuth::CodexCache),
        "keychain" => Ok(ExistingAuth::Keychain {
            name: optional_name()?,
        }),
        "oauth" => Ok(ExistingAuth::Oauth {
            name: optional_name()?,
        }),
        other => Err(format!(
            "has an `auth` of unrecognized kind {other:?}; this build understands \
             env, keychain, oauth, and codex-cache"
        )),
    }
}

/// The catalog the flow offers per provider: how to describe it, the variable
/// its key conventionally lives in, and a starting model when nothing live can
/// be listed.
pub(crate) struct ProviderCard {
    /// `None` is the card for an endpoint this build knows no name for: the
    /// dialect and base URL are asked for instead, and the provider is built from
    /// the answers (docs/transport.md).
    pub(crate) provider: Option<Provider>,
    pub(crate) label: &'static str,
    pub(crate) detail: &'static str,
    /// The conventional variable name, `None` for a provider whose credential is
    /// not an API key.
    pub(crate) env_key: Option<&'static str>,
}

pub(crate) const PROVIDERS: [ProviderCard; 5] = [
    ProviderCard {
        provider: Some(Provider::Anthropic),
        label: "Anthropic",
        detail: "an API key for api.anthropic.com",
        env_key: Some("ANTHROPIC_API_KEY"),
    },
    ProviderCard {
        provider: Some(Provider::OpenRouter),
        label: "OpenRouter",
        detail: "one key, many models",
        env_key: Some("OPENROUTER_API_KEY"),
    },
    ProviderCard {
        provider: Some(Provider::OpenAI),
        label: "OpenAI",
        detail: "an API key for the platform API",
        env_key: Some("OPENAI_API_KEY"),
    },
    ProviderCard {
        provider: Some(Provider::OpenAICodex),
        label: "ChatGPT subscription",
        detail: "your Plus or Pro plan",
        env_key: None,
    },
    ProviderCard {
        provider: None,
        label: "Something else",
        detail: "an endpoint speaking Anthropic Messages or OpenAI Responses",
        // A described endpoint has no conventional variable name; the flow
        // suggests one from the connection's own name instead.
        env_key: None,
    },
];

#[cfg(test)]
mod test;
