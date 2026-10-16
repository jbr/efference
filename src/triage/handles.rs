//! Result shapes the triage does not get to decide about (docs/triage.md).
//!
//! A result whose value is a machine-precise handle the model must reproduce
//! exactly cannot pass through a summarizer: a paraphrase keeps the meaning and
//! destroys the function, and nothing in the result tells the triage that. So the
//! decision is not the fork's to make — a result carrying one of the shapes below
//! is never offered for a verdict, and its raw text reaches the canonical wire
//! byte-for-byte.
//!
//! One entry per known shape in [`SHAPES`]. A second handle-bearing format is one
//! more entry, not another mechanism.

use super::*;

/// A handle format: a sentinel followed by a fixed-width hex digest. The digest
/// width and the hex alphabet are what keep the match narrow — a result that
/// merely mentions the sentinel in prose is not a handle-bearing result.
pub(super) struct Shape {
    /// How the shape is named in the bypass breadcrumb the `Triage` records.
    name: &'static str,
    sentinel: &'static str,
    digits: usize,
}

impl Shape {
    /// Whether `text` carries this shape anywhere in it. The handle is quoted
    /// inside a sentence of refusal prose, so position is not part of the match;
    /// the digest is. A trailing alphanumeric rules out a longer token that
    /// happens to open with the same bytes.
    fn matches(&self, text: &str) -> bool {
        text.match_indices(self.sentinel).any(|(at, _)| {
            let rest = &text.as_bytes()[at + self.sentinel.len()..];
            rest.len() >= self.digits
                && rest[..self.digits].iter().all(u8::is_ascii_hexdigit)
                && rest
                    .get(self.digits)
                    .is_none_or(|b| !b.is_ascii_alphanumeric())
        })
    }
}

/// The known handle-bearing result shapes.
///
/// `fs-mcp write retry token`: a refused `write` (the path exists, or `overwrite`
/// and `append` were both set) preserves the rejected body server-side under
/// `write-retry-<16 hex>` and tells the caller to repeat the call with that token
/// as `contents`. The token is the only way to reach the preserved body, so a
/// refusal whose token was paraphrased away costs the whole body again on the
/// next attempt — measured at ~26 KB across three re-sends of one file.
const SHAPES: &[Shape] = &[Shape {
    name: "fs-mcp write retry token",
    sentinel: "write-retry-",
    digits: 16,
}];

/// The name of the first handle shape `text` carries, or `None` if it carries
/// none — in which case the result is the fork's to triage as usual.
pub(super) fn verbatim_handle(text: &str) -> Option<&'static str> {
    SHAPES
        .iter()
        .find(|shape| shape.matches(text))
        .map(|s| s.name)
}

/// The known-shape rule (docs/triage.md): the name of the verbatim handle
/// this result carries, if any. A handle-bearing result is **bypassed** — kept out
/// of the ask, decided by the harness, and delivered raw — because the triage has
/// nothing in front of it that says the text is unsummarizable.
pub(super) fn bypassed(pending: &Pending<'_>) -> Option<&'static str> {
    if is_reply(pending.call) {
        return Some(REPLY_SHAPE);
    }
    verbatim_handle(&pending.raw.content)
}

/// How an `ask` reply is named in the bypass breadcrumb.
const REPLY_SHAPE: &str = "operator reply";

/// Whether `call` is an `ask`, whose result is the operator's own words
/// (docs/process-monitor.md). Operator words are never the triage's to summarize.
pub(super) fn is_reply(call: &ToolCall) -> bool {
    call.name == crate::process_monitor::ASK
}

/// Whether this triage holds an `ask` reply and nothing else the triage would decide
/// about, so the batch reaches the triage with every result already decided
/// ([`bypassed_triage`]). The loop then triages the inbox itself, after the results are
/// logged, because the triage that would have carried it does not run.
pub fn reply_only_triage(pending: &[Pending<'_>]) -> bool {
    pending.iter().any(|p| is_reply(p.call)) && pending.iter().all(|p| bypassed(p).is_some())
}

/// Each pending result's label in the ask (docs/triage.md), in call order:
/// `r1`, `r2`, … across the results the shape rule did not bypass, `None` for a bypassed
/// one. This is the verdict's coordinate system — the triage view heads each block with its
/// label and the verdict answers by label, mapped back to the call harness-side. The
/// provider's call id never reaches the ask: it is a 24–36 character opaque string, and
/// copying it back verbatim is a transcription task models fail at by dropping or
/// altering characters, which withholds output the model had decided on.
///
/// A bypassed result gets no label, so it appears in neither the ask nor the coverage
/// check — the triage is never asked to decide something the harness already decided.
pub(super) fn labels(pending: &[Pending<'_>]) -> Vec<Option<String>> {
    let mut n = 0;
    pending
        .iter()
        .map(|p| {
            bypassed(p).is_none().then(|| {
                n += 1;
                format!("r{n}")
            })
        })
        .collect()
}

/// The labels this triage asks about: [`labels`] without the bypassed gaps.
pub(super) fn offered_labels(pending: &[Pending<'_>]) -> Vec<String> {
    labels(pending).into_iter().flatten().collect()
}

/// The harness's verdict on a bypassed result (docs/triage.md):
/// [`WireState::Raw`], so the whole body — handle included — projects verbatim.
/// `origin` is `Fallback` because no model produced it; `raw_keep` names the shape,
/// which is what separates a bypass from a failed triage in the log after the fact.
pub(super) fn bypass_verdict(shape: &str) -> TriageOutcome {
    TriageOutcome {
        wire: WireState::Raw,
        origin: Origin::Fallback,
        raw_keep: format!("bypassed triage: {shape} — delivered verbatim"),
    }
}

/// The whole triage's outcome when the shape rule bypassed **every** result: decided
/// without a request at all, since there is nothing left to ask about — a skipped
/// verdict is also a skipped round-trip. `None` means at least one result is still
/// the fork's, and the triage runs.
///
/// The triage-scoped riders (inbox, background, surfacing) simply wait for the next
/// triage: an undispositioned notification stays active and is re-shown there
/// (docs/inbox.md), so nothing is lost by not opening a triage for them here.
pub(super) fn bypassed_triage(pending: &[Pending<'_>]) -> Option<HoldOutcome> {
    let results: Vec<TriageOutcome> = pending
        .iter()
        .map_while(|p| bypassed(p).map(bypass_verdict))
        .collect();
    (results.len() == pending.len()).then(|| HoldOutcome {
        results,
        ..HoldOutcome::default()
    })
}

#[cfg(test)]
mod test;
