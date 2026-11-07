//! Agent-facing model discovery, without editorial task ratings (docs/model-catalog.md).

use super::{CatalogModel, ConnectionCatalog, ModelRole};

pub use crate::config::guide::MODELS as URI;
const HELP: &str = "Read efference://models?connection=<name>&q=<search>&role=main&limit=20&offset=0. Filters combine; URL-encode values. connection is exact; q is a case-insensitive substring of model ID, name, or description. role defaults to main; main and triage both exclude decision-only models, spotter includes them. This is compatibility, not a task recommendation. limit is 1–100; offset pages through the ordered matches.";

/// A validated concrete resource query. Unknown and repeated parameters are errors.
#[derive(Debug)]
pub struct Query {
    pub connection: Option<String>,
    pub role: ModelRole,
    q: Option<String>,
    limit: usize,
    offset: usize,
}

impl Query {
    /// Whether this URI addresses the catalogue, independently of its query.
    pub fn addresses(uri: &str) -> bool {
        uri.split_once('?').map_or(uri, |(base, _)| base) == URI && !uri.contains('#')
    }

    pub fn parse(uri: &str) -> Result<Self, String> {
        let (_, query) = uri.split_once('?').unwrap_or((uri, ""));
        if !Self::addresses(uri) {
            return Err(format!("no model resource at {uri:?}. {HELP}"));
        }
        let mut result = Self {
            connection: None,
            role: ModelRole::Main,
            q: None,
            limit: 20,
            offset: 0,
        };
        let mut seen = std::collections::BTreeSet::new();
        for pair in query.split('&').filter(|s| !s.is_empty()) {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("expected name=value. {HELP}"))?;
            let key = decode(key)?;
            let value = decode(value)?;
            if !seen.insert(key.clone()) {
                return Err(format!("repeated parameter {key:?}. {HELP}"));
            }
            match key.as_str() {
                "connection" if !value.is_empty() => result.connection = Some(value),
                "q" => result.q = Some(value.to_lowercase()),
                "role" => {
                    result.role = ModelRole::from_name(&value)
                        .ok_or_else(|| format!("unknown role {value:?}. {HELP}"))?;
                }
                "limit" => {
                    result.limit = value
                        .parse::<usize>()
                        .ok()
                        .filter(|n| (1..=100).contains(n))
                        .ok_or_else(|| format!("limit must be 1–100. {HELP}"))?;
                }
                "offset" => {
                    result.offset = value
                        .parse()
                        .map_err(|_| format!("offset must be a nonnegative integer. {HELP}"))?;
                }
                _ => return Err(format!("unknown or empty parameter {key:?}. {HELP}")),
            }
        }
        Ok(result)
    }

    /// Validate connection spelling against the unfiltered inventory, then apply compatibility.
    pub fn select(
        &self,
        connections: Vec<ConnectionCatalog>,
    ) -> Result<Vec<ConnectionCatalog>, String> {
        if let Some(name) = &self.connection
            && !connections.iter().any(|c| &c.connection == name)
        {
            return Err(format!(
                "no connection named {name:?}. Connections: {}. {HELP}",
                connections
                    .iter()
                    .map(|c| c.connection.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(connections
            .into_iter()
            .filter(|c| self.connection.as_ref().is_none_or(|n| n == &c.connection))
            .filter_map(|c| c.for_role(self.role))
            .collect())
    }

    pub fn render(&self, connections: &[ConnectionCatalog]) -> String {
        let mut out = String::from(
            "Model catalog — exact connection/model slugs; USD/1M input/output, unknown is not free.\n",
        );
        if self.connection.is_none() && self.q.is_none() && self.offset == 0 {
            out.push_str("Unlisted typed IDs may still work; capabilities are provider facts, not task ratings.\n");
            out.push_str(HELP);
            out.push('\n');
        }
        out.push_str(&format!("\nConnections (role={}):\n", self.role.name()));
        for c in connections {
            out.push_str(&format!(
                "- {}: {}; {} listed rows",
                c.connection,
                match c.status {
                    super::Status::Fresh => "fresh",
                    super::Status::Stale => "stale",
                    super::Status::Missing => "missing",
                    super::Status::Failed => "failed",
                    super::Status::Unavailable => "unavailable",
                },
                c.models.len()
            ));
            if let Some(age) = c.age {
                out.push_str(&format!("; age {}", age_label(age)));
            }
            if let Some(error) = &c.error {
                out.push_str(&format!("; {}", one_line(error, 240)));
            }
            out.push('\n');
        }
        out.push_str(
            "Missing/stale listings refresh in the background; read again for updated results.\n",
        );
        let mut models: Vec<&CatalogModel> = connections
            .iter()
            .flat_map(|c| &c.models)
            .filter(|m| {
                self.q.as_ref().is_none_or(|q| {
                    [
                        m.row.model.as_str(),
                        m.row.name.as_deref().unwrap_or(""),
                        m.row.description.as_deref().unwrap_or(""),
                    ]
                    .iter()
                    .any(|s| s.to_lowercase().contains(q))
                })
            })
            .collect();
        models.sort_by(|a, b| {
            (a.recent.unwrap_or(u32::MAX), &a.connection, &a.row.model).cmp(&(
                b.recent.unwrap_or(u32::MAX),
                &b.connection,
                &b.row.model,
            ))
        });
        let shown = models.len().saturating_sub(self.offset).min(self.limit);
        out.push_str(&format!(
            "\n{} matches; showing {shown} at offset {} (recent picks first, then slug).\n",
            models.len(),
            self.offset
        ));
        for m in models.iter().skip(self.offset).take(self.limit) {
            out.push_str(&format!("- {}/{}", m.connection, m.row.model));
            if let Some(name) = &m.row.name {
                out.push_str(&format!(" — {}", one_line(name, 120)));
            }
            if let Some(q) = &self.q
                && !m.row.model.to_lowercase().contains(q)
                && !m
                    .row
                    .name
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(q)
                && let Some(description) = &m.row.description
            {
                out.push_str(&format!(
                    "; description match: {}",
                    match_excerpt(description, q)
                ));
            }
            if let Some(rank) = m.recent {
                out.push_str(&format!("; recent #{rank}"));
            }
            if let Some(context) = m.row.context {
                out.push_str(&format!("; context={context}"));
            }
            match m.row.price {
                Some(p) => out.push_str(&format!("; USD/1M input={} output={}", p.input, p.output)),
                None => out.push_str("; price=unknown"),
            }
            if let Some(value) = m.row.reasoning {
                out.push_str(&format!("; reasoning={value}"));
            }
            if let Some(value) = m.row.images {
                out.push_str(&format!("; images={value}"));
            }
            if !m.row.kind.is_text() {
                out.push_str("; decision-only");
            }
            if let Some(lifecycle) = &m.row.lifecycle {
                out.push_str(&format!("; lifecycle={}", one_line(lifecycle, 80)));
            }
            out.push('\n');
        }
        let next = self.offset.saturating_add(shown);
        if next < models.len() {
            out.push_str(&format!(
                "More matches: keep these filters and set offset={next}, or narrow connection/q.\n"
            ));
        }
        out
    }
}

fn age_label(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d {}h", seconds / 86400, seconds % 86400 / 3600),
    }
}

fn match_excerpt(description: &str, query: &str) -> String {
    let text = description.split_whitespace().collect::<Vec<_>>().join(" ");
    // Search the normalized text again: whitespace folding may move the match.
    // Character offsets keep slicing safe when Unicode lowercasing expands a letter.
    let start = text
        .char_indices()
        .find_map(|(i, _)| text[i..].to_lowercase().starts_with(query).then_some(i))
        .unwrap_or(0);
    let prefix = if start > 0 { "…" } else { "" };
    let excerpt: String = text[start..].chars().take(120).collect();
    let suffix = if text[start..].chars().count() > 120 {
        "…"
    } else {
        ""
    };
    format!("{prefix}{excerpt}{suffix}")
}

fn one_line(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

fn decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return Err(format!("invalid percent encoding. {HELP}"));
        }
    }
    percent_encoding::percent_decode_str(&text.replace('+', " "))
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| format!("query must be UTF-8. {HELP}"))
}

#[cfg(test)]
mod test;
