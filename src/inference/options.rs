//! One resolution flow for a proposed session's inference options
//! (docs/control-api.md). `QUERY /sessions` serializes the result; `POST /sessions`
//! rejects its issues and then attaches credentials to the same selections, so
//! neither route carries its own copy of the precedence rules.
//!
//! Every field resolves independently. A field the registry can *disprove* — a
//! connection that is not configured, a temperature outside its range — records an
//! issue and resolves to nothing; a field it merely cannot check — a model name the
//! host has no catalog for — resolves from the request unquestioned. "Could not
//! validate" and "nothing known wrong with it" are the same answer here, and
//! creation and boot stay authoritative either way.

use serde::{Deserialize, Serialize};

use super::{
    Connection, InferenceRegistry, ModelReference, ModelSelection, ResolvedRole, ResolvedTriage,
    SessionInference, TriageChoice, role_secret_values,
};
use crate::api::Error;
use crate::distill::TriageModel;

/// The accepted temperature, stated once. Both control routes and the CLI reach
/// it through [`InferenceRegistry::resolve_options`], so none of them carries its
/// own copy of the bound.
pub const TEMPERATURE: std::ops::RangeInclusive<u8> = 0..=100;

/// The editable selections a client proposes, shared by preview and creation. A
/// role names its connection and its provider-native model separately, or gives
/// the `connection/model` slug alone and lets [`InferenceRegistry::parse_model`]
/// split it. Naming the connection is what makes a native id containing a slash
/// unambiguous; the slug stays for surfaces where one opaque string is the
/// convenient thing to pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotter_connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotter_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<TriageChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<u8>,
}

/// The precedence branch that supplied an effective value. Closed, because a
/// client renders each branch differently: an inherited value is editable in
/// place, a requested one is the edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    /// The request named it.
    Request,
    /// `[defaults]` in the user's inference config.
    UserDefault,
    /// The main connection's provider supplied its own cheap-band spotter.
    MainProvider,
    /// The selected project config's `[transport]`.
    ProjectConfig,
    /// Nothing named it, so the wire omits it and the provider decides.
    ProviderDefault,
    /// Nothing named a separate triage client, so the fork peek shares main's.
    SharedMain,
}

/// One effective field: the value that would be used, and where it came from.
/// `source: null` marks a field whose resolution *failed* — the paired issue says
/// why. A `value: null` with a source present is an ordinary omission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effective<T> {
    pub value: Option<T>,
    pub source: Option<SelectionSource>,
}

impl<T> Effective<T> {
    fn resolved(value: T, source: SelectionSource) -> Self {
        Self {
            value: Some(value),
            source: Some(source),
        }
    }

    /// A field nothing named: the wire omits it and the provider decides.
    fn omitted() -> Self {
        Self {
            value: None,
            source: Some(SelectionSource::ProviderDefault),
        }
    }

    /// A field whose resolution failed. The paired issue carries the reason.
    fn failed() -> Self {
        Self {
            value: None,
            source: None,
        }
    }

    fn fail(&mut self) {
        *self = Self::failed();
    }
}

/// The effective triage client, flattened so it reads like the other role
/// selections rather than echoing the request's tagged spelling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EffectiveTriage {
    /// The fork peek runs on the main client.
    Shared,
    Separate {
        #[serde(flatten)]
        selection: ModelSelection,
        thinking: bool,
        auto_choice: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
}

/// Why a field could not be used as given. `field` names the request field, so a
/// client can attach the message to the control the operator is editing.
///
/// `field` is a `Cow` rather than a `&'static str` because the type crosses the
/// wire in both directions: the host builds every issue from a literal, and a
/// control client decodes the same type back (docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub field: std::borrow::Cow<'static, str>,
    pub code: IssueCode,
    pub message: String,
    /// Why, in terms only a local caller should see — a credential loader's own
    /// words, which may name an environment key or a keychain item. Creation
    /// appends it, because the caller is the operator and it is what they need to
    /// fix the connection; preview does not serialize it, because the `message`
    /// above is the whole user-facing answer.
    #[serde(skip)]
    pub detail: Option<String>,
}

