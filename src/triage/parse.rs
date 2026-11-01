//! Decoding the fork's tool call back into a verdict (docs/triage.md).

use super::*;

/// The verdict on one result of a triage: which result it addresses (by its label —
/// [`labels`]), the structured
/// `keep`, the optional `note`, and the model's exact pre-parse `keep` string (for
/// that result's [`Triage`] audit entry).
///
/// [`Triage`]: crate::record::log::Triage
pub(super) struct ResultVerdict {
    pub(super) label: String,
    pub(super) keep: Keep,
    pub(super) note: Option<String>,
    /// Triage's request for a better `intent`, when the one it was given left it
    /// nothing to decide against (docs/triage.md).
    pub(super) intent_feedback: Option<String>,
    pub(super) raw_keep: String,
}

/// A parsed `retain` call: one verdict per result the model addressed, plus the
/// triage-scoped inbox / background / surfacing dispositions (unfiltered — the caller
/// drops ids it didn't show).
pub(super) struct ParsedRetain {
    /// Verdicts keyed by label. May be short, over-long, or carry labels that were
    /// never offered — the caller reconciles against what it actually showed.
    pub(super) results: Vec<ResultVerdict>,
    pub(super) inbox: Vec<(Seq, Disposition)>,
    pub(super) background: Vec<(String, BgVerdict)>,
    /// Names the model surfaced — **unfiltered**; the caller drops any not on the roster.
    pub(super) surfaced: Vec<String>,
}

/// One entry of the `results` array the schema asks for.
#[derive(Deserialize)]
pub(super) struct ResultInput {
    pub(super) result: String,
    pub(super) keep: String,
    #[serde(default)]
    pub(super) note: Option<String>,
    #[serde(default)]
    pub(super) intent_feedback: Option<String>,
}

/// The `retain` tool's input.
///
/// Both answer shapes stand in one static schema (docs/triage.md). `keep` /
/// `note` at the top level are the **flat form**, which the tail instruction asks for on
/// a single-result triage — the overwhelming majority — since naming the one call in
/// question is transcription with no decision in it. `results` is the keyed form, asked
/// for when a triage carries several. Either may arrive for either triage size (nothing
/// enforces the schema — the tool isn't `strict`), so both are always accepted; a parse
/// failure would mean the result's output withheld as a hard error
/// (docs/triage.md).
#[derive(Deserialize)]
pub(super) struct RetainInput {
    #[serde(default)]
    pub(super) results: Vec<ResultInput>,
    #[serde(default)]
    pub(super) keep: Option<String>,
    #[serde(default)]
    pub(super) note: Option<String>,
    #[serde(default)]
    pub(super) intent_feedback: Option<String>,
    #[serde(default)]
    pub(super) inbox: Option<InboxVerdict>,
    #[serde(default)]
    pub(super) background: Option<BackgroundVerdict>,
    #[serde(default)]
    pub(super) surfaced: Vec<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct InboxVerdict {
    #[serde(default, deserialize_with = "lenient_ids")]
    pub(super) pin: Vec<u64>,
    #[serde(default, deserialize_with = "lenient_ids")]
    pub(super) dismiss: Vec<u64>,
}

/// Notification ids, accepting `102` and `"102"` alike. The triage reads ids out
/// of rendered prose (`[seq 102]`), so models stringify them routinely — a fork answer
/// whose only defect is `"pin": ["102"]` otherwise costs a
/// full retry. A string that isn't a number still
/// fails the whole parse: tolerance is for transcription, not invention.
fn lenient_ids<'de, D>(deserializer: D) -> Result<Vec<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(u64),
        Text(String),
    }
    let raw = Vec::<Id>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|id| match id {
            Id::Number(n) => Ok(n),
            Id::Text(s) => s.trim().parse().map_err(serde::de::Error::custom),
        })
        .collect()
}

#[derive(Deserialize, Default)]
pub(super) struct BackgroundVerdict {
    #[serde(default)]
    pub(super) pin: Vec<String>,
    #[serde(default)]
    pub(super) mute: Vec<String>,
}

/// The inbox/background fields of the standalone `triage` tool (no `keep`).
#[derive(Deserialize)]
pub(super) struct TriageInput {
    #[serde(default)]
    pub(super) inbox: Option<InboxVerdict>,
    #[serde(default)]
    pub(super) background: Option<BackgroundVerdict>,
}

/// Flatten the optional inbox/background verdicts into disposition lists (unfiltered —
/// the caller drops handles/ids it didn't show). Shared by `retain` and `triage`.
pub(super) fn collect_dispositions(
    inbox: Option<InboxVerdict>,
    background: Option<BackgroundVerdict>,
) -> Dispositions {
    let mut ibx = Vec::new();
    if let Some(ib) = inbox {
        ibx.extend(ib.pin.into_iter().map(|s| (s, Disposition::Pin)));
        ibx.extend(ib.dismiss.into_iter().map(|s| (s, Disposition::Dismiss)));
    }
    let mut bg = Vec::new();
    if let Some(b) = background {
        bg.extend(b.pin.into_iter().map(|h| (h, BgVerdict::Pin)));
        bg.extend(b.mute.into_iter().map(|h| (h, BgVerdict::Mute)));
    }
    (ibx, bg)
}

