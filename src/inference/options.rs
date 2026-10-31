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
use crate::triage::TriageModel;

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
    /// Past explicit picks, decayed (docs/transport.md): the project mode
    /// past its mass threshold, else the system-wide aggregate.
    History,
    /// `[defaults]` in the user's inference config.
    UserDefault,
    /// The main connection's provider supplied its own cheap-band spotter.
    MainProvider,
    /// A connection that can reach a decision model supplied jev as the spotter.
    DecisionConnection,
    /// The selected project config's `[transport]`.
    ProjectConfig,
    /// Nothing named it, so the wire omits it and the provider decides.
    ProviderDefault,
    /// Nothing named a separate triage client, so the triage shares main's.
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
    /// The triage runs on the main client.
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
        history: Option<(&crate::history::History, &crate::history::Scope)>,
    ) -> ResolvedOptions {
        let mut issues = Vec::new();
        let main = self.resolve_main(options, history, &mut issues);
        let spotter = self.resolve_spotter(options, &main, &mut issues);
        let triage = self.resolve_triage(options, &mut issues);
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
        history: Option<(&crate::history::History, &crate::history::Scope)>,
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
            // Past picks are the default: the project mode past its mass
            // threshold, else the system-wide aggregate. A remembered
            // connection that is no longer usable is not an issue — it falls
            // through to the refusal, since no request named it
            // (docs/transport.md).
            Requested::Omitted => {
                if let Some((store, scope)) = history
                    && let Some(candidate) = store.default(scope, crate::history::now())
                    && let Some(selected) =
                        self.select_if_usable(&candidate.connection, &candidate.model)
                {
                    return inference_only(
                        Effective::resolved(selected, SelectionSource::History),
                        "model",
                        issues,
                    );
                }
                issues.push(Issue::new(
                    "model",
                    IssueCode::Required,
                    "no model was given and no past pick applies",
                ));
                Effective::failed()
            }
        }
    }

    /// The history candidate, when its connection is still configured and
    /// available. Silent (`None`) otherwise, so a stale pick falls through to
    /// the refusal rather than erroring on a request nobody made.
    fn select_if_usable(&self, connection: &str, model: &str) -> Option<ModelSelection> {
        let configured = self.connections.get(connection)?;
        if configured.disabled || configured.credential.is_err() {
            return None;
        }
        if !configured.provider.serves_inference() {
            return None;
        }
        Some(ModelSelection {
            connection: connection.to_string(),
            provider: configured.provider,
            model: model.to_string(),
        })
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
                // Then the main connection's own `spotter-model`, then a decision
                // model on any connection that reaches one, then the main
                // provider's cheap band.
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
                        .map(|model| (main_selection(main, model), SelectionSource::MainProvider))
                        .or_else(|| {
                            self.decision_spotter()
                                .map(|selection| (selection, SelectionSource::DecisionConnection))
                        })
                        .or_else(|| {
                            self.default_spotter(main).map(|model| {
                                (main_selection(main, model), SelectionSource::MainProvider)
                            })
                        }) {
                        Some((selection, source)) => Effective::resolved(selection, source),
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
                    // No main to borrow a provider from: the spotter is unresolvable
                    // for a reason main's issue already states, so this adds no
                    // second issue for one mistake.
                    None => Effective::failed(),
                },
            },
        }
    }

    /// The spotter default where a decision model is reachable (docs/permissions.md):
    /// the first loaded `typesafe` connection by name, else the first OpenRouter one.
    /// First-party ahead of OpenRouter, whose decisions route is an alpha; by name
    /// within a provider so the choice is the same on every host with this config.
    fn decision_spotter(&self) -> Option<ModelSelection> {
        [
            crate::log::Provider::TypeSafe,
            crate::log::Provider::OpenRouter,
        ]
        .into_iter()
        .find_map(|provider| {
            let (name, _) = self.connections.iter().find(|(_, connection)| {
                connection.provider == provider && connection.credential.is_ok()
            })?;
            Some(ModelSelection {
                connection: name.clone(),
                provider,
                model: provider.default_decision_model()?.to_string(),
            })
        })
    }

    /// The effective triage client is what a session would actually get: the
    /// request's choice, else main's own client: there is no configured triage
    /// default (docs/transport.md).
    fn resolve_triage(
        &self,
        options: &SessionOptions,
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
            // No configured triage: an omitted triage shares the main client,
            // whatever main resolved to (docs/transport.md).
            None => {
                return Effective::resolved(EffectiveTriage::Shared, SelectionSource::SharedMain);
            }
        };
        let selected = self.select(&selection, "triage", source, issues);
        let Some(selection) = selected.value else {
            return Effective::failed();
        };
        // A separate triage reads the text view (docs/triage.md), so any wire
        // dialect that serves turns can triage for any other.
        if !requires_inference(&selection, "triage", issues) {
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
            credential_variables: self.credential_variables.iter().cloned().collect(),
            secrets: std::sync::Arc::clone(&self.secrets),
        })
    }

    /// The spotter role alone, for a verb that runs only the spotter (`efference
    /// dev spotter`, `dev spotter-eval`): `spotter` names it, else the same
    /// default chain a session takes. Main is resolved only as that chain's last
    /// link needs it, so a config with no main default refuses only when the
    /// spotter would have fallen through to main's cheap band.
    pub fn resolve_spotter_role(
        &self,
        spotter: Option<&str>,
    ) -> Result<(ModelSelection, crate::api::Endpoint), Error> {
        let resolved = self.resolve_options(
            &SessionOptions {
                spotter_model: spotter.map(str::to_string),
                ..Default::default()
            },
            None,
            None,
        );
        if let Some(issue) = resolved
            .issues
            .iter()
            .find(|issue| issue.field == "spotter_model")
        {
            return Err(match &issue.detail {
                Some(detail) => format!("spotter: {}: {detail}", issue.message),
                None => format!("spotter: {}", issue.message),
            }
            .into());
        }
        let selection = resolved.effective.spotter.value.ok_or(
            "no spotter resolved: name one (`connection/model`), or set [defaults].spotter \
             in ~/.efference/config.toml",
        )?;
        let role = self.attach(selection, "spotter")?;
        Ok((role.selection, role.endpoint))
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

/// A spotter on the main connection, under `model`.
fn main_selection(main: &ModelSelection, model: String) -> ModelSelection {
    ModelSelection {
        connection: main.connection.clone(),
        provider: main.provider,
        model,
    }
}

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
             back the spotter, but not {field}",
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