impl Issue {
    pub fn new(field: &'static str, code: IssueCode, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code,
            message: message.into(),
            detail: None,
        }
    }

    fn detailed(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// The closed issue vocabulary. A client may render an unknown code by its
/// `message` alone, but every code here is one it can special-case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// The field is needed and absent.
    Required,
    /// The JSON value is the wrong type for the field.
    InvalidType,
    /// A numeric field is outside its accepted range.
    OutOfRange,
    /// The value is well-formed but selects nothing that exists.
    InvalidSelection,
    /// The connection is configured, but its credential is disabled or did not load.
    UnavailableConnection,
    /// A separate triage client cannot replay main's wire dialect.
    IncompatibleDialect,
    /// The selected project config could not be read or parsed.
    InvalidConfig,
}

/// Every effective field plus the issues that would stop creation. Preview
/// serializes this; creation refuses a non-empty `issues` and then resolves
/// credentials for the same selections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedOptions {
    pub effective: EffectiveOptions,
    pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveOptions {
    pub main: Effective<ModelSelection>,
    pub spotter: Effective<ModelSelection>,
    pub triage: Effective<EffectiveTriage>,
    pub effort: Effective<String>,
    pub temperature: Effective<u8>,
}

impl ResolvedOptions {
    /// Mark a field as failed because something upstream of resolution rejected
    /// it — a preview body whose JSON value would not even decode into the
    /// field's type. Resolution sees such a field as absent and would otherwise
    /// report the omission branch, which reads as "nothing was sent" rather than
    /// "what you sent was unusable". One rule holds either way: **a field with an
    /// issue has no source.**
    pub fn invalidate(&mut self, field: &str) {
        let effective = &mut self.effective;
        match field {
            "model" | "connection" => effective.main.fail(),
            "spotter_model" | "spotter_connection" => effective.spotter.fail(),
            "triage" => effective.triage.fail(),
            "effort" => effective.effort.fail(),
            "temperature" => effective.temperature.fail(),
            // `cwd` and `config` are the host's own request fields; neither has
            // an effective selection to blank.
            _ => {}
        }
    }
}

/// What the request said about one role, kept apart from "nothing was said" so an
/// invalid override never silently falls back to a default.
enum Requested {
    /// The request named nothing; the default precedence applies.
    Omitted,
    Reference(ModelReference),
    /// The request named something unusable. Its issue is already recorded.
    Failed,
}

impl InferenceRegistry {
    /// Resolve every option independently, without constructing an endpoint or
    /// touching credential material. `project_effort` is the selected project
    /// config's `[transport].effort`, already loaded by the caller — resolution
    /// reads no files.
    pub fn resolve_options(
        &self,
        options: &SessionOptions,
        project_effort: Option<&str>,
    ) -> ResolvedOptions {
        let mut issues = Vec::new();
        let main = self.resolve_main(options, &mut issues);
        let spotter = self.resolve_spotter(options, &main, &mut issues);
        let triage = self.resolve_triage(options, &main, &mut issues);
        let effort = match (&options.effort, project_effort) {
            (Some(effort), _) => Effective::resolved(effort.clone(), SelectionSource::Request),
            (None, Some(effort)) => {
                Effective::resolved(effort.to_string(), SelectionSource::ProjectConfig)
            }
            (None, None) => Effective::omitted(),
        };
        let temperature = match options.temperature {
            Some(temperature) if TEMPERATURE.contains(&temperature) => {
                Effective::resolved(temperature, SelectionSource::Request)
            }
            Some(_) => {
                issues.push(Issue::new(
                    "temperature",
                    IssueCode::OutOfRange,
                    format!(
                        "must be a whole number from {} to {}",
                        TEMPERATURE.start(),
                        TEMPERATURE.end()
                    ),
                ));
                Effective::failed()
            }
            None => Effective::omitted(),
        };
        ResolvedOptions {
            effective: EffectiveOptions {
                main,
                spotter,
                triage,
                effort,
                temperature,
            },
            issues,
        }
    }

