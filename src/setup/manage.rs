//! The management flow: shown instead of the plain add path once a config
//! file has connections in it (backlog/setup-edits-connections.md). A list of
//! rows, each stating its readiness from local checks only, with edit /
//! disable / delete on a picked row and "add another connection" folding back
//! into `src/setup/flow.rs`'s add path.

use std::collections::BTreeMap;

use super::*;

mod credential;
use credential::edit_connection;

/// A mutable view of `existing`, updated as the session decides edits, so the
/// list, collision checks, and the "what does [defaults] point at" prompts
/// stay accurate across several actions in one run without touching the file
/// until the whole session is rendered.
pub(super) struct Working {
    pub(super) connections: BTreeMap<String, ExistingConnection>,
    pub(super) default_model: Option<String>,
    pub(super) default_spotter: Option<String>,
    pub(super) default_triage: Option<String>,
}

impl Working {
    fn from(existing: &Existing) -> Self {
        Self {
            connections: existing.connections.clone(),
            default_model: existing.default_model.clone(),
            default_spotter: existing.default_spotter.clone(),
            default_triage: existing.default_triage.clone(),
        }
    }

    /// The `Existing` shape `flow::add_connections`/`flow::choose_default`
    /// already know how to read, so those two are unchanged by this module.
    pub(super) fn as_existing(&self) -> Existing {
        Existing {
            connections: self.connections.clone(),
            default_model: self.default_model.clone(),
            default_spotter: self.default_spotter.clone(),
            default_triage: self.default_triage.clone(),
        }
    }

    pub(super) fn default_role_mut(&mut self, role: DefaultRole) -> &mut Option<String> {
        match role {
            DefaultRole::Model => &mut self.default_model,
            DefaultRole::Spotter => &mut self.default_spotter,
            DefaultRole::Triage => &mut self.default_triage,
        }
    }
}

pub async fn run<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    existing: &Existing,
) -> Result<Draft, Cancelled> {
    let mut working = Working::from(existing);
    let mut draft = Draft::default();
    loop {
        let names: Vec<String> = working.connections.keys().cloned().collect();
        let mut choices: Vec<Choice> = names
            .iter()
            .map(|name| row_choice(&working, probe, name))
            .collect();
        choices.push(Choice::new("Add another connection"));
        choices.push(Choice::new("Done"));
        let picked = prompter.select("Connections", &choices)?;
        if picked == choices.len() - 1 {
            return Ok(draft);
        }
        if picked == choices.len() - 2 {
            add_another(prompter, probe, &mut working, &mut draft).await?;
            continue;
        }
        let name = names[picked].clone();
        act_on_connection(prompter, probe, &mut working, &mut draft, &name).await?;
    }
}

/// The add path (`src/setup/flow.rs`), folded in as one entry of the list: new
/// connections join the working set so the list reflects them immediately,
/// and a fresh default is offered the same way the first-run path offers one.
async fn add_another<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    working: &mut Working,
    draft: &mut Draft,
) -> Result<(), Cancelled> {
    let before = draft.connections.len();
    let existing_view = working.as_existing();
    flow::add_connections(prompter, probe, &existing_view, draft).await?;
    for (name, connection) in &draft.connections[before..] {
        working.connections.insert(
            name.clone(),
            ExistingConnection {
                provider: connection.provider.name().to_string(),
                auth: credential::to_existing_auth(&connection.auth),
                disabled: false,
            },
        );
    }
    if let Some(name) = flow::choose_default(prompter, &working.as_existing(), draft)? {
        draft.default_connection = Some(name.clone());
        if let Some((_, connection)) = draft.connections.iter().find(|(n, _)| n == &name) {
            working.default_model = Some(format!("{name}/{}", connection.model));
            working.default_spotter = Some(format!("{name}/{}", connection.spotter));
        }
    }
    Ok(())
}