/// Pull the `retain` verdict out of the triage's completion. `None` when the turn
/// didn't call `retain` (or called something else).
///
/// `offered` is the labels the triage actually showed, in order. It is used only to
/// resolve verdicts that carry no usable label, both of which are unambiguous exactly when
/// one result is in flight and are ignored otherwise:
///
/// - a bare top-level `keep`/`note` — the **flat form**, which is what the tail
///   instruction asks for on a single-result triage;
/// - a lone `results` entry whose `result` doesn't match, which is re-keyed to the one
///   offered label. With a single target there is nothing to mis-attribute *to*, so
///   rejecting a decided verdict over a mistyped label would withhold output the model
///   plainly triaged (docs/triage.md).
///
/// Verdicts naming a label that was never offered are otherwise left in place — the
/// caller reconciles, the same harness-side guard used for invented notification ids
/// and roster names (docs/triage.md).
pub(super) fn parse_retain(completion: &Completion, offered: &[String]) -> Option<ParsedRetain> {
    let Payload::Assistant { output, .. } = &completion.assistant else {
        return None;
    };
    let calls = output.tool_calls().ok()?;
    // The shared envelope allows parallel calls (docs/README.md), so a triage may
    // split its verdict across several `retain` calls — one per result, say. Each
    // is read and their answers merged; any that does not parse makes the whole
    // verdict unusable, as a single malformed call does.
    let inputs = calls
        .iter()
        .filter(|c| c.name == RETAIN)
        .map(|c| RetainInput::deserialize(&c.input).ok())
        .collect::<Option<Vec<_>>>()?;
    if inputs.is_empty() {
        return None;
    }

    let mut verdicts: Vec<ResultVerdict> = Vec::new();
    let mut flat = None;
    let (mut dispositions, mut bg) = (Vec::new(), Vec::new());
    let mut surfaced: Vec<String> = Vec::new();
    for input in inputs {
        let RetainInput {
            results,
            keep,
            note,
            intent_feedback,
            inbox,
            background,
            surfaced: named,
        } = input;
        verdicts.extend(results.into_iter().map(|r| ResultVerdict {
            label: r.result,
            keep: parse_keep(&r.keep),
            note: r.note,
            intent_feedback: r.intent_feedback,
            raw_keep: r.keep,
        }));
        if flat.is_none() {
            flat = keep.map(|keep| (keep, note, intent_feedback));
        }
        let (ibx, b) = collect_dispositions(inbox, background);
        dispositions.extend(ibx);
        bg.extend(b);
        surfaced.extend(named);
    }
    // Both id-free shapes resolve only against a single-result triage, and only when the
    // array didn't already answer it.
    if let [only] = offered
        && !verdicts.iter().any(|v| v.label == *only)
    {
        match (flat, verdicts.as_mut_slice()) {
            // Flat form — the shape the tail instruction asks for on a single triage.
            (Some((keep, note, intent_feedback)), _) => verdicts.push(ResultVerdict {
                label: only.clone(),
                keep: parse_keep(&keep),
                note,
                intent_feedback,
                raw_keep: keep,
            }),
            // A lone keyed entry with the wrong label: unambiguous, so re-key it rather
            // than withhold a result the model plainly decided.
            (None, [lone]) => lone.label = only.clone(),
            (None, _) => {}
        }
    }

    Some(ParsedRetain {
        results: verdicts,
        inbox: dispositions,
        background: bg,
        surfaced,
    })
}

/// Pull the inbox/background dispositions from a `triage`-tool completion (the
/// standalone idle triage). `None` when the turn didn't call the named tool.
pub(super) fn parse_dispositions(completion: &Completion, tool: &str) -> Option<Dispositions> {
    let Payload::Assistant { output, .. } = &completion.assistant else {
        return None;
    };
    let calls = output.tool_calls().ok()?;
    let call = calls.iter().find(|c| c.name == tool)?;
    let TriageInput { inbox, background } = TriageInput::deserialize(&call.input).ok()?;
    Some(collect_dispositions(inbox, background))
}

/// Parse a `keep` string: `all` / `none` / range list. Lenient — malformed tokens are
/// dropped; a list with no valid ranges reads as `none`.
pub(super) fn parse_keep(s: &str) -> Keep {
    match s.trim().to_ascii_lowercase().as_str() {
        "all" => Keep::All,
        "none" | "" => Keep::None,
        _ => {
            let ranges: Vec<(usize, usize)> = s.split(',').filter_map(parse_range).collect();
            if ranges.is_empty() {
                Keep::None
            } else {
                Keep::Ranges(ranges)
            }
        }
    }
}

pub(super) fn parse_range(tok: &str) -> Option<(usize, usize)> {
    let tok = tok.trim();
    if tok.is_empty() {
        return None;
    }
    match tok.split_once('-') {
        Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
        None => {
            let v = tok.parse().ok()?;
            Some((v, v))
        }
    }
}