    fn resolve_main(
        &self,
        options: &SessionOptions,
        issues: &mut Vec<Issue>,
    ) -> Effective<ModelSelection> {
        match self.requested(
            options.connection.as_deref(),
            options.model.as_deref(),
            "model",
            issues,
        ) {
            Requested::Failed => Effective::failed(),
            Requested::Reference(reference) => {
                let selected = self.select(&reference, "model", SelectionSource::Request, issues);
                inference_only(selected, "model", issues)
            }
            Requested::Omitted => match self.defaults.model.as_ref() {
                Some(default) => {
                    let selected =
                        self.select(default, "model", SelectionSource::UserDefault, issues);
                    inference_only(selected, "model", issues)
                }
                None => {
                    issues.push(Issue::new(
                        "model",
                        IssueCode::Required,
                        "no model was given and [defaults].model is not configured",
                    ));
                    Effective::failed()
                }
            },
        }
    }

    /// Spotter falls back to the main connection's provider default, so it needs
    /// main's selection — but only for that branch. An explicit spotter resolves
    /// whether or not main did, which is what keeps the two independent.
    fn resolve_spotter(
        &self,
        options: &SessionOptions,
        main: &Effective<ModelSelection>,
        issues: &mut Vec<Issue>,
    ) -> Effective<ModelSelection> {
        match self.requested(
            options.spotter_connection.as_deref(),
            options.spotter_model.as_deref(),
            "spotter_model",
            issues,
        ) {
            Requested::Failed => Effective::failed(),
            Requested::Reference(reference) => self.select(
                &reference,
                "spotter_model",
                SelectionSource::Request,
                issues,
            ),
            Requested::Omitted => match self.defaults.spotter.as_ref() {
                Some(default) => self.select(
                    default,
                    "spotter_model",
                    SelectionSource::UserDefault,
                    issues,
                ),
                // No default and no main to borrow a provider from: the spotter is
                // unresolvable for a reason main's issue already states, so this
                // adds no second issue for one mistake.
                None => match main.value.as_ref() {
                    // The main connection answers first, in its own configuration
                    // (`spotter-model`), then the arm's built-in default. Both are
                    // answers about a catalog this harness does not compile, so a
                    // connection that names its own cheap model is the more
                    // specific of the two.
                    Some(main) => match self
                        .connections
                        .get(&main.connection)
                        .and_then(|connection| connection.spotter_model.clone())
                        .or_else(|| main.provider.default_spotter_model().map(str::to_string))
                    {
                        Some(model) => Effective::resolved(
                            ModelSelection {
                                connection: main.connection.clone(),
                                provider: main.provider,
                                model,
                            },
                            SelectionSource::MainProvider,
                        ),
                        // A custom endpoint has no cheap band to borrow and no
                        // catalog to read one from: whatever the operator pointed
                        // at is the whole domain, so the spotter has to be named
                        // rather than guessed.
                        None => {
                            issues.push(Issue::new(
                                "spotter_model",
                                IssueCode::Required,
                                format!(
                                    "{} publishes no model catalog to pick a default spotter \
                                     from — name one in [defaults].spotter or on the \
                                     connection as `spotter-model`",
                                    main.provider
                                ),
                            ));
                            Effective::failed()
                        }
                    },
                    None => Effective::failed(),
                },
            },
        }
    }

