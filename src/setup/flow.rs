//! The questions, in order (docs/operating.md). One connection per pass of the
//! loop: provider, name, credential source; then whether to add another. No
//! role is asked: the main model has no default (the new-session form names
//! the current one), and the spotter and triage each have a default that runs
//! — the defaults editor reaches those two later.
//!
//! What is checked live depends on what the flow can see. A key that is in this
//! shell's environment is checked without asking. One that is not can be pasted
//! for the check alone: it goes to the provider and nowhere else, and the file
//! still records only the variable name. A key pasted to be kept goes to the
//! secret store (docs/secrets.md) and the file records only the kind.

use super::*;

mod chatgpt;
pub(super) use chatgpt::{chatgpt_source, codex_cache_source, oauth_source};

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
        // A decision-model provider backs the spotter and never takes a turn,
        // so a file holding only that has no agent to start (docs/transport.md).
        while !draft
            .connections
            .iter()
            .any(|(_, connection)| connection.provider.serves_inference())
        {
            prompter.say(
                Tone::Note,
                "That provider serves the permission spotter only. Sessions also need \
                 a provider for the agent itself.",
            );
            add_connections(prompter, probe, existing, &mut draft).await?;
        }
        // A first run ends with no default model: the new-session form names
        // the current one, prefilled from past picks where they apply
        // (docs/operating.md).
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
            &format!("{name}: sessions select a model on it as {name}/<model>."),
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
    // A described endpoint answers what a named provider derives: the dialect it
    // speaks and where it lives (docs/transport.md). Asked before the name,
    // because the host makes a better name suggestion than the word "custom".
    let described = match card.provider {
        Some(_) => None,
        None => Some(describe_endpoint(prompter)?),
    };
    let (provider, base_url) = match (&card.provider, &described) {
        (Some(provider), _) => (*provider, None),
        (None, Some((dialect, base))) => (Provider::Custom(*dialect), Some(base.clone())),
        (None, None) => unreachable!("a card without a provider is asked to describe one"),
    };
    let target = match &base_url {
        Some(base) => Target {
            provider,
            base: Some(base.clone()),
        },
        None => Target::named(provider),
    };

    let taken = |name: &str| {
        existing.connections.contains_key(name)
            || draft.connections.iter().any(|(taken, _)| taken == name)
    };
    // `custom-messages` is a poor connection name; the endpoint's host is what
    // the operator already calls this thing.
    let stem = base_url
        .as_deref()
        .and_then(host_stem)
        .unwrap_or_else(|| provider.name().to_string());
    let name = loop {
        let mut suggestion = stem.clone();
        let mut n = 2;
        while taken(&suggestion) {
            suggestion = format!("{stem}-{n}");
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

    // The key the checks used is dropped here: adding a connection asks for no
    // model, so nothing downstream needs it, and setup keeps no credential past
    // the check it was given for.
    let (auth, _) = match (card.env_key, card.provider) {
        (Some(default_key), _) => {
            api_key_source(prompter, probe, &target, &name, default_key).await?
        }
        // A described endpoint takes a key like any other; the variable name is
        // suggested from the connection rather than from a convention that does
        // not exist for it.
        (None, None) => {
            let suggested = default_env_key(&name);
            api_key_source(prompter, probe, &target, &name, &suggested).await?
        }
        (None, Some(_)) => (chatgpt_source(prompter, probe, &name).await?, None),
    };

    Ok((
        name,
        ConnectionDraft {
            provider,
            base_url,
            auth,
            disabled: false,
        },
    ))
}

/// The dialect and base URL for an endpoint this build knows no name for. The
/// dialect first: it is the choice that decides what the endpoint has to be,
/// and a person who cannot answer it has nothing to type in the URL either.
fn describe_endpoint<P: Prompter>(prompter: &mut P) -> Result<(WireDialect, String), Cancelled> {
    let dialects = [
        Choice::new("Anthropic Messages").detail("POST <base>/messages, as Anthropic's API"),
        Choice::new("OpenAI Responses").detail("POST <base>/responses, as OpenAI's API"),
        Choice::new("Chat Completions")
            .detail("POST <base>/chat/completions, as most other hosted and local servers"),
        Choice::new("System One decisions").detail(
            "a decision model (jev and kin) — backs the spotter only; POST to the URL as given",
        ),
    ];
    let dialect = match prompter.select("Which API does it speak?", &dialects)? {
        0 => WireDialect::Messages,
        1 => WireDialect::Responses,
        2 => WireDialect::Completions,
        _ => WireDialect::SystemOne,
    };
    // A System One provider's route is whatever its docs say (`/v1/systemone`,
    // `/decisions/v1/systemone`, a per-model run URL), so the URL is taken whole.
    let (label, example) = match dialect {
        WireDialect::SystemOne => ("Request URL", "https://api.example.com/v1/systemone"),
        WireDialect::Messages | WireDialect::Responses | WireDialect::Completions => {
            ("Base URL", "https://api.example.com/v1")
        }
    };
    let base = loop {
        let base = prompter
            .text(label, Some(example))?
            .trim()
            .trim_end_matches('/')
            .to_string();
        if base.starts_with("https://") || base.starts_with("http://") {
            break base;
        }
        prompter.say(
            Tone::Bad,
            "A base URL starts with https:// and ends where the route begins, \
             as in https://api.example.com/v1.",
        );
    };
    Ok((dialect, base))
}

/// The host's own name, for suggesting a connection name: `api.meta.ai/v1` reads
/// as `meta`. `None` when nothing recognizable can be pulled out, which leaves
/// the provider's name as the suggestion.
fn host_stem(base: &str) -> Option<String> {
    let host = base
        .split_once("://")?
        .1
        .split(['/', ':'])
        .next()?
        .to_ascii_lowercase();
    let labels: Vec<&str> = host
        .split('.')
        .filter(|label| !label.is_empty() && *label != "www" && *label != "api")
        .collect();
    // The label before the public suffix, which is the name people use. A
    // two-label host (`meta.ai`) leaves the first; a longer one drops the tail.
    let stem = if labels.len() > 1 {
        labels[labels.len() - 2]
    } else {
        labels.first().copied()?
    };
    let stem: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!stem.is_empty()).then_some(stem)
}

