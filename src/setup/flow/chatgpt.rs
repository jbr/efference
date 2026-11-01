//! The two ways to hold a ChatGPT subscription (docs/operating.md): Codex CLI's
//! cached login, which Codex refreshes, and efference's own grant, which this
//! flow obtains and the host refreshes (docs/secrets.md). One question, because
//! the subscription is the same either way; what differs is who keeps the token
//! current.

use super::*;

/// Which of the two ways to hold a ChatGPT subscription this connection uses.
/// Both reach the same provider with the same plan; they differ in who keeps
/// the token current, so each option is listed with its own constraint rather
/// than one being the default (docs/transport.md).
///
/// An option that cannot work here is shown with its reason, as the API-key
/// question does. When neither can, the reasons are said and the flow backs
/// out: there is nothing to pick.
pub(crate) async fn chatgpt_source<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    connection: &str,
) -> Result<AuthSource, Cancelled> {
    let cache = probe.codex_cache();
    let store = probe.secret_store();
    // An unavailable entry shows its reason where the detail would go
    // (`setup/terminal.rs`), so each entry carries one or the other.
    let piggyback = Choice::new("Piggyback on Codex's login");
    let piggyback = match &cache {
        Ok(account) => piggyback.detail(format!(
            "account {account}; only Codex CLI can refresh this token, so you will \
             need to run Codex every so often to keep it fresh — pick this if you use \
             Codex regularly"
        )),
        Err(why) => piggyback.unavailable(why.clone()),
    };
    let log_in = Choice::new("Log in here");
    let log_in = match &store {
        Ok(description) => log_in.detail(format!(
            "the token is kept in {description} and refreshed here automatically; \
             Codex does not have to be installed"
        )),
        Err(why) => log_in.unavailable(why.clone()),
    };
    if let (Err(cache_why), Err(store_why)) = (&cache, &store) {
        prompter.say(
            Tone::Bad,
            &format!(
                "Neither way to hold a ChatGPT login works here. Codex's cached \
                 login: {cache_why}. Logging in here needs a secret store: {store_why}."
            ),
        );
        return Err(Cancelled);
    }
    let choices = [piggyback, log_in];
    if prompter.select("How should efference hold the login?", &choices)? == 0 {
        codex_cache_source(prompter, probe)
    } else {
        oauth_source(prompter, probe, connection).await
    }
}

/// The login efference runs and keeps: the grant goes into the secret store
/// before this returns, so the connection the file records has something to
/// read (docs/secrets.md). A failed login is offered again rather than
/// recorded, because a connection with no stored grant fails at host launch.
pub(crate) async fn oauth_source<P: Prompter, Q: Probe>(
    prompter: &mut P,
    probe: &Q,
    connection: &str,
) -> Result<AuthSource, Cancelled> {
    loop {
        prompter.say(
            Tone::Plain,
            "A browser will open on ChatGPT's login page. The URL is printed too, \
             for a machine with no browser of its own.",
        );
        let login = probe
            .chatgpt_login(connection, &mut |text| prompter.say(Tone::Plain, text))
            .await;
        match login {
            Ok(login) => {
                let plan = login.plan.map_or_default(|plan| format!(", {plan} plan"));
                prompter.say(
                    Tone::Good,
                    &format!(
                        "Logged in as account {}{plan}. Kept as `{}`.",
                        login.account_id,
                        crate::platform::secrets::oauth_name(connection)
                    ),
                );
                return Ok(AuthSource::Oauth);
            }
            Err(error) => {
                prompter.say(Tone::Bad, &format!("The login did not finish: {error}"));
                if !prompter.confirm("Try again?", true)? {
                    return Err(Cancelled);
                }
            }
        }
    }
}

/// Codex CLI's cached login, checked locally. A missing login is a note rather
/// than a stop: the operator can run `codex login` before starting a host.
pub(crate) fn codex_cache_source<P: Prompter, Q: Probe>(
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
