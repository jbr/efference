//! The SQL-ish `sort` override on a `[context]` group (docs/context.md).
//!
//! A group's files are rows and their frontmatter keys are columns, plus two built-ins: `name`
//! (the file's tag name — its frontmatter `name`, else its basename) and `modified` (the file's
//! mtime). `sort = "priority desc, name asc"` orders those rows. Ordering is a pure semantic
//! choice and never a stability constraint: the preamble is read once at boot and the resolved
//! order is frozen into the `Payload::Preamble` snapshot (docs/context.md), so a later sort
//! change cannot move bytes behind a live cache breakpoint.
//!
//! Typing is decided **per column, over the whole group**: if every row that has a value for that
//! column parses as a number, the column compares numerically; otherwise it compares lexically
//! (byte order). A row missing the column sorts **last** regardless of direction (NULLS LAST) —
//! direction reorders the rows that have a value, it does not promote the ones that don't.

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// One parsed `<field> [asc|desc]` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub field: String,
    pub descending: bool,
}

/// A `sort` spec that isn't `<field> [asc|desc]`, comma-separated. The preamble builder fails
/// closed on one (docs/context.md): a misspelled sort silently falling back to `name asc` would
/// be the operator getting an order they did not write.
#[derive(Debug, PartialEq, Eq)]
pub struct SortSpecError(pub String);

impl std::fmt::Display for SortSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parse a comma-separated SQL-ish spec into its keys. Each clause is a field name with an
/// optional `asc`/`desc` (case-insensitive; `asc` is the default). An empty clause, a missing
/// field, an unknown direction word, or extra tokens are all errors.
pub fn parse(spec: &str) -> Result<Vec<SortKey>, SortSpecError> {
    let mut keys = Vec::new();
    for clause in spec.split(',') {
        let mut tokens = clause.split_whitespace();
        let Some(field) = tokens.next() else {
            return Err(SortSpecError(format!(
                "`{spec}` has an empty sort clause; each clause is `<field> [asc|desc]`"
            )));
        };
        let descending = match tokens.next() {
            None | Some("asc") | Some("ASC") | Some("Asc") => false,
            Some("desc") | Some("DESC") | Some("Desc") => true,
            Some(other) => {
                return Err(SortSpecError(format!(
                    "sort direction `{other}` on field `{field}` is not `asc` or `desc`"
                )));
            }
        };
        if let Some(extra) = tokens.next() {
            return Err(SortSpecError(format!(
                "trailing `{extra}` after `{field}` — a sort clause is `<field> [asc|desc]`"
            )));
        }
        keys.push(SortKey {
            field: field.to_string(),
            descending,
        });
    }
    Ok(keys)
}

/// One file as a sortable row: the columns it can be sorted on. `name` and `modified` are the
/// built-ins; `fields` are the file's top-level frontmatter keys verbatim. `display` is the path
/// as the operator wrote it — the final, invisible tiebreak that keeps the order total (two files
/// with the same name and no other distinguishing column still have a fixed order).
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub name: String,
    pub display: String,
    /// Seconds since the epoch, or `None` when the mtime was unavailable — a missing column like
    /// any other, so an unreadable mtime sorts last rather than failing the boot.
    pub modified: Option<f64>,
    pub fields: BTreeMap<String, String>,
}

impl Row {
    /// This row's value for one column: a frontmatter key wins by its own name; otherwise the two
    /// built-ins answer. `None` is a missing value (NULLS LAST).
    fn column(&self, field: &str) -> Option<String> {
        if let Some(v) = self.fields.get(field) {
            return Some(v.clone());
        }
        match field {
            "name" => Some(self.name.clone()),
            "modified" => self.modified.map(|m| m.to_string()),
            _ => None,
        }
    }
}

/// Order `items` by the parsed `keys`, with an implicit final `name asc` (then display path)
/// tiebreak. `rows[i]` describes `items[i]`; both are permuted together.
///
/// Column typing is computed once, before the comparison: a column is numeric iff every row that
/// has a value for it parses as `f64`.
pub fn apply<T>(items: &mut Vec<T>, rows: &mut Vec<Row>, keys: &[SortKey]) {
    let numeric: Vec<bool> = keys
        .iter()
        .map(|k| {
            let mut any = false;
            for row in rows.iter() {
                let Some(v) = row.column(&k.field) else {
                    continue;
                };
                any = true;
                if v.trim().parse::<f64>().is_err() {
                    return false;
                }
            }
            any
        })
        .collect();

    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| compare(&rows[a], &rows[b], keys, &numeric));

    let mut permuted_items = Vec::with_capacity(items.len());
    let mut permuted_rows = Vec::with_capacity(rows.len());
    let mut taken_items: Vec<Option<T>> = items.drain(..).map(Some).collect();
    let mut taken_rows: Vec<Option<Row>> = rows.drain(..).map(Some).collect();
    for i in order {
        permuted_items.push(taken_items[i].take().expect("each index appears once"));
        permuted_rows.push(taken_rows[i].take().expect("each index appears once"));
    }
    *items = permuted_items;
    *rows = permuted_rows;
}

/// Compare two rows across the keys in order, then the implicit `name asc` / display-path tiebreak.
fn compare(a: &Row, b: &Row, keys: &[SortKey], numeric: &[bool]) -> Ordering {
    for (key, &is_numeric) in keys.iter().zip(numeric) {
        let ordering = match (a.column(&key.field), b.column(&key.field)) {
            (None, None) => Ordering::Equal,
            // NULLS LAST, direction-independent: a missing value is not a small value.
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            (Some(x), Some(y)) => {
                let cmp = if is_numeric {
                    x.trim()
                        .parse::<f64>()
                        .unwrap_or(f64::NAN)
                        .partial_cmp(&y.trim().parse::<f64>().unwrap_or(f64::NAN))
                        .unwrap_or(Ordering::Equal)
                } else {
                    x.cmp(&y)
                };
                if key.descending { cmp.reverse() } else { cmp }
            }
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a.name.cmp(&b.name).then_with(|| a.display.cmp(&b.display))
}