/// The environment variable to suggest for a described endpoint's key, from the
/// connection's name: `meta` reads as `META_API_KEY`.
fn default_env_key(connection: &str) -> String {
    let stem: String = connection
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{stem}_API_KEY")
}

/// Where an API key comes from, and the key itself when one is on hand for the
/// live checks. The second choice keeps a pasted key in the secret store under
/// `inference:<connection>`; it is listed as unavailable, with the store's own
/// reason, when there is no store to write.
pub(super) async fn api_key_source<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    target: &Target,
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
        return keychain_source(prompter, probe, target, connection).await;
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
        Some(key) => check_key(prompter, probe, connection, target, key).await?,
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
    target: &Target,
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
    let checked = check_key(prompter, probe, connection, target, pasted.clone()).await?;
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
    who: &str,
    target: &Target,
    key: String,
) -> Result<Option<String>, Cancelled> {
    match probe.check_key(target, &key).await {
        Ok(Some(report)) => {
            let label = report
                .label
                .map(|label| format!(" ({label})"))
                .unwrap_or_default();
            prompter.say(Tone::Good, &format!("{who} accepted the key{label}."));
            Ok(Some(key))
        }
        Ok(None) => Ok(Some(key)),
        Err(error) => {
            prompter.say(
                Tone::Bad,
                &format!("{who} rejected the key: {}", describe(&error)),
            );
            if !prompter.confirm("Record this connection anyway?", false)? {
                return Err(Cancelled);
            }
            Ok(None)
        }
    }
}