/// Edit / disable / delete on one row, picked from the list.
async fn act_on_connection<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    working: &mut Working,
    draft: &mut Draft,
    name: &str,
) -> Result<(), Cancelled> {
    let connection = working.connections[name].clone();
    let toggle = if connection.disabled {
        "Enable"
    } else {
        "Disable"
    };
    let choices = [
        Choice::new("Edit"),
        Choice::new(toggle),
        Choice::new("Delete"),
        Choice::new("Back"),
    ];
    match prompter.select(name, &choices)? {
        0 => edit_connection(prompter, probe, working, draft, name, &connection).await?,
        1 => {
            let disabled = !connection.disabled;
            if disabled {
                offer_repoint_defaults(prompter, working, draft, name).await?;
            }
            draft.record(Change::SetDisabled {
                name: name.to_string(),
                disabled,
            });
            working.connections.get_mut(name).unwrap().disabled = disabled;
            prompter.say(
                Tone::Good,
                &format!("{name} {}.", if disabled { "disabled" } else { "enabled" }),
            );
        }
        2 => {
            if prompter.confirm(
                &format!("Delete {name}? This removes it from the file."),
                false,
            )? {
                offer_repoint_defaults(prompter, working, draft, name).await?;
                if let Some(item) = connection.auth.item_name(name)
                    && matches!(probe.secret_value(&item), Ok(Some(_)))
                    && prompter
                        .confirm(&format!("Also delete the stored secret `{item}`?"), false)?
                {
                    match probe.delete_secret(&item) {
                        Ok(()) => prompter.say(Tone::Good, &format!("Deleted `{item}`.")),
                        Err(error) => {
                            prompter.say(Tone::Bad, &format!("Could not delete `{item}`: {error}"))
                        }
                    }
                }
                draft.record(Change::Delete {
                    name: name.to_string(),
                });
                working.connections.remove(name);
                prompter.say(Tone::Good, &format!("{name} removed."));
            }
        }
        3 => {}
        _ => unreachable!(),
    }
    Ok(())
}

/// `[defaults]` naming a disabled or deleted connection is an error at load
/// (src/inference.rs), so disable and delete share this prompt: name the
/// affected role, offer to repoint it, and accept a dangling default if the
/// operator chooses one.
async fn offer_repoint_defaults<P: Prompter>(
    prompter: &mut P,
    working: &mut Working,
    draft: &mut Draft,
    name: &str,
) -> Result<(), Cancelled> {
    let affected: Vec<DefaultRole> = working.as_existing().defaults_naming(name).collect();
    for role in affected {
        prompter.say(Tone::Note, &format!("[defaults].{role} names {name}."));
        let others: Vec<String> = working
            .connections
            .iter()
            .filter(|(other, connection)| other.as_str() != name && !connection.disabled)
            .map(|(other, _)| other.clone())
            .collect();
        if others.is_empty() {
            prompter.say(
                Tone::Note,
                "No other connection to repoint it at; it will be left as is.",
            );
            continue;
        }
        if !prompter.confirm(&format!("Repoint [defaults].{role}?"), true)? {
            continue;
        }
        let choices: Vec<Choice> = others
            .iter()
            .map(|other| Choice::new(other.clone()))
            .collect();
        let picked = prompter.select("To which connection?", &choices)?;
        let target = &others[picked];
        let model = prompter
            .text(&format!("Model for [defaults].{role}"), None)?
            .trim()
            .to_string();
        if model.is_empty() {
            prompter.say(Tone::Bad, "No model given; left as is.");
            continue;
        }
        let qualified = format!("{target}/{model}");
        draft.record(Change::SetDefaultRole {
            role,
            value: Some(qualified.clone()),
        });
        *working.default_role_mut(role) = Some(qualified);
    }
    Ok(())
}

fn row_choice<Q: Probe>(working: &Working, probe: &Q, name: &str) -> Choice {
    let connection = &working.connections[name];
    let (source, problem) = describe_auth(name, connection, probe);
    let mut label = format!("{name} ({})", connection.provider);
    if connection.disabled {
        label.push_str(" — disabled");
    }
    let detail = match problem {
        Some(problem) => format!("{source} — {problem}"),
        None => source,
    };
    Choice::new(label).detail(detail)
}

/// The readiness annotation: local checks only, never a network call — an
/// environment variable's presence, whether the store holds an item, and
/// whether the Codex cache parses and is not past its own `exp`
/// (backlog/setup-edits-connections.md).
fn describe_auth<Q: Probe>(
    name: &str,
    connection: &ExistingConnection,
    probe: &Q,
) -> (String, Option<String>) {
    match &connection.auth {
        ExistingAuth::Env { key } => {
            let source = format!("env · {key}");
            match probe.env_var(key) {
                Some(_) => (source, None),
                None => (source, Some("not set".to_string())),
            }
        }
        ExistingAuth::Keychain { .. } => {
            let item = connection.auth.item_name(name).expect("keychain auth");
            let source = format!("keychain · {item}");
            match probe.secret_value(&item) {
                Ok(Some(_)) => (source, None),
                Ok(None) => (source, Some("not in the store".to_string())),
                Err(error) => (source, Some(error)),
            }
        }
        ExistingAuth::Oauth { .. } => {
            let item = connection.auth.item_name(name).expect("oauth auth");
            let source = format!("oauth · {item}");
            match probe.secret_value(&item) {
                Ok(Some(_)) => (source, None),
                Ok(None) => (source, Some("no stored login".to_string())),
                Err(error) => (source, Some(error)),
            }
        }
        ExistingAuth::CodexCache => {
            let source = "cache · codex login".to_string();
            match probe.codex_cache() {
                Ok(_) => (source, None),
                Err(error) => (source, Some(error)),
            }
        }
    }
}
