//! Decoding the fork's tool call back into a verdict (docs/redaction-fork.md).

use super::*;

/// A parsed `retain` call: the structured `keep`, the optional `note`, the model's
/// exact pre-parse `keep` string (for the [`Triage`] audit entry), and the inbox
/// dispositions (unfiltered — the caller drops ids it didn't show).
///
/// [`Triage`]: crate::log::Triage
pub(super) struct ParsedRetain {
    pub(super) keep: Keep,
    pub(super) note: Option<String>,
    pub(super) raw_keep: String,
    pub(super) inbox: Vec<(Seq, Disposition)>,
    pub(super) background: Vec<(String, BgVerdict)>,
    /// Names the model surfaced — **unfiltered**; the caller drops any not on the roster.
    pub(super) surfaced: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct RetainInput {
    pub(super) keep: String,
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
    #[serde(default)]
    pub(super) pin: Vec<u64>,
    #[serde(default)]
    pub(super) dismiss: Vec<u64>,
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
/// didn't call `retain` (or called something else) — the caller then falls back to
/// keep-all (docs/redaction-fork.md, docs/README.md).
pub(super) fn parse_retain(completion: &Completion) -> Option<ParsedRetain> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let retain = calls.iter().find(|c| c.name == RETAIN)?;
    let RetainInput {
        keep,
        note,
        inbox,
        background,
        surfaced,
    } = RetainInput::deserialize(&retain.input).ok()?;
    let parsed_keep = parse_keep(&keep);
    let (dispositions, bg) = collect_dispositions(inbox, background);
    Some(ParsedRetain {
        keep: parsed_keep,
        note,
        raw_keep: keep,
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
