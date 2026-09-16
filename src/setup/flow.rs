//! The questions, in order (docs/operating.md). One connection per pass of the
//! loop: provider, name, credential source, model, spotter; then whether to add
//! another; then which connection sessions default to when there is a choice.
//!
//! What is checked live depends on what the flow can see. A key that is in this
//! shell's environment is checked without asking. One that is not can be pasted
//! for the check alone: it goes to the provider and nowhere else, and the file
//! still records only the variable name. A key pasted to be kept goes to the
//! secret store (docs/secrets.md) and the file records only the kind.

use super::*;

/// Run the questions against `existing` and return what to change. Says what it
/// finds as it goes; the caller renders and writes the result.
///
/// Two flows behind one entry point (backlog/setup-edits-connections.md): a
/// config with nothing configured yet gets the plain add path below; one that
/// already has connections gets the management list (`src/setup/manage.rs`),
/// which can also reach this same add path through its "add another" entry.
pub async fn run<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    existing: &Existing,
) -> Result<Draft, Cancelled> {
    if existing.connections.is_empty() {
        prompter.say(
            Tone::Plain,
            "efference talks to a model through a named connection: a provider plus \
             where its credential comes from. Let's set one up.",
        );
        let mut draft = Draft::default();
        add_connections(prompter, probe, existing, &mut draft).await?;
        draft.default_connection = choose_default(prompter, existing, &draft)?;
        Ok(draft)
    } else {
        super::manage::run(prompter, probe, existing).await
    }
}

/// One or more new connections, looped until the person says no more. Shared
/// by the first-run path and the management list's "add another connection".
pub(super) async fn add_connections<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    existing: &Existing,
    draft: &mut Draft,
) -> Result<(), Cancelled> {
    loop {
        let (name, connection) = one_connection(prompter, probe, existing, draft).await?;
        prompter.say(
            Tone::Good,
            &format!(
                "{name}: sessions select it as {name}/{} (spotter {name}/{}).",
                connection.model, connection.spotter
            ),
        );
        draft.connections.push((name, connection));
        if !prompter.confirm("Add another connection?", false)? {
            break;
        }
    }
    Ok(())
}

async fn one_connection<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    existing: &Existing,
    draft: &Draft,
) -> Result<(String, ConnectionDraft), Cancelled> {
    let cards: Vec<Choice> = PROVIDERS
        .iter()
        .map(|card| Choice::new(card.label).detail(card.detail))
        .collect();
    let card = &PROVIDERS[prompter.select("Which provider?", &cards)?];
    let provider = card.provider;

    let taken = |name: &str| {
        existing.connections.contains_key(name)
            || draft.connections.iter().any(|(taken, _)| taken == name)
    };
    let name = loop {
        let mut suggestion = provider.name().to_string();
        let mut n = 2;
        while taken(&suggestion) {
            suggestion = format!("{}-{n}", provider.name());
            n += 1;
        }
        let name = prompter
            .text("Name for this connection", Some(&suggestion))?
            .trim()
            .to_string();
        if name.is_empty() || name.contains('/') {
            prompter.say(Tone::Bad, "A connection name is nonempty and has no '/'.");
            continue;
        }
        if taken(&name) {
            prompter.say(
                Tone::Bad,
                &format!("{name:?} is already a connection; pick another name."),
            );
            continue;
        }
        break name;
    };

    let (auth, key) = match card.env_key {
        Some(default_key) => api_key_source(prompter, probe, provider, &name, default_key).await?,
        None => (codex_source(prompter, probe)?, None),
    };

    let model = choose_model(prompter, probe, card, key.as_deref()).await?;
    let spotter = choose_spotter(prompter, probe, card, key.as_deref()).await?;
    Ok((
        name,
        ConnectionDraft {
            provider,
            auth,
            model,
            spotter,
        },
    ))
}