    /// The effective triage client is what a session would actually get: the
    /// request's choice, else `[defaults].triage`, else main's own client. An
    /// omitted triage must not report `shared` while creation would build a
    /// separate client from the configured default.
    fn resolve_triage(
        &self,
        options: &SessionOptions,
        main: &Effective<ModelSelection>,
        issues: &mut Vec<Issue>,
    ) -> Effective<EffectiveTriage> {
        let (selection, options_from, source) = match &options.triage {
            Some(TriageChoice::Shared) => {
                return Effective::resolved(EffectiveTriage::Shared, SelectionSource::Request);
            }
            Some(TriageChoice::Separate {
                connection,
                model,
                thinking,
                auto_choice,
                effort,
            }) => {
                let reference = match self.requested(
                    connection.as_deref(),
                    Some(model.as_str()),
                    "triage",
                    issues,
                ) {
                    Requested::Reference(reference) => reference,
                    // `Some(model)` above means `requested` never answers Omitted.
                    Requested::Omitted | Requested::Failed => return Effective::failed(),
                };
                (
                    reference,
                    (*thinking, *auto_choice, effort.clone()),
                    SelectionSource::Request,
                )
            }
            None => match self.defaults.triage.as_ref() {
                Some(default) => (
                    default.clone(),
                    (false, false, None),
                    SelectionSource::UserDefault,
                ),
                None => {
                    return Effective::resolved(
                        EffectiveTriage::Shared,
                        SelectionSource::SharedMain,
                    );
                }
            },
        };
        let selected = self.select(&selection, "triage", source, issues);
        let Some(selection) = selected.value else {
            return Effective::failed();
        };
        // A separate triage replays main's transcript, so it must speak main's
        // dialect. Unknowable when main did not resolve, and silence there is
        // correct: creation checks it again with both roles in hand.
        if !requires_inference(&selection, "triage", issues) {
            return Effective::failed();
        }
        if let Some(main) = main.value.as_ref()
            && selection.provider.inference_dialect() != main.provider.inference_dialect()
        {
            issues.push(Issue::new(
                "triage",
                IssueCode::IncompatibleDialect,
                format!(
                    "triage {} ({}) cannot replay main {} ({}) because their wire dialects differ",
                    selection.model,
                    selection.provider.name(),
                    main.model,
                    main.provider.name(),
                ),
            ));
            return Effective::failed();
        }
        let (thinking, auto_choice, effort) = options_from;
        Effective::resolved(
            EffectiveTriage::Separate {
                selection,
                thinking,
                auto_choice,
                effort,
            },
            source,
        )
    }

    /// Split what the request said about one role into the three cases
    /// resolution distinguishes. An explicit connection makes `model` a native
    /// id, so a slash inside it is never read as a connection prefix.
    fn requested(
        &self,
        connection: Option<&str>,
        model: Option<&str>,
        field: &'static str,
        issues: &mut Vec<Issue>,
    ) -> Requested {
        match (connection, model) {
            (None, None) => Requested::Omitted,
            (Some(connection), Some(model)) if !model.is_empty() => {
                Requested::Reference(ModelReference {
                    connection: connection.to_string(),
                    model: model.to_string(),
                })
            }
            // A connection with no model is a draft mid-edit, not a malformed
            // request: there is no provider-default main model to stand in, so
            // the honest answer is that a model is still needed.
            (Some(_), _) => {
                issues.push(Issue::new(
                    field,
                    IssueCode::Required,
                    "naming a connection also needs a model",
                ));
                Requested::Failed
            }
            (None, Some(model)) => match self.parse_model(model, field) {
                Ok(reference) => Requested::Reference(reference),
                Err(error) => {
                    issues.push(Issue::new(
                        field,
                        IssueCode::InvalidSelection,
                        error.to_string(),
                    ));
                    Requested::Failed
                }
            },
        }
    }

    /// Check a reference against the catalog. An unconfigured or unavailable
    /// connection is disprovable and becomes an issue; the model name is not
    /// checkable here and passes through.
    /// A role that runs a turn cannot sit on a provider that serves no inference
    /// (docs/transport.md) — a decision-model connection answers questions about
    /// a state and produces no assistant output to encode, assemble, or log. The
    /// refusal happens here, at resolution, which is why everything downstream
    /// can read `inference_dialect()` as present.
    fn select(
        &self,
        reference: &ModelReference,
        field: &'static str,
        source: SelectionSource,
        issues: &mut Vec<Issue>,
    ) -> Effective<ModelSelection> {
        let Some(connection) = self.connections.get(&reference.connection) else {
            issues.push(Issue::new(
                field,
                IssueCode::InvalidSelection,
                format!(
                    "connection {:?} is not configured in ~/.efference/config.toml",
                    reference.connection
                ),
            ));
            return Effective::failed();
        };
        if let Err(error) = &connection.credential {
            issues.push(
                Issue::new(
                    field,
                    IssueCode::UnavailableConnection,
                    format!("connection {:?} is unavailable", reference.connection),
                )
                .detailed(error),
            );
            return Effective::failed();
        }
        Effective::resolved(
            ModelSelection {
                connection: reference.connection.clone(),
                provider: connection.provider,
                model: reference.model.clone(),
            },
            source,
        )
    }

