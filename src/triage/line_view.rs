//! How a triage view labels the lines of a result, and how `keep` names them back
//! (docs/triage.md, "Line numbering").
//!
//! Experiment scaffolding behind `EFFERENCE_TRIAGE_LINES`, in the manner of
//! [`AskVariant`](super::prompt): the production view is [`LineView::Numbers`] and an unset
//! variable changes nothing. The arms differ only in what the model is shown and what it
//! writes in `keep`; everything past validation — retention, `render_hunks`, the agent's
//! wire rendering — keeps taking 1-based shown-line indices, so [`LineView::Tags`] maps
//! tags back to indices here and nowhere else.

use super::*;

/// What the triage view puts in front of each line of a result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum LineView {
    /// `cat -n`-style numbers (1-based, 6-wide, tab-separated), the shipped view: the
    /// coordinate system `keep` addresses, shown only in the throwaway fork and never on
    /// the canonical wire, which uses the leaner `@@` hunks of
    /// [`render_triaged`](crate::project::render_triaged).
    #[default]
    Numbers,
    /// The shipped numbers, plus a header sentence saying numbers inside the output are
    /// part of the text.
    Header,
    /// `12 │ text`, and the header sentence naming the margin.
    Margin,
    /// `a3f │ text`: a tag per line, `keep` names tag ranges, and [`Self::parse_keep`]
    /// maps them back to indices.
    Tags,
}

const HEADER_NUMBERS: &str = "Lines are numbered from 1 below this header; numbers inside the \
output are part of the text.";
const HEADER_MARGIN: &str = "Each line starts with its line number and a │ margin (`12 │ text`); \
numbers after the margin are part of the text, and `keep` uses the margin's numbers.";
const HEADER_TAGS: &str = "Each line starts with a tag and a │ margin (`a3f │ text`); \
numbers after the margin are part of the text, and `keep` names ranges by the margin's tags, \
like `a3f-c61`.";

/// Tag space: a letter then two of `[a-z0-9]`. Line `i` gets `(i * STEP + OFFSET) mod
/// TAG_SPACE`; `STEP` is coprime to the space, so tags are unique, and consecutive lines are
/// far apart in it, so a tag carries no count to continue from. A leading letter keeps a
/// tag from reading as a number.
const TAG_SPACE: usize = 26 * 36 * 36;
const STEP: usize = 7919;
const OFFSET: usize = 4271;
const DIGITS: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

impl LineView {
    /// Read `EFFERENCE_TRIAGE_LINES`: `numbers`, `header`, `margin`, or `tags`. An
    /// unrecognized value panics, for the reason [`AskVariant::from_env`] does.
    pub(super) fn from_env() -> Self {
        match std::env::var("EFFERENCE_TRIAGE_LINES").as_deref() {
            Err(_) | Ok("numbers") => Self::Numbers,
            Ok("header") => Self::Header,
            Ok("margin") => Self::Margin,
            Ok("tags") => Self::Tags,
            Ok(other) => panic!("EFFERENCE_TRIAGE_LINES: unknown arm {other:?}"),
        }
    }

