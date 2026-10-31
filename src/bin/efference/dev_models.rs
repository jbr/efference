//! `efference dev models {connection}` — read a configured connection's model
//! listing, live (docs/operating.md, docs/transport.md).
//!
//! Which models a provider names, and under which credential, is what this
//! instrument reads: it resolves the connection exactly as a session would, GETs
//! the listing route with that connection's credential, and prints the status and
//! the entries as they arrived. It reads the route directly rather than through
//! the Codex arm's catalog cache (`api/provider/codex.rs`), so what it prints is
//! the provider's answer now.

use super::*;

use efference::api::{Endpoint, Provider};
use serde_json::Value;

/// How much of a body this build does not recognize to print before eliding the
/// rest; `--raw` prints all of it.
const BODY_PREVIEW_BYTES: usize = 1000;

/// What past sessions picked, decayed: the recent menu (per connection, or
/// every connection) and the omitted-branch default for one project scope —
/// the same answers creation and the pickers read, printed for inspection.
pub(crate) fn history(
    cwd: Option<PathBuf>,
    config: Option<PathBuf>,
    connection: Option<String>,
) -> CliResult {
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir()?,
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let store = match home.as_deref() {
        Some(home) => efference::history::History::user(home),
        None => efference::history::History::disabled(),
    };
    let scope = efference::history::Scope::for_request(&cwd, config.as_deref());
    let now = efference::history::now();
    println!("scope: {}", scope.cwd.display());
    println!("config: {}", scope.config_key);
    match connection.as_deref() {
        Some(connection) => {
            let recents = store.recent(&scope, connection, now, 10);
            if recents.is_empty() {
                println!("no past picks on {connection}");
            }
            for candidate in &recents {
                println!("  {:>8.2}  {}", candidate.mass, candidate.model);
            }
        }
        None => {
            if let Some(default) = store.default(&scope, now) {
                println!(
                    "default: {}/{} ({:.2})",
                    default.connection, default.model, default.mass
                );
            } else {
                println!("default: none (no past picks apply)");
            }
        }
    }
    Ok(())
}

/// Read the listing routes for one configured connection and print what each
/// answered.
pub(crate) async fn models(connection: &str, urls: &[String], raw: bool) -> CliResult {
    let registry = inference::registry()?;
    let (provider, endpoint) = registry.endpoint(connection)?;
    println!("{connection} — {provider}");

    for url in candidate_urls(provider, &endpoint, urls) {
        println!("\nGET {url}");
        let response = endpoint.probe(&url).await?;
        let status = response.status.map_or_else(
            || "no status".to_string(),
            |status| {
                if (200..300).contains(&status) {
                    status.to_string()
                } else {
                    format!("{status}, not success")
                }
            },
        );
        let bytes = response.body.len();
        match &response.etag {
            Some(etag) => println!("  {status} · {bytes} bytes · etag {etag}"),
            None => println!("  {status} · {bytes} bytes"),
        }
        // `--raw` answers with the body in place of the reading, so a
        // recognized listing pipes to a JSON tool as readily as an error does.
        if raw {
            println!("{}", response.body);
            continue;
        }
        match response
            .status
            .filter(|status| (200..300).contains(status))
            .and_then(|_| parse_listing(&response.body))
        {
            Some(entries) if !entries.is_empty() => {
                println!("  {} models:", entries.len());
                let width = entries
                    .iter()
                    .map(|(id, _)| id.len())
                    .max()
                    .unwrap_or_default();
                for (id, label) in &entries {
                    println!("    {id:<width$}  {label}");
                }
            }
            // A 2xx this build cannot read is a shape question, not a listing:
            // show enough of it to ask.
            _ => println!("{}", preview(&response.body)),
        }
    }
    Ok(())
}

/// The routes one run reads: the provider's own listing, the other OpenAI arm's
/// beside it — the two arms' catalogs are one question, and a single run then
/// says whether each route answers the other's credential — and whatever `--url`
/// adds.
fn candidate_urls(provider: Provider, endpoint: &Endpoint, extra: &[String]) -> Vec<String> {
    let mut urls: Vec<String> = endpoint.models_url().into_iter().collect();
    let other = match provider {
        Provider::OpenAI => Provider::OpenAICodex,
        Provider::OpenAICodex => Provider::OpenAI,
        Provider::Anthropic | Provider::OpenRouter | Provider::TypeSafe | Provider::Custom(_) => {
            urls.extend(extra.iter().cloned());
            return urls;
        }
    };
    urls.extend(other.models_url(None));
    urls.extend(extra.iter().cloned());
    urls
}

/// The entries of a listing whose shape this build recognizes — the OpenAI and
/// Anthropic spelling (`data[].id`) and the Codex backend's own
/// (`models[].slug`) — as `(id, display name)`, or `None` for a body it does not.
pub(crate) fn parse_listing(body: &str) -> Option<Vec<(String, String)>> {
    let json: Value = serde_json::from_str(body).ok()?;
    let entries = json
        .as_object()?
        .get("data")
        .or_else(|| json.get("models"))?
        .as_array()?;
    entries
        .iter()
        .map(|entry| {
            let id = entry
                .get("id")
                .or_else(|| entry.get("slug"))?
                .as_str()?
                .to_string();
            let label = entry
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            Some((id, label))
        })
        .collect()
}

/// The head of a body, with the elision marked so a truncated answer is not read
/// as a whole one.
fn preview(body: &str) -> String {
    let mut chars = body.char_indices();
    let cut = chars
        .nth(BODY_PREVIEW_BYTES)
        .map_or(body.len(), |(at, _)| at);
    if cut == body.len() {
        return body.to_string();
    }
    format!("{}… ({} bytes total)", &body[..cut], body.len())
}
