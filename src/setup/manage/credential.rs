//! Rename and the per-kind credential edits (backlog/setup-edits-connections.md:
//! "The menu is per kind, not uniform"): repair through the same prompt the
//! add path uses, the env→keychain move and its `op://` guard, and the
//! keychain→env reveal-then-switch.

use super::Working;
use crate::api::Provider;
use crate::setup::*;

/// What can be done about one connection's credential, per its kind.
pub(super) async fn edit_connection<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    working: &mut Working,
    draft: &mut Draft,
    name: &str,
    connection: &ExistingConnection,
) -> Result<(), Cancelled> {
    let mut choices = vec![Choice::new("Rename")];
    match &connection.auth {
        ExistingAuth::Env { key } => {
            choices.push(Choice::new("Update the credential"));
            let move_choice = match probe.env_var(key) {
                Some(value) if !looks_like_scheme_reference(&value) => {
                    Choice::new("Move the key into the keychain")
                }
                Some(value) => Choice::new("Move the key into the keychain").unavailable(format!(
                    "{key} holds a reference ({value}), not a plain key"
                )),
                None => Choice::new("Move the key into the keychain")
                    .unavailable(format!("{key} is not set in this shell")),
            };
            choices.push(move_choice);
        }
        ExistingAuth::Keychain { .. } => {
            choices.push(Choice::new("Update the credential"));
            choices.push(Choice::new(
                "Reveal the key and switch to an environment variable",
            ));
        }
        ExistingAuth::CodexCache => {
            choices.push(Choice::new("Re-authorize"));
        }
        // No move for a grant: there is no value to put in a variable, and
        // nothing to paste. Running the login again is the whole repair.
        ExistingAuth::Oauth { .. } => {
            choices.push(Choice::new("Re-authorize"));
        }
    }
    choices.push(Choice::new("Back"));
    let picked = prompter.select(&format!("Edit {name}"), &choices)?;
    match choices[picked].label.as_str() {
        "Rename" => rename_connection(prompter, probe, working, draft, name, connection).await,
        "Update the credential" => {
            update_credential(prompter, probe, working, draft, name, connection).await
        }
        "Move the key into the keychain" => {
            move_env_to_keychain(prompter, probe, working, draft, name, connection).await
        }
        "Reveal the key and switch to an environment variable" => {
            reveal_and_switch_to_env(prompter, probe, working, draft, name, connection).await
        }
        // One label, two mechanisms: the cached login is re-checked, a grant is
        // obtained again. The stored item name is the connection's unless the
        // file pinned one.
        "Re-authorize" => {
            match &connection.auth {
                ExistingAuth::Oauth { name: item } => {
                    let item = item.clone().unwrap_or_else(|| name.to_string());
                    crate::setup::flow::oauth_source(prompter, probe, &item).await?;
                }
                _ => {
                    crate::setup::flow::codex_cache_source(prompter, probe)?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Rename moves the connection's table key and rewrites `[defaults]`
/// references to it (render.rs); when the auth is keychain-backed under the
/// connection's own name, the secret store item moves first — write the new
/// item, confirm it reads back, then delete the old, so a failure strands a
/// copy rather than destroying the only one.
async fn rename_connection<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    working: &mut Working,
    draft: &mut Draft,
    name: &str,
    connection: &ExistingConnection,
) -> Result<(), Cancelled> {
    let new_name = loop {
        let typed = prompter.text("New name", Some(name))?.trim().to_string();
        if typed == name {
            return Ok(());
        }
        if typed.is_empty() || typed.contains('/') {
            prompter.say(Tone::Bad, "A connection name is nonempty and has no '/'.");
            continue;
        }
        if working.connections.contains_key(&typed) {
            prompter.say(
                Tone::Bad,
                &format!("{typed:?} is already a connection; pick another name."),
            );
            continue;
        }
        break typed;
    };
    // A stored item named after the connection moves with it; an item name the
    // file pinned explicitly stays where the file says it is.
    let moving = match &connection.auth {
        ExistingAuth::Keychain { name: None } => Some((
            crate::secrets::inference_name(name),
            crate::secrets::inference_name(&new_name),
        )),
        ExistingAuth::Oauth { name: None } => Some((
            crate::secrets::oauth_name(name),
            crate::secrets::oauth_name(&new_name),
        )),
        _ => None,
    };
    if let Some((old_item, new_item)) = moving {
        let value = match probe.secret_value(&old_item) {
            Ok(Some(value)) => value,
            Ok(None) => {
                prompter.say(
                    Tone::Bad,
                    &format!("`{old_item}` is not in the store; rename skipped."),
                );
                return Ok(());
            }
            Err(error) => {
                prompter.say(Tone::Bad, &format!("Could not read `{old_item}`: {error}"));
                return Ok(());
            }
        };
        if let Err(error) = probe.store_secret(&new_item, &value) {
            prompter.say(
                Tone::Bad,
                &format!("Could not write `{new_item}`: {error}. `{old_item}` was left in place."),
            );
            return Ok(());
        }
        match probe.secret_value(&new_item) {
            Ok(Some(confirmed)) if confirmed == value => {
                if let Err(error) = probe.delete_secret(&old_item) {
                    prompter.say(
                        Tone::Bad,
                        &format!(
                            "Kept as `{new_item}`, but could not remove the old `{old_item}`: {error}"
                        ),
                    );
                } else {
                    prompter.say(
                        Tone::Good,
                        &format!("Moved the stored credential to `{new_item}`."),
                    );
                }
            }
            _ => {
                prompter.say(
                    Tone::Bad,
                    &format!(
                        "Wrote `{new_item}` but could not read it back; `{old_item}` was left in place."
                    ),
                );
                return Ok(());
            }
        }
    }
    draft.record(Change::Rename {
        from: name.to_string(),
        to: new_name.clone(),
    });
    let prefix = format!("{name}/");
    for role in [
        DefaultRole::Model,
        DefaultRole::Spotter,
        DefaultRole::Triage,
    ] {
        if let Some(value) = working.default_role_mut(role)
            && let Some(rest) = value.strip_prefix(&prefix)
        {
            *value = format!("{new_name}/{rest}");
        }
    }
    // A keychain or oauth auth with an implicit item name now defaults to the
    // new connection's name, which is exactly what just happened above.
    let updated = connection.clone();
    working.connections.remove(name);
    working.connections.insert(new_name.clone(), updated);
    prompter.say(Tone::Good, &format!("Renamed to {new_name}."));
    Ok(())
}

/// Repair: re-run the same credential prompt the add path uses, pinned to
/// this connection's name, provider, and (unchanged) models.
async fn update_credential<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    working: &mut Working,
    draft: &mut Draft,
    name: &str,
    connection: &ExistingConnection,
) -> Result<(), Cancelled> {
    let Some(provider) = Provider::from_name(&connection.provider).ok() else {
        prompter.say(
            Tone::Bad,
            &format!(
                "Unknown provider {:?}; cannot update its credential here.",
                connection.provider
            ),
        );
        return Ok(());
    };
    let default_key = PROVIDERS
        .iter()
        .find(|card| card.provider == provider)
        .and_then(|card| card.env_key)
        .unwrap_or("API_KEY");
    let (auth, _) =
        crate::setup::flow::api_key_source(prompter, probe, provider, name, default_key).await?;
    draft.record(Change::SetAuth {
        name: name.to_string(),
        auth: auth.clone(),
    });
    if let Some(existing) = working.connections.get_mut(name) {
        existing.auth = to_existing_auth(&auth);
    }
    prompter.say(Tone::Good, "Credential updated.");
    Ok(())
}

/// The forward move: setup already has the variable's live value from the
/// readiness check, so it goes straight into the store without a paste.
/// Refused on a scheme-shaped value (docs: `op://…`), which a resolver
/// expands at read time — storing what the variable literally says would put
/// the unexpanded reference in the keychain.
async fn move_env_to_keychain<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    working: &mut Working,
    draft: &mut Draft,
    name: &str,
    connection: &ExistingConnection,
) -> Result<(), Cancelled> {
    let ExistingAuth::Env { key } = &connection.auth else {
        return Ok(());
    };
    let Some(value) = probe.env_var(key) else {
        prompter.say(Tone::Bad, &format!("{key} is not set in this shell."));
        return Ok(());
    };
    if looks_like_scheme_reference(&value) {
        prompter.say(
            Tone::Bad,
            &format!("{key} holds a reference ({value}), not a plain key; move it by hand."),
        );
        return Ok(());
    }
    let item = crate::secrets::inference_name(name);
    match probe.store_secret(&item, &value) {
        Ok(()) => {
            draft.record(Change::SetAuth {
                name: name.to_string(),
                auth: AuthSource::Keychain,
            });
            if let Some(existing) = working.connections.get_mut(name) {
                existing.auth = ExistingAuth::Keychain { name: None };
            }
            prompter.say(
                Tone::Good,
                &format!("Moved into `{item}`; {key} is no longer used for this connection."),
            );
        }
        Err(error) => prompter.say(Tone::Bad, &format!("Could not write `{item}`: {error}")),
    }
    Ok(())
}

/// The reverse move is destructive, so it is never spelled as a move: reveal
/// the stored value, switch the config to read it from a variable, and leave
/// the stored copy in place unless the operator separately asks to clear it.
async fn reveal_and_switch_to_env<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    working: &mut Working,
    draft: &mut Draft,
    name: &str,
    connection: &ExistingConnection,
) -> Result<(), Cancelled> {
    let Some(item) = connection.auth.item_name(name) else {
        return Ok(());
    };
    let secret = match probe.secret_value(&item) {
        Ok(Some(secret)) => secret,
        Ok(None) => {
            prompter.say(
                Tone::Bad,
                &format!("`{item}` is not in the store; nothing to reveal."),
            );
            return Ok(());
        }
        Err(error) => {
            prompter.say(Tone::Bad, &format!("Could not read `{item}`: {error}"));
            return Ok(());
        }
    };
    prompter.say(Tone::Note, &format!("{item}: {secret}"));
    let variable = loop {
        let typed = prompter.text("Variable name", None)?.trim().to_string();
        if typed.is_empty() || typed.contains('=') {
            prompter.say(Tone::Bad, "A variable name is nonempty and has no '='.");
            continue;
        }
        break typed;
    };
    draft.record(Change::SetAuth {
        name: name.to_string(),
        auth: AuthSource::Env {
            key: variable.clone(),
        },
    });
    if let Some(existing) = working.connections.get_mut(name) {
        existing.auth = ExistingAuth::Env {
            key: variable.clone(),
        };
    }
    prompter.say(
        Tone::Good,
        &format!(
            "{name} now reads {variable} from the environment. `{item}` is still in the store."
        ),
    );
    if prompter.confirm(&format!("Clear the stored secret `{item}`?"), false)? {
        match probe.delete_secret(&item) {
            Ok(()) => prompter.say(Tone::Good, &format!("Deleted `{item}`.")),
            Err(error) => prompter.say(Tone::Bad, &format!("Could not delete `{item}`: {error}")),
        }
    }
    Ok(())
}

pub(super) fn to_existing_auth(auth: &AuthSource) -> ExistingAuth {
    match auth {
        AuthSource::Env { key } => ExistingAuth::Env { key: key.clone() },
        AuthSource::CodexCache => ExistingAuth::CodexCache,
        AuthSource::Keychain => ExistingAuth::Keychain { name: None },
        AuthSource::Oauth => ExistingAuth::Oauth { name: None },
    }
}

/// Whether `value` reads as `<scheme>://…` (`op://…`, an `[env]` indirection
/// a resolver expands at read time) rather than a plain secret.
fn looks_like_scheme_reference(value: &str) -> bool {
    match value.find("://") {
        Some(at) if at > 0 => value[..at]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'),
        _ => false,
    }
}