/// Where an API key comes from, and the key itself when one is on hand for the
/// live checks. The second choice keeps a pasted key in the secret store under
/// `inference:<connection>`; it is listed as unavailable, with the store's own
/// reason, when there is no store to write.
pub(super) async fn api_key_source<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    provider: Provider,
    connection: &str,
    default_key: &str,
) -> Result<(AuthSource, Option<String>), Cancelled> {
    let store = probe.secret_store();
    let keep = match &store {
        Ok(description) => Choice::new("Paste it now, kept in the OS keychain")
            .detail(format!("stored in {description}")),
        Err(why) => Choice::new("Paste it now, kept in the OS keychain").unavailable(why.clone()),
    };
    let choices = [
        Choice::new("An environment variable").detail("read from the host's environment at launch"),
        keep,
    ];
    if prompter.select("Where will the API key come from?", &choices)? == 1 {
        return keychain_source(prompter, probe, provider, connection).await;
    }
    let variable = loop {
        let variable = prompter
            .text("Variable name", Some(default_key))?
            .trim()
            .to_string();
        if variable.is_empty() || variable.contains('=') {
            prompter.say(Tone::Bad, "A variable name is nonempty and has no '='.");
            continue;
        }
        break variable;
    };
    let key = match probe.env_var(&variable) {
        Some(value) => {
            prompter.say(Tone::Good, &format!("{variable} is set in this shell."));
            Some(value)
        }
        None => {
            prompter.say(
                Tone::Note,
                &format!(
                    "{variable} is not set in this shell. The host needs it in its \
                     environment when it launches; nothing here changes that."
                ),
            );
            if prompter.confirm(
                "Paste the key now to check it and browse models? It is sent to the \
                 provider and not saved.",
                false,
            )? {
                let pasted = prompter.secret("API key")?;
                (!pasted.trim().is_empty()).then(|| pasted.trim().to_string())
            } else {
                None
            }
        }
    };
    let key = match key {
        Some(key) => check_key(prompter, probe, provider, key).await?,
        None => None,
    };
    Ok((AuthSource::Env { key: variable }, key))
}

/// A pasted key, checked and then kept: the store is written only after the
/// provider's answer, so a rejected key is kept only on request. The item name
/// is said so the person can find it in the OS's own UI.
async fn keychain_source<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    provider: Provider,
    connection: &str,
) -> Result<(AuthSource, Option<String>), Cancelled> {
    let pasted = loop {
        let pasted = prompter.secret("API key")?;
        let pasted = pasted.trim().to_string();
        if pasted.is_empty() {
            prompter.say(Tone::Bad, "Nothing pasted.");
            continue;
        }
        break pasted;
    };
    let checked = check_key(prompter, probe, provider, pasted.clone()).await?;
    let name = crate::secrets::inference_name(connection);
    match probe.store_secret(&name, &pasted) {
        Ok(()) => prompter.say(Tone::Good, &format!("Kept as `{name}`.")),
        Err(error) => {
            prompter.say(Tone::Bad, &format!("Could not keep the key: {error}"));
            return Err(Cancelled);
        }
    }
    Ok((AuthSource::Keychain, checked))
}

/// Ask the provider about `key`. `Some(key)` when it is usable for the model
/// checks that follow; `None` when it was rejected and the person chose to
/// record the connection regardless.
async fn check_key<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    provider: Provider,
    key: String,
) -> Result<Option<String>, Cancelled> {
    match probe.check_key(provider, &key).await {
        Ok(Some(report)) => {
            let label = report
                .label
                .map(|label| format!(" ({label})"))
                .unwrap_or_default();
            prompter.say(
                Tone::Good,
                &format!("{} accepted the key{label}.", provider),
            );
            Ok(Some(key))
        }
        Ok(None) => Ok(Some(key)),
        Err(error) => {
            prompter.say(
                Tone::Bad,
                &format!("{provider} rejected the key: {}", describe(&error)),
            );
            if !prompter.confirm("Record this connection anyway?", false)? {
                return Err(Cancelled);
            }
            Ok(None)
        }
    }
}

pub(super) fn codex_source<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
) -> Result<AuthSource, Cancelled> {
    match probe.codex_cache() {
        Ok(account) => prompter.say(
            Tone::Good,
            &format!("Found a Codex login for account {account}."),
        ),
        Err(error) => {
            prompter.say(Tone::Bad, &format!("No Codex login found: {error}"));
            prompter.say(
                Tone::Note,
                "Run `codex login` before starting a host; the host reads the cache at launch.",
            );
            if !prompter.confirm("Record this connection anyway?", true)? {
                return Err(Cancelled);
            }
        }
    }
    Ok(AuthSource::CodexCache)
}

