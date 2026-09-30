//! OpenRouter's model catalog as a two-level menu (docs/transport.md).
//!
//! `GET /api/v1/models` answers the whole text-model catalog in one body — 464
//! models and 762 KB at last read — and offers no author listing. Its
//! `model_authors` filter narrows a read but cannot enumerate authors, so the
//! author step needs the whole body regardless. The host therefore reads the
//! catalog once, holds it for [`CATALOG_TTL`], and answers both steps from that
//! copy: the authors, then one author's models.
//!
//! An author is the id's prefix (`anthropic` in `anthropic/claude-…`). A leading
//! `~` marks OpenRouter's rolling aliases (`~anthropic/claude-sonnet-latest`);
//! the author list folds them under the plain author, and the model ids stay as
//! the catalog spells them.

use super::*;

/// How long a catalog read stands. A catalog changes when a lab ships a model,
/// not within a menu's life; the Codex catalog's interval, for the same reason.
const CATALOG_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// The ceiling on one read. The body is large, so this is longer than the boot
/// path's metadata reads get.
const CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One catalog entry: the routable id and the display name.
type Entry = (String, String);

#[derive(Deserialize)]
struct Listing {
    data: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    name: String,
}

struct Cached {
    entries: std::sync::Arc<Vec<Entry>>,
    read_at: std::time::Instant,
}

/// The catalogs read in this process, by base URL. The catalog is the same for
/// every key, so the credential is not part of the identity.
fn cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Cached>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Cached>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// The author an id belongs to, alias marker removed. `None` for an id with no
/// `/`, which has no author to list under.
fn author_of(id: &str) -> Option<&str> {
    let (author, _) = id.split_once('/')?;
    Some(author.strip_prefix('~').unwrap_or(author))
}

/// The authors in `entries`, sorted by name, each labelled with its model count.
pub(super) fn authors(entries: &[Entry]) -> Vec<Entry> {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for (id, _) in entries {
        if let Some(author) = author_of(id) {
            *counts.entry(author).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|(author, count)| {
            let noun = if count == 1 { "model" } else { "models" };
            (author.to_string(), format!("{count} {noun}"))
        })
        .collect()
}

/// One author's entries in catalog order.
pub(super) fn models_of(entries: &[Entry], author: &str) -> Vec<Entry> {
    entries
        .iter()
        .filter(|(id, _)| author_of(id) == Some(author))
        .cloned()
        .collect()
}

fn parse(body: &str) -> Result<Vec<Entry>, Error> {
    let listing: Listing = serde_json::from_str(body)
        .map_err(|e| -> Error { format!("model catalog is not a model catalog: {e}").into() })?;
    Ok(listing
        .data
        .into_iter()
        .map(|row| (row.id, row.name))
        .collect())
}

/// The catalog, read through the process cache. A failed refresh serves an older
/// copy, and says so in the log; with nothing cached the failure is the caller's.
pub(super) async fn catalog(endpoint: &Endpoint) -> Result<std::sync::Arc<Vec<Entry>>, Error> {
    let key = format!("{:?}", endpoint.base);
    let held = cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).map(|c| (c.entries.clone(), c.read_at)));
    if let Some((entries, read_at)) = &held
        && read_at.elapsed() < CATALOG_TTL
    {
        return Ok(entries.clone());
    }
    let url = endpoint
        .models_url()
        .ok_or("the openrouter provider has no model catalog route")?;
    let failure: Error =
        match tokio::time::timeout(CATALOG_TIMEOUT, endpoint.authed_get(&url)).await {
            Ok(Ok(body)) => {
                let entries = std::sync::Arc::new(parse(&body)?);
                if let Ok(mut cache) = cache().lock() {
                    cache.insert(
                        key,
                        Cached {
                            entries: entries.clone(),
                            read_at: std::time::Instant::now(),
                        },
                    );
                }
                return Ok(entries);
            }
            Ok(Err(error)) => error,
            Err(_elapsed) => format!("model catalog at {url} did not answer in time").into(),
        };
    match held {
        Some((entries, read_at)) => {
            log::warn!(
                "the openrouter model catalog is {}s old and its refresh failed ({failure}); using it",
                read_at.elapsed().as_secs()
            );
            Ok(entries)
        }
        None => Err(failure),
    }
}

#[cfg(test)]
mod test;
