//! Decoding the fork's tool call back into a verdict (docs/redaction-fork.md).

use super::*;

/// The verdict on one result of a gate: which call it addresses, the structured
/// `keep`, the optional `note`, and the model's exact pre-parse `keep` string (for
/// that result's [`Triage`] audit entry).
///
/// [`Triage`]: crate::log::Triage
pub(super) struct ResultVerdict {
    pub(super) call_id: String,
    pub(super) keep: Keep,
    pub(super) note: Option<String>,
    pub(super) raw_keep: String,
}

/// A parsed `retain` call: one verdict per result the model addressed, plus the
/// gate-scoped inbox / background / surfacing dispositions (unfiltered — the caller
/// drops ids it didn't show).
pub(super) struct ParsedRetain {
    /// Verdicts keyed by `call_id`. May be short, over-long, or carry ids that were
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
    pub(super) call_id: String,
    pub(super) keep: String,
    #[serde(default)]
    pub(super) note: Option<String>,
}

/// The `retain` tool's input.
///
/// Both answer shapes stand in one static schema (docs/redaction-fork.md). `keep` /
/// `note` at the top level are the **flat form**, which the tail instruction asks for on
/// a single-result gate — the overwhelming majority — since naming the one call in
/// question is transcription with no decision in it. `results` is the keyed form, asked
/// for when a gate carries several. Either may arrive for either gate size (nothing
/// enforces the schema — the tool isn't `strict`), so both are always accepted; a parse
/// failure would mean the result's output withheld as a hard error
/// (docs/redaction-fork.md).
#[derive(Deserialize)]
pub(super) struct RetainInput {
    #[serde(default)]
    pub(super) results: Vec<ResultInput>,
    #[serde(default)]
    pub(super) keep: Option<String>,
    #[serde(default)]
    pub(super) note: Option<String>,
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

/// Notification ids, accepting `102` and `"102"` alike. The peek reads ids out
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

/// Pull the `retain` verdict out of the peek's completion. `None` when the turn
/// didn't call `retain` (or called something else).
///
/// `offered` is the call ids the peek actually showed, in order. It is used only to
/// resolve verdicts that carry no usable id, both of which are unambiguous exactly when
/// one result is in flight and are ignored otherwise:
///
/// - a bare top-level `keep`/`note` — the **flat form**, which is what the tail
///   instruction asks for on a single-result gate;
/// - a lone `results` entry whose `call_id` doesn't match, which is re-keyed to the one
///   offered id. With a single target there is nothing to mis-attribute *to*, so
///   rejecting a decided verdict over a mistyped id would withhold output the model
///   plainly triaged (docs/redaction-fork.md).
///
/// Verdicts naming an id that was never offered are otherwise left in place — the
/// caller reconciles, the same harness-side guard used for invented notification ids
/// and roster names (docs/redaction-fork.md).
pub(super) fn parse_retain(completion: &Completion, offered: &[String]) -> Option<ParsedRetain> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let retain = calls.iter().find(|c| c.name == RETAIN)?;
    let RetainInput {
        results,
        keep,
        note,
        inbox,
        background,
        surfaced,
    } = RetainInput::deserialize(&retain.input).ok()?;

    let mut verdicts: Vec<ResultVerdict> = results
        .into_iter()
        .map(|r| ResultVerdict {
            call_id: r.call_id,
            keep: parse_keep(&r.keep),
            note: r.note,
            raw_keep: r.keep,
        })
        .collect();
    // Both id-free shapes resolve only against a single-result gate, and only when the
    // array didn't already answer it.
    if let [only] = offered
        && !verdicts.iter().any(|v| v.call_id == *only)
    {
        match (keep, verdicts.as_mut_slice()) {
            // Flat form — the shape the tail instruction asks for on a single gate.
            (Some(keep), _) => verdicts.push(ResultVerdict {
                call_id: only.clone(),
                keep: parse_keep(&keep),
                note,
                raw_keep: keep,
            }),
            // A lone keyed entry with the wrong id: unambiguous, so re-key it rather
            // than withhold a result the model plainly decided.
            (None, [lone]) => lone.call_id = only.clone(),
            (None, _) => {}
        }
    }

    let (dispositions, bg) = collect_dispositions(inbox, background);
    Some(ParsedRetain {
        results: verdicts,
        inbox: dispositions,
        background: bg,
        surfaced,
    })
}

/// Pull the inbox/background dispositions from a `triage`-tool completion (the
/// standalone idle peek). `None` when the turn didn't call the named tool.
pub(super) fn parse_dispositions(completion: &Completion, tool: &str) -> Option<Dispositions> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
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
