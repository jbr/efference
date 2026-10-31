//! Editing `[defaults]` — which model the spotter runs when a session names
//! none (docs/operating.md).
//!
//! Its own step, not part of adding a connection. Which providers are configured
//! and which model sessions default to are separate questions, and asking the
//! second one per connection asked it once per connection while answering it at
//! most once — so on the ordinary path it was two required questions into
//! nothing.
//!
//! A role is set by picking a connection and then a model on it, which is where
//! a listing belongs: the connection selects the endpoint to ask.

use super::*;

impl DefaultRole {
    /// The one role setup edits. The main model has no default: past picks
    /// answer the omitted branch, else the session form names one — and an
    /// omitted triage shares the main client, so neither is anything to
    /// configure (docs/transport.md).
    pub(super) const ALL: [Self; 1] = [Self::Spotter];

    /// What this role is called where a person reads it.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Spotter => "Spotter",
        }
    }

    /// What it is for, in one clause. Said beside the label rather than left to
    /// the key name, since `spotter` names nothing a new reader knows.
    pub(super) fn purpose(self) -> &'static str {
        match self {
            Self::Spotter => "reviews tool calls the permission rules did not clear",
        }
    }
}

/// What `[defaults]` holds now, so the editor can show each role's current value
/// and offer leaving it alone. Empty on a first run.
#[derive(Debug, Clone, Default)]
pub(super) struct Current {
    pub(super) spotter: Option<String>,
}

impl Current {
    fn of(&self, role: DefaultRole) -> Option<&str> {
        match role {
            DefaultRole::Spotter => self.spotter.as_deref(),
        }
    }
}

/// Walk the one role: a connection, then a model on it.
pub(super) async fn edit<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    connections: &BTreeMap<String, ExistingConnection>,
    draft: &mut Draft,
    current: &Current,
) -> Result<(), Cancelled> {
    let mut decided = current.clone();
    for role in DefaultRole::ALL {
        let Some(choice) = role_value(prompter, probe, role, connections, &decided).await? else {
            continue;
        };
        match role {
            DefaultRole::Spotter => decided.spotter = choice.clone(),
        }
        draft.record(Change::SetDefaultRole {
            role,
            value: choice,
        });
    }
    Ok(())
}

/// One role's new value: `None` to leave it as it is, `Some(None)` to clear it,
/// `Some(Some(qualified))` to set it.
///
/// The menu leads with what the role already is, so the cheapest answer is Enter.
async fn role_value<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    role: DefaultRole,
    connections: &BTreeMap<String, ExistingConnection>,
    current: &Current,
) -> Result<Option<Option<String>>, Cancelled> {
    let mut choices = Vec::new();
    let existing = current.of(role).map(str::to_string);
    match &existing {
        Some(value) => choices.push(Choice::new(format!("Keep {value}")).detail(role.purpose())),
        // An unset spotter is a working configuration: the provider fallbacks apply.
        None => choices.push(Choice::new("Leave unset").detail(role.purpose())),
    }
    choices.push(Choice::new("Choose a model").detail(role.purpose()));
    if existing.is_some() {
        choices.push(Choice::new("Clear it").detail(role.purpose()));
    }
    let picked = prompter.select(role.label(), &choices)?;
    if picked == 0 {
        return Ok(None);
    }
    if picked == 2 {
        return Ok(Some(None));
    }
    let name = pick_connection(prompter, connections)?;
    let connection = &connections[&name];
    // The listing comes from the connection being picked on, so the menu is the
    // models that connection actually serves. A credential setup cannot resolve
    // leaves a typed id, which is taken as written and meets the same boot
    // validation any other does.
    let key = readable_key(probe, &name, connection);
    let model = flow::choose_model(
        prompter,
        probe,
        &format!("{} on {name}", role.label()),
        &name,
        &target_for(connection),
        hint(connection),
        key.as_deref(),
    )
    .await?;
    Ok(Some(Some(format!("{name}/{model}"))))
}

/// Which connection the role's model lives on. A single configured connection is
/// not a question.
fn pick_connection<P: Prompter>(
    prompter: &mut P,
    connections: &BTreeMap<String, ExistingConnection>,
) -> Result<String, Cancelled> {
    let names: Vec<&String> = connections.keys().collect();
    if let [only] = names[..] {
        return Ok(only.clone());
    }
    let choices: Vec<Choice> = names
        .iter()
        .map(|name| Choice::new(*name).detail(&connections[*name].provider))
        .collect();
    let picked = prompter.select("On which connection?", &choices)?;
    Ok(names[picked].clone())
}

/// What to dial a listing against: the connection's provider, plus the base URL
/// a described endpoint carries (docs/transport.md). An unreadable provider name
/// leaves no target, and the model is typed instead.
fn target_for(connection: &ExistingConnection) -> Target {
    let provider = match connection.provider.strip_prefix("custom-") {
        Some(dialect) => WireDialect::from_name(dialect).ok().map(Provider::Custom),
        None => Provider::from_name(&connection.provider).ok(),
    };
    Target {
        provider: provider.unwrap_or(Provider::Anthropic),
        base: connection.base_url.clone(),
    }
}

/// The prefill for a typed model id: the provider's cheap band where it has
/// one. There is no id that is right across endpoints otherwise.
fn hint(connection: &ExistingConnection) -> Option<&'static str> {
    Provider::from_name(&connection.provider)
        .ok()
        .and_then(|provider| provider.default_spotter_model())
}

/// The connection's key, where setup can read it without asking: an environment
/// variable set in this shell, or an item the store already holds. Never
/// prompted for — this editor is about which model a role runs, and a credential
/// question here would be a second concern in the same step.
fn readable_key<Q: Probe>(
    probe: &Q,
    name: &str,
    connection: &ExistingConnection,
) -> Option<String> {
    match &connection.auth {
        ExistingAuth::Env { key } => probe.env_var(key),
        ExistingAuth::Keychain { name: item } => probe
            .secret_value(&crate::secrets::inference_name(
                item.as_deref().unwrap_or(name),
            ))
            .ok()
            .flatten(),
        ExistingAuth::CodexCache | ExistingAuth::Oauth { .. } | ExistingAuth::None => None,
    }
}
