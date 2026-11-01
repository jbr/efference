//! Reading capabilities out of a model listing, for a custom endpoint
//! (docs/transport.md).
//!
//! A named provider has a capability source this crate knows the shape of. A
//! custom endpoint has only whatever `GET {base}/models` happens to return, and
//! that varies from the bare OpenAI object — `id`, `object`, `created`,
//! `owned_by`, and nothing else — to a listing carrying context window, output
//! cap, modalities, cache pricing, and accepted sampling parameters.
//!
//! So this parse is **opportunistic and field-by-field**: each capability is read
//! if a spelling for it is present and left unknown otherwise, and a listing that
//! answers nothing is a valid answer rather than an error. Every unknown takes
//! the arm that asserts least ([`ModelCapabilities::unknown`]).
//!
//! Each spelling read here is one some endpoint actually uses; none is invented
//! for thoroughness, since a key nothing sends is untested code claiming coverage
//! it does not have. Where endpoints spell one capability differently, each
//! spelling is read.
//!
//! Deliberately **not** keyed off [`WireDialect`]: the listing's shape and the
//! dialect the endpoint serves turns in are independent axes. A bare
//! OpenAI-shaped listing can front both dialects, and a capability-rich one can
//! front a dialect this crate does not speak at all.

use super::row::{ListedModel, arrange};
use super::*;

/// One entry in a model listing. Every field is optional: this type is the union
/// of the listing shapes the parse handles, and no one endpoint fills all of it.
#[derive(Deserialize)]
struct Entry {
    id: String,
    /// The context window. `context_length` is the OpenRouter-derived spelling;
    /// `max_input_tokens` is the Anthropic-shaped one.
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    max_input_tokens: Option<u64>,
    /// The output cap, in each spelling a listing uses for it.
    #[serde(default)]
    max_output_length: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    max_completion_tokens: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    /// Modalities, flat or nested under `architecture` as OpenRouter nests them.
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    architecture: Option<Architecture>,
    /// Accepted sampling parameters, in either spelling. A listing that names them
    /// but omits `temperature` is stating that the model rejects it.
    #[serde(default)]
    supported_sampling_parameters: Option<Vec<String>>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
    #[serde(default)]
    pricing: Option<Pricing>,
    /// The display name, in either spelling, and the creation time in unix
    /// seconds: the two facts a menu shows that a capability parse does not read.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    created: Option<i64>,
    /// The date the provider retires the model (OpenAI's platform listing:
    /// `YYYY-MM-DD`). Held as raw JSON so a spelling this parse does not read
    /// cannot fail the listing; only a string is compared (`is_past_shutdown`).
    #[serde(default)]
    shutdown_date: Option<Value>,
}

#[derive(Deserialize)]
struct Architecture {
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
}

/// Cache pricing, which is how a listing discloses a prompt cache: a read price
/// means a cache exists, and a **nonzero** write price means the caller chooses
/// what gets written (an explicit-write cache). Singular and plural spellings
/// both appear — OpenRouter uses the singular — so each is read.
///
/// Prices arrive as strings, carrying more precision than a float literal would,
/// so they are compared as parsed numbers rather than by string equality
/// with `"0"`.
#[derive(Deserialize)]
struct Pricing {
    #[serde(default, alias = "input_cache_reads")]
    input_cache_read: Option<String>,
    #[serde(default, alias = "input_cache_writes")]
    input_cache_write: Option<String>,
}

fn price(value: Option<&str>) -> Option<f64> {
    value?.trim().parse().ok()
}

impl Entry {
    /// Whether the entry names a shutdown date earlier than `today`
    /// (`YYYY-MM-DD`, UTC). An entry with no date, or one in a spelling that is
    /// not a date, is not past it: nothing was said.
    fn is_past_shutdown(&self, today: &str) -> bool {
        self.shutdown_date
            .as_ref()
            .and_then(Value::as_str)
            .and_then(|date| date.get(..10))
            .is_some_and(|date| date < today)
    }

