//! The per-request usage record, `usage.jsonl` (docs/log-and-projection.md): one line
//! per completed upstream request, written by the session that made it and read back
//! by a restarted host, the `cache` verb, and a dormant session's row
//! (docs/control-api.md).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::api::{CacheMiss, Usage};
use crate::log::RequestOrigin;

/// The usage record's file name in a session directory.
pub const USAGE_NAME: &str = "usage.jsonl";

/// One line of `usage.jsonl`: when, which shape, the upstream's own id for the
/// request, the model that served it, and the four token counts. The model is
/// per line rather than per file because the session's model can change
/// mid-log, so only a per-request record attributes every entry.
///
/// Readable as well as writable: the `cache` verb (docs/operating.md) replays a
/// recorded session's lines back through [`ColdStreak`](super::ColdStreak), so the offline report and
/// the live counter share one classification. Missing counters read as zero, which
/// is what a line written before a field existed means.
#[derive(Serialize, Deserialize)]
pub struct UsageLine {
    pub at_ms: u128,
    pub origin: RequestOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// The request read the main prefix although its origin does not imply it: a
    /// result triage on the agent's own client (`triage = "Shared"`,
    /// docs/transport.md). Omitted when false, and absent on lines written before the
    /// field existed, which read as false. Not written for origins that always touch the
    /// main prefix ([`RequestOrigin::touches_main_prefix`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub main_prefix: bool,
    /// The upstream that served the request, when the response named one
    /// (`Receipt::served_by`, docs/transport.md). Absent on lines written before the
    /// field existed and on providers that do not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_by: Option<String>,
    /// The cost the response billed inline, in USD (`Receipt::cost`). Absent when the
    /// response carried none; such a request is priced by the generation lookup, if
    /// armed ([`Session::price_request`](crate::daemon::Session::price_request)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The provider's own attribution of this request's cache miss, when it named one
    /// ([`CacheMiss`], docs/transport.md). Absent on most lines: a request the provider
    /// saw no divergence in carries nothing, and a provider with no such feature never
    /// does. In the provider's own spelling (`"cache_miss":{"type":"tools_changed",…}`)
    /// so the record and a raw response agree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_miss: Option<CacheMiss>,
}

impl UsageLine {
    /// The line for one completed request at `at_ms`. `main_prefix` says the request
    /// ran on the agent's own client; it is recorded only where `origin` does not
    /// already imply it.
    pub fn new(
        origin: RequestOrigin,
        at_ms: u128,
        main_prefix: bool,
        request: &crate::api::RequestUsage,
    ) -> Self {
        Self {
            at_ms,
            origin,
            id: request.id.clone(),
            model: request.model.clone(),
            input: request.usage.input_tokens,
            output: request.usage.output_tokens,
            cache_read: request.usage.cache_read_input_tokens,
            cache_write: request.usage.cache_creation_input_tokens,
            main_prefix: main_prefix && !origin.touches_main_prefix(),
            served_by: request.receipt.served_by.clone(),
            cost_usd: request.receipt.cost.map(|c| c.total_usd),
            cache_miss: request.cache_miss.clone(),
        }
    }

    /// Whether this request refreshed the main prefix: its origin always does, or the
    /// line records that it ran on the agent's own client.
    pub fn touches_main_prefix(&self) -> bool {
        self.main_prefix || self.origin.touches_main_prefix()
    }

    /// The four token counts as the request's [`Usage`].
    pub fn usage(&self) -> Usage {
        Usage {
            input_tokens: self.input,
            output_tokens: self.output,
            cache_read_input_tokens: self.cache_read,
            cache_creation_input_tokens: self.cache_write,
        }
    }

    /// Every line of the record at `path` that parses; a line that does not is
    /// skipped. A missing file is an empty
    /// record rather than an error: a session that made no requests, or one from
    /// before the record existed.
    pub fn read(path: &Path) -> std::io::Result<Vec<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }
}

/// The last main-loop request's usage: the latest [`RequestOrigin::Turn`] line. Its
/// [`Usage::prompt_tokens`] is what reading the context uncached would cost — the
/// `cold` topic's `context_tokens` (docs/control-api.md).
pub fn last_turn_usage(lines: &[UsageLine]) -> Option<Usage> {
    lines
        .iter()
        .rfind(|line| line.origin == RequestOrigin::Turn)
        .map(UsageLine::usage)
}