/// The main model: from a live listing when the provider offers one and a key
/// is on hand, else typed with the card's hint prefilled. A checked model is
/// described; an unchecked one is taken as written.
async fn choose_model<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    card: &ProviderCard,
    key: Option<&str>,
) -> Result<String, Cancelled> {
    let listing = match key {
        Some(key) => probe
            .list_models(card.provider, key)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    loop {
        let model = if listing.is_empty() {
            prompter
                .text("Model for the agent", Some(card.model_hint))?
                .trim()
                .to_string()
        } else {
            let mut choices: Vec<Choice> = listing
                .iter()
                .map(|entry| Choice::new(&entry.id).detail(&entry.display_name))
                .collect();
            choices.push(Choice::new("Type a model id"));
            let picked = prompter.select("Model for the agent", &choices)?;
            match listing.get(picked) {
                Some(entry) => entry.id.clone(),
                None => prompter
                    .text("Model id", Some(card.model_hint))?
                    .trim()
                    .to_string(),
            }
        };
        if model.is_empty() {
            prompter.say(Tone::Bad, "A model id is required.");
            continue;
        }
        if check_model(prompter, probe, card.provider, key, &model).await? {
            return Ok(model);
        }
    }
}

async fn choose_spotter<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    card: &ProviderCard,
    key: Option<&str>,
) -> Result<String, Cancelled> {
    prompter.say(
        Tone::Plain,
        "The spotter reviews tool calls the permission rules did not clear. It is \
         volume work, so a cheap model suits it.",
    );
    loop {
        let spotter = prompter
            .text(
                "Model for the spotter",
                Some(card.provider.default_spotter_model()),
            )?
            .trim()
            .to_string();
        if spotter.is_empty() {
            prompter.say(Tone::Bad, "A model id is required.");
            continue;
        }
        if check_model(prompter, probe, card.provider, key, &spotter).await? {
            return Ok(spotter);
        }
    }
}

/// `Ok(true)` to keep `model`: it checked out, could not be checked, or the
/// person kept it after a failed check. `Ok(false)` to ask again.
async fn check_model<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    provider: Provider,
    key: Option<&str>,
    model: &str,
) -> Result<bool, Cancelled> {
    let Some(key) = key else {
        return Ok(true);
    };
    match probe.check_model(provider, key, model).await {
        Ok(report) => {
            let output = report
                .max_output_tokens
                .map(|tokens| format!(", {} output", tokens_short(tokens)))
                .unwrap_or_default();
            prompter.say(
                Tone::Good,
                &format!(
                    "{model}: {} context{output}.",
                    tokens_short(report.context_length)
                ),
            );
            Ok(true)
        }
        Err(error) => {
            prompter.say(
                Tone::Bad,
                &format!("{provider} does not know {model}: {}", describe(&error)),
            );
            prompter.confirm("Keep it anyway?", false)
        }
    }
}

/// Which connection `[defaults]` points at. One new connection on an empty file
/// needs no question. An existing default is kept unless the person says
/// otherwise; several candidates are a menu.
pub(super) fn choose_default<P: Prompter>(
    prompter: &mut P,
    existing: &Existing,
    draft: &Draft,
) -> Result<Option<String>, Cancelled> {
    if let Some(current) = &existing.default_model {
        let question = format!("Sessions currently default to {current}. Change that?");
        if !prompter.confirm(&question, false)? {
            return Ok(None);
        }
    }
    if draft.connections.len() == 1 {
        return Ok(Some(draft.connections[0].0.clone()));
    }
    let choices: Vec<Choice> = draft
        .connections
        .iter()
        .map(|(name, connection)| Choice::new(name).detail(&connection.model))
        .collect();
    let picked = prompter.select("Which connection should sessions use by default?", &choices)?;
    Ok(Some(draft.connections[picked].0.clone()))
}

/// `200k`, `1.05M`: enough precision to recognize a model by, no more.
fn tokens_short(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let millions = tokens as f64 / 1_000_000.0;
        if (millions - millions.round()).abs() < 0.005 {
            format!("{}M", millions.round())
        } else {
            format!("{millions:.2}M")
        }
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// A provider error as one line: the status the request came back with and the
/// message inside the JSON body when there is one, without the URL and the
/// braces. `GET https://… returned 401 Unauthorized: {"error":{"message":"bad
/// key"}}` reads as `401 Unauthorized: bad key`.
pub(super) fn describe(error: &str) -> String {
    let (head, body) = match error.find('{') {
        Some(at) => (&error[..at], Some(&error[at..])),
        None => (error, None),
    };
    let head = head.trim().trim_end_matches(':').trim();
    let head = match head.find(" returned ") {
        Some(at) => &head[at + " returned ".len()..],
        None => head,
    };
    let message = body
        .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .and_then(|json| {
            let error = json.get("error")?;
            error
                .get("message")
                .or_else(|| json.get("message"))
                .and_then(|message| message.as_str())
                .map(str::to_string)
                .or_else(|| error.as_str().map(str::to_string))
        });
    match message {
        Some(message) if !head.is_empty() => format!("{head}: {message}"),
        Some(message) => message,
        None => head.lines().next().unwrap_or(head).to_string(),
    }
}