/// Pick a model for `connection`: past picks on it first — most decayed
/// weight first, the only entries for a zero-inventory endpoint — then the
/// live listing when the endpoint offers one and a key is on hand, then the
/// typed-ID escape hatch, which stays in every case. A checked model is
/// described; an unchecked one is taken as written. `question` is what the
/// model is *for*, so the same picker serves each default role.
///
/// A listing entry whose display name is its own id — every described endpoint's,
/// since no listing carries display names — is shown once rather than twice.
/// History entries merge with the listing rather than shadowing it: a model in
/// both keeps the listing's row, so the check state reads the same.
pub(super) async fn choose_model<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    question: &str,
    connection: &str,
    target: &Target,
    hint: Option<&str>,
    key: Option<&str>,
) -> Result<String, Cancelled> {
    // Setup runs wherever the operator invoked it, so recents scope to this
    // process's root — the project the new config will serve. A root that will
    // not read scopes nothing: the menu falls back to listing plus typed.
    let scope = std::env::current_dir()
        .map(|cwd| crate::history::Scope::for_request(&cwd, None))
        .ok();
    let store = match std::env::var_os("HOME") {
        Some(home) => crate::history::History::user(std::path::Path::new(&home)),
        None => crate::history::History::disabled(),
    };
    let recents: Vec<String> = scope
        .map(|scope| {
            store
                .recent(&scope, connection, crate::history::now(), 5)
                .into_iter()
                .map(|candidate| candidate.model)
                .collect()
        })
        .unwrap_or_default();
    let listing = match key {
        Some(key) => probe.list_models(target, key).await.unwrap_or_default(),
        None => Vec::new(),
    };
    loop {
        let model = if listing.is_empty() && recents.is_empty() {
            prompter.text(question, hint)?.trim().to_string()
        } else {
            let mut choices: Vec<Choice> = recents
                .iter()
                .filter(|recent| !listing.iter().any(|entry| &entry.id == *recent))
                .map(Choice::new)
                .collect();
            choices.extend(listing.iter().map(|entry| {
                let choice = Choice::new(&entry.id);
                if entry.display_name == entry.id {
                    choice
                } else {
                    choice.detail(&entry.display_name)
                }
            }));
            choices.push(Choice::new("Type a model id"));
            let picked = prompter.select(question, &choices)?;
            match choices.get(picked) {
                Some(choice) if choice.label == "Type a model id" => {
                    prompter.text("Model id", hint)?.trim().to_string()
                }
                Some(choice) => choice.label.clone(),
                None => prompter.text("Model id", hint)?.trim().to_string(),
            }
        };
        if model.is_empty() {
            prompter.say(Tone::Bad, "A model id is required.");
            continue;
        }
        if check_model(prompter, probe, target, key, &model).await? {
            return Ok(model);
        }
    }
}

/// `Ok(true)` to keep `model`: it checked out, could not be checked, or the
/// person kept it after a failed check. `Ok(false)` to ask again.
pub(super) async fn check_model<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    target: &Target,
    key: Option<&str>,
    model: &str,
) -> Result<bool, Cancelled> {
    let Some(key) = key else {
        return Ok(true);
    };
    match probe.check_model(target, key, model).await {
        Ok(report) => {
            let output = report
                .max_output_tokens
                .map(|tokens| format!(", {} output", tokens_short(tokens)))
                .unwrap_or_default();
            // An endpoint that reports no window is accepted and said so: the
            // model answered the check, which is what this step asks.
            let context = report
                .context_length
                .map(|tokens| format!("{} context", tokens_short(tokens)))
                .unwrap_or_else(|| "context window not reported".to_string());
            prompter.say(Tone::Good, &format!("{model}: {context}{output}."));
            Ok(true)
        }
        Err(error) => {
            prompter.say(
                Tone::Bad,
                &format!(
                    "{} does not know {model}: {}",
                    target.provider,
                    describe(&error)
                ),
            );
            prompter.confirm("Keep it anyway?", false)
        }
    }
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
