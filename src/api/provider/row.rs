//! The normalized model row every provider's listing is reduced to
//! (docs/model-catalog.md).
//!
//! A listing's shape is the provider's; the row is the host's. Each provider
//! module reads its own source into [`ListedModel`]s, and [`arrange`] gives the
//! set one order. Facts are filled only where the source declares them: an
//! absent field means the source said nothing, not that the answer is no.

use super::*;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

impl Price {
    /// A price from per-token decimal strings, as OpenRouter spells them. A
    /// negative value is the source's "unknown" (`-1` on routers whose price
    /// depends on the upstream), so it yields no price at all.
    pub(super) fn from_per_token(input: &str, output: &str) -> Option<Self> {
        let per_million = |text: &str| -> Option<f64> {
            let value: f64 = text.trim().parse().ok()?;
            // Rounded at four decimals: `0.000003 * 1e6` is not exactly 3 in
            // floating point, and a stored `3.0000000000000004` would be shown.
            (value >= 0.0).then(|| (value * 1e6 * 1e4).round() / 1e4)
        };
        Some(Self {
            input: per_million(input)?,
            output: per_million(output)?,
        })
    }
}

/// What a listed model does (docs/model-catalog.md): produce a turn, or answer
/// typed questions about a state without producing one (a decision model, which
/// only the spotter can use — docs/permissions.md).
///
/// A row inherits its kind from where it was listed: a connection whose provider
/// serves no turns lists only decision models, and OpenRouter declares it per
/// entry in `architecture.output_modalities`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    #[default]
    Text,
    Decisions,
}

impl ModelKind {
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text)
    }
}

/// One model a connection lists. `connection` is not part of the stored row: the
/// catalog keys rows by it (`catalog.rs`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ListedModel {
    /// The provider-native id a session draft carries.
    pub model: String,
    /// OpenRouter's author, alias marker removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Provider-supplied prose, when nonempty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    /// Context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Image input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
    /// Absent on the wire for a text model, the common case.
    #[serde(default, skip_serializing_if = "ModelKind::is_text")]
    pub kind: ModelKind,
    /// The model line the provider files it under (Anthropic `line`: `haiku`,
    /// `sonnet`, …), read rather than inferred from the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// The provider's lifecycle stage (Anthropic `lifecycle`: `active`,
    /// `deprecated`, `retired`), verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
}

impl ListedModel {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    /// A display name, dropped when empty or when it only repeats the id.
    pub fn named(mut self, name: &str) -> Self {
        let name = name.trim();
        self.name = (!name.is_empty() && name != self.model).then(|| name.to_string());
        self
    }

    /// Provider prose is retained verbatim; whitespace-only text is absent.
    pub fn described(mut self, description: Option<&str>) -> Self {
        self.description = description
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string);
        self
    }
}

/// Order a normalized set: dated rows newest first, then undated rows, each
/// keeping the source's order among equals. A source that dates nothing keeps
/// its own order whole. Rows of different groups interleave, which keeps each
/// group's own order the same as sorting the group alone.
pub(super) fn arrange(mut rows: Vec<ListedModel>) -> Vec<ListedModel> {
    rows.sort_by_key(|row| std::cmp::Reverse(row.created));
    rows
}

/// Unix seconds for an RFC 3339 timestamp (`2026-02-04T00:00:00Z`, or with a
/// numeric offset). Fractions of a second are dropped. `None` for any other
/// spelling.
pub(super) fn unix_from_rfc3339(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(['T', 't', ' '])?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
        Some(at) => {
            let (clock, zone) = time.split_at(at);
            let seconds = match zone {
                "Z" | "z" => 0,
                zone => {
                    let sign = if zone.starts_with('-') { -1 } else { 1 };
                    let (hours, minutes) = zone[1..].split_once(':')?;
                    sign * (hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60)
                }
            };
            (clock, seconds)
        }
        None => (time, 0),
    };
    let clock = clock.split('.').next()?;
    let mut parts = clock.splitn(3, ':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next().unwrap_or("0").parse().ok()?;
    // Days from the civil calendar (Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// The UTC calendar date (`YYYY-MM-DD`) of a unix-seconds instant.
pub(super) fn utc_date(unix: i64) -> String {
    // Civil from days (Hinnant's algorithm), the inverse of the one above.
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod test;