    /// The sentence the result header carries, if the arm has one.
    pub(super) fn header_sentence(self) -> Option<&'static str> {
        match self {
            Self::Numbers => None,
            Self::Header => Some(HEADER_NUMBERS),
            Self::Margin => Some(HEADER_MARGIN),
            Self::Tags => Some(HEADER_TAGS),
        }
    }

    /// The lines as the view shows them, one per row.
    pub(super) fn render(self, lines: &[&str]) -> String {
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| match self {
                Self::Numbers | Self::Header => format!("{:>6}\t{}", i + 1, line),
                Self::Margin => format!("{:>4} │ {}", i + 1, line),
                Self::Tags => format!("{} │ {}", tag(i + 1), line),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The text that stands in for a truncation marker's indent.
    pub(super) fn truncation_indent(self) -> &'static str {
        match self {
            Self::Numbers | Self::Header => "       ",
            Self::Margin | Self::Tags => "     ",
        }
    }

    /// Parse a `keep` string in the form this arm shows. Lenient like [`parse_keep`]: a
    /// token that names nothing is dropped here, and [`Self::check_tags`] is what rejects
    /// it.
    pub(super) fn parse_keep(self, s: &str) -> Keep {
        match self {
            Self::Tags => match s.trim().to_ascii_lowercase().as_str() {
                "all" => Keep::All,
                "none" | "" => Keep::None,
                _ => {
                    let ranges: Vec<(usize, usize)> =
                        s.split(',').filter_map(|t| tag_range(t).ok()).collect();
                    match ranges.is_empty() {
                        true => Keep::None,
                        false => Keep::Ranges(ranges),
                    }
                }
            },
            _ => parse_keep_numbers(s),
        }
    }

    /// The tag arm's validity rule for one result's raw `keep`, against its shown line
    /// count `n`: every token must name a tag of the shown output. Checked on the raw
    /// string because [`Self::parse_keep`] drops what it cannot read, and a dropped token
    /// is silent loss. Other arms have nothing to add to [`require_in_bounds`].
    pub(super) fn check_tags(self, label: &str, raw_keep: &str, n: usize) -> Result<(), String> {
        if self != Self::Tags {
            return Ok(());
        }
        if matches!(
            raw_keep.trim().to_ascii_lowercase().as_str(),
            "all" | "none" | ""
        ) {
            return Ok(());
        }
        for tok in raw_keep.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            let (s, e) = tag_range(tok).map_err(|bad| {
                format!(
                    "keep range `{tok}` for [{label}] names no line tag (`{bad}`) — keep \
                     ranges name the tags in the shown margin, like `a3f-c61`, not numbers \
                     inside the output"
                )
            })?;
            if s.min(e) > n {
                return Err(format!(
                    "keep range `{tok}` for [{label}] is beyond the shown output, which has \
                     {n} lines — name tags from the margin, not numbers inside the output"
                ));
            }
        }
        Ok(())
    }

    /// Rewrite the numbered-range wording of the ask and the tool schema for this arm.
    /// Only the tag arm changes what `keep` is made of.
    pub(super) fn rewrite(self, text: &str) -> String {
        if self != Self::Tags {
            return text.to_string();
        }
        text.replace(
            "comma-separated 1-based inclusive line ranges like\n              \"40-52,310-318\"",
            "comma-separated inclusive ranges of line tags like\n              \"a3f-c61,k2m-p9x\"",
        )
        .replace(
            "comma-separated 1-based inclusive line ranges, e.g. \"40-52,310-318\" (a single line is \"7\")",
            "comma-separated inclusive ranges of line tags, e.g. \"a3f-c61,k2m-p9x\" (a single line is its tag alone)",
        )
        .replace("line-numbered output", "line-tagged output")
        .replace("The output is line-numbered.", "The output's lines are tagged.")
        .replace(
            "Line numbers restart at 1 within each result, so ranges \
are always relative to the result you are deciding.",
            "Tags are per result, so ranges are always relative to the result you are deciding.",
        )
        .replace(
            "Line numbers restart at 1 within each result.",
            "Tags are per result.",
        )
        .replace(
            "Numbered within this result only.",
            "Tags are within this result only.",
        )
    }
}

/// The tag of the 1-based line `n`.
pub(super) fn tag(n: usize) -> String {
    let v = (n.saturating_sub(1) * STEP + OFFSET) % TAG_SPACE;
    [v / 1296, (v / 36) % 36, v % 36]
        .iter()
        .map(|&d| DIGITS[d] as char)
        .collect()
}

/// The 1-based line a tag names, among the lines a triage view can show.
pub(super) fn untag(t: &str) -> Option<usize> {
    let t = t.trim().to_ascii_lowercase();
    (1..=TRIAGE_LINE_CAP).find(|&n| tag(n) == t)
}

/// One `a3f` or `a3f-c61` token as 1-based indices; the `Err` is the end that names no tag.
fn tag_range(tok: &str) -> Result<(usize, usize), String> {
    let tok = tok.trim();
    let (a, b) = tok.split_once('-').unwrap_or((tok, tok));
    let end = |t: &str| untag(t).ok_or_else(|| t.trim().to_string());
    Ok((end(a)?, end(b)?))
}