    fn capabilities(&self) -> ModelCapabilities {
        let images = self
            .input_modalities
            .as_ref()
            .or(self
                .architecture
                .as_ref()
                .and_then(|a| a.input_modalities.as_ref()))
            .map(|m| m.iter().any(|m| m == "image"));
        let parameters = self
            .supported_sampling_parameters
            .as_ref()
            .or(self.supported_parameters.as_ref());
        ModelCapabilities {
            context_length: self.context_length.or(self.max_input_tokens),
            max_output_tokens: self
                .max_output_length
                .or(self.max_output_tokens)
                .or(self.max_completion_tokens)
                .or(self.max_tokens),
            // A listing carries no reasoning support or requirement. OpenRouter
            // puts them on its singular model entry, a route a custom endpoint is
            // not assumed to have.
            thinking: ThinkingSupport::UNKNOWN,
            // Unknown modalities read as image input: a refusal is recovered
            // from, while a gate left closed gives no signal (docs/transport.md).
            supports_image_input: images.unwrap_or(true),
            // Unknown parameters read as temperature accepted, which asserts
            // nothing: the value passes through and the endpoint's own validation
            // answers for it.
            accepts_temperature: parameters.is_none_or(|p| p.iter().any(|p| p == "temperature")),
            prompt_cache: match self.pricing.as_ref() {
                None => PromptCachePolicy::None,
                Some(pricing) => match (
                    price(pricing.input_cache_read.as_deref()),
                    price(pricing.input_cache_write.as_deref()),
                ) {
                    (None, None) => PromptCachePolicy::None,
                    (_, Some(write)) if write > 0.0 => PromptCachePolicy::Explicit,
                    _ => PromptCachePolicy::Implicit,
                },
            },
            // Effort levels are validated only where a source affirmatively
            // declares them (`ModelCapabilities::validates_efforts`), which
            // excludes a custom endpoint, so leaving these unset does not
            // refuse a configured level — it passes through unvalidated.
            supported_efforts: None,
            default_effort: None,
            validates_efforts: false,
            // A listing's prices are catalog prices (`Price`), not the per-endpoint
            // cache-read pricing the keepalive check needs: that check falls back
            // to its token-ratio rule.
            prices: None,
        }
    }
}

/// The capabilities a listing declares for `model`, or [`ModelCapabilities::unknown`]
/// when the listing does not name it.
///
/// A missing entry is **not** an error. A listing can be paginated, can omit an
/// alias the endpoint nonetheless serves, and is in no case a statement about
/// what the endpoint will accept — so refusing to boot over one would turn a
/// non-authoritative document into an authorization check. The boot line says
/// which capabilities went unanswered, and the first turn is the real test
/// (docs/transport.md).
pub(super) fn capabilities_for(body: &str, model: &str) -> Result<ModelCapabilities, Error> {
    Ok(parse(body)?
        .data
        .iter()
        .find(|entry| entry.id == model)
        .map_or_else(ModelCapabilities::unknown, Entry::capabilities))
}

/// The rows a listing names, in the order the endpoint gave them and then
/// arranged newest first where it dates them. Facts are read field-by-field like
/// the capabilities above: a spelling that is present is used, and nothing is
/// inferred from one that is not. The row set is a suggestion list, never a
/// domain of legal models: an id absent here is still sent to creation normally.
pub(super) fn rows(body: &str) -> Result<Vec<ListedModel>, Error> {
    rows_where(body, |_| true)
}

/// [`rows`] without the entries whose `shutdown_date` is before `today`
/// (`YYYY-MM-DD`, UTC): the platform menu when the Codex catalog cannot be read
/// (docs/model-catalog.md). An undated entry stays.
pub(super) fn rows_not_shut_down(body: &str, today: &str) -> Result<Vec<ListedModel>, Error> {
    rows_where(body, |entry| !entry.is_past_shutdown(today))
}

/// The ids a listing names.
pub(super) fn ids(body: &str) -> Result<Vec<String>, Error> {
    Ok(parse(body)?
        .data
        .into_iter()
        .map(|entry| entry.id)
        .collect())
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    data: Vec<Entry>,
}

fn parse(body: &str) -> Result<Listing, Error> {
    serde_json::from_str(body)
        .map_err(|e| -> Error { format!("model listing is not a model listing: {e}").into() })
}

fn rows_where(body: &str, keep: impl Fn(&Entry) -> bool) -> Result<Vec<ListedModel>, Error> {
    Ok(arrange(
        parse(body)?
            .data
            .into_iter()
            .filter(|entry| keep(entry))
            .map(|entry| {
                let images = entry
                    .input_modalities
                    .as_ref()
                    .or(entry
                        .architecture
                        .as_ref()
                        .and_then(|a| a.input_modalities.as_ref()))
                    .map(|modalities| modalities.iter().any(|m| m == "image"));
                let name = entry
                    .display_name
                    .as_deref()
                    .or(entry.name.as_deref())
                    .unwrap_or_default();
                ListedModel {
                    // Some endpoints fill `created` with 0 for every entry, which
                    // is a placeholder rather than a date to sort by.
                    created: entry.created.filter(|&created| created > 0),
                    context: entry.context_length.or(entry.max_input_tokens),
                    images,
                    ..ListedModel::new(&entry.id)
                        .named(name)
                        .described(entry.description.as_deref())
                }
            })
            .collect(),
    ))
}

#[cfg(test)]
mod test;