    /// Creation's half: refuse anything preview would have flagged, then attach
    /// each selected role's endpoint and masking needles. It re-checks nothing —
    /// the selections are the ones preview reported.
    pub fn prepare(&self, resolved: ResolvedOptions) -> Result<SessionInference, Error> {
        if let Some(issue) = resolved.issues.first() {
            return Err(match &issue.detail {
                Some(detail) => format!("{}: {}: {detail}", issue.field, issue.message),
                None => format!("{}: {}", issue.field, issue.message),
            }
            .into());
        }
        let EffectiveOptions {
            main,
            spotter,
            triage,
            ..
        } = resolved.effective;
        let main = self.attach(required(main, "model")?, "main")?;
        let spotter = self.attach(required(spotter, "spotter_model")?, "spotter")?;
        let triage = match required(triage, "triage")? {
            EffectiveTriage::Shared => None,
            EffectiveTriage::Separate {
                selection,
                thinking,
                auto_choice,
                effort,
            } => {
                let options = TriageModel {
                    model: selection.model.clone(),
                    thinking,
                    auto_choice,
                    effort,
                };
                Some(ResolvedTriage {
                    role: self.attach(selection, "triage")?,
                    options,
                })
            }
        };
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

    /// Clone the credential material for an already-selected role.
    fn attach(&self, selection: ModelSelection, role: &str) -> Result<ResolvedRole, Error> {
        let credential = self
            .connections
            .get(&selection.connection)
            .map(|connection: &Connection| &connection.credential)
            .ok_or_else(|| {
                format!(
                    "{role} model selects connection {:?}, but it is not configured in ~/.efference/config.toml",
                    selection.connection
                )
            })?
            .as_ref()
            .map_err(|error| {
                format!(
                    "{role} model selects connection {:?}, but it is unavailable: {error}",
                    selection.connection
                )
            })?;
        Ok(ResolvedRole {
            endpoint: credential.endpoint.clone(),
            secret_values: credential.secret_values.clone(),
            selection,
        })
    }
}

/// A field creation cannot proceed without. Unreachable with an empty issue list —
/// every failed field records one — but stated rather than unwrapped.
fn required<T>(effective: Effective<T>, field: &str) -> Result<T, Error> {
    effective
        .value
        .ok_or_else(|| format!("{field} did not resolve").into())
}

#[cfg(test)]
mod test;

/// Reject a selection whose provider serves no inference, for a role that needs
/// it. Separate from `select` because the spotter role is exactly the one that
/// may name such a provider (docs/permissions.md).
fn requires_inference(
    selection: &ModelSelection,
    field: &'static str,
    issues: &mut Vec<Issue>,
) -> bool {
    if selection.provider.serves_inference() {
        return true;
    }
    issues.push(Issue::new(
        field,
        IssueCode::InvalidSelection,
        format!(
            "connection {:?} is a {} connection, which serves decision models only — it can \
             back the spotter, but not {field} (docs/permissions.md)",
            selection.connection,
            selection.provider.name(),
        ),
    ));
    false
}

/// [`requires_inference`] applied to an already-resolved selection, failing it
/// when the provider cannot serve a turn.
fn inference_only(
    selected: Effective<ModelSelection>,
    field: &'static str,
    issues: &mut Vec<Issue>,
) -> Effective<ModelSelection> {
    match selected.value.as_ref() {
        Some(selection) if !requires_inference(selection, field, issues) => Effective::failed(),
        _ => selected,
    }
}
