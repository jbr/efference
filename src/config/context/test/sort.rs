//! Tests for `sort.rs` — the SQL-ish `sort` override on a `[context]` group.

use super::*;
use crate::config::context::sort::{SortKey, parse};

fn sorted(heading: &str, paths: &[&str], spec: &str) -> ContextEntry {
    let mut entry = labeled(heading, paths);
    entry.sort = Some(spec.to_string());
    entry
}

fn names(resolved: &Resolved) -> Vec<String> {
    resolved.sections[0]
        .items
        .iter()
        .map(|item| match item {
            Item::Inline { name, .. } | Item::Exec { name, .. } | Item::Listed { name, .. } => {
                name.clone()
            }
        })
        .collect()
}

/// A file per line: `(basename, frontmatter body)`.
fn write_group(dir: &Path, files: &[(&str, &str)]) {
    let group = dir.join("group");
    std::fs::create_dir_all(&group).unwrap();
    for (name, text) in files {
        std::fs::write(group.join(name), text).unwrap();
    }
}

#[test]
fn parse_reads_fields_and_directions() {
    assert_eq!(
        parse("modified desc"),
        Ok(vec![SortKey {
            field: "modified".into(),
            descending: true
        }])
    );
    assert_eq!(
        parse("priority desc, name"),
        Ok(vec![
            SortKey {
                field: "priority".into(),
                descending: true
            },
            SortKey {
                field: "name".into(),
                descending: false
            },
        ])
    );
    assert!(parse("name sideways").is_err());
    assert!(parse("name asc, ").is_err());
    assert!(parse("name asc extra").is_err());
}

/// A column whose every present value parses as a number compares numerically — `10` after `9`,
/// which a lexical compare would have reversed.
#[test]
fn numeric_column_compares_numerically() {
    let dir = tmpdir("sort-numeric");
    write_group(
        &dir,
        &[
            (
                "a.md",
                "---\nname: a\ndescription: d\npriority: 9\n---\nb\n",
            ),
            (
                "b.md",
                "---\nname: b\ndescription: d\npriority: 10\n---\nb\n",
            ),
            (
                "c.md",
                "---\nname: c\ndescription: d\npriority: 2\n---\nb\n",
            ),
        ],
    );
    let resolved = resolve(&[sorted("G", &["group"], "priority asc")], &dir).unwrap();
    assert_eq!(names(&resolved), ["c", "a", "b"]);
    let resolved = resolve(&[sorted("G", &["group"], "priority desc")], &dir).unwrap();
    assert_eq!(names(&resolved), ["b", "a", "c"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// One non-numeric value in the column makes the whole column lexical — typing is decided per
/// column over the whole group, not per row.
#[test]
fn one_non_numeric_value_makes_the_column_lexical() {
    let dir = tmpdir("sort-lexical");
    write_group(
        &dir,
        &[
            ("a.md", "---\nname: a\ndescription: d\nrank: 9\n---\nb\n"),
            ("b.md", "---\nname: b\ndescription: d\nrank: 10\n---\nb\n"),
            ("c.md", "---\nname: c\ndescription: d\nrank: high\n---\nb\n"),
        ],
    );
    let resolved = resolve(&[sorted("G", &["group"], "rank asc")], &dir).unwrap();
    assert_eq!(names(&resolved), ["b", "a", "c"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A row missing the column sorts last in **both** directions (NULLS LAST), and rows that tie on
/// every key fall back to the implicit `name asc`.
#[test]
fn missing_sorts_last_and_name_breaks_ties() {
    let dir = tmpdir("sort-missing");
    write_group(
        &dir,
        &[
            ("a.md", "---\nname: a\ndescription: d\n---\nb\n"),
            ("b.md", "---\nname: b\ndescription: d\nrank: 1\n---\nb\n"),
            ("c.md", "---\nname: c\ndescription: d\nrank: 1\n---\nb\n"),
        ],
    );
    let resolved = resolve(&[sorted("G", &["group"], "rank asc")], &dir).unwrap();
    assert_eq!(names(&resolved), ["b", "c", "a"]);
    let resolved = resolve(&[sorted("G", &["group"], "rank desc")], &dir).unwrap();
    assert_eq!(names(&resolved), ["b", "c", "a"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The built-in `modified` column orders by mtime; `name` is the file's own name (its frontmatter
/// `name`, else its basename), not its position in the walk.
#[test]
fn built_in_columns_are_modified_and_name() {
    let dir = tmpdir("sort-builtin");
    write_group(
        &dir,
        &[
            ("a.md", "---\nname: zulu\ndescription: d\n---\nb\n"),
            ("b.md", "---\nname: alpha\ndescription: d\n---\nb\n"),
        ],
    );
    let resolved = resolve(&[sorted("G", &["group"], "name asc")], &dir).unwrap();
    assert_eq!(names(&resolved), ["alpha", "zulu"]);

    let group = dir.join("group");
    let older = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(group.join("b.md"))
        .unwrap()
        .set_modified(older)
        .unwrap();
    let resolved = resolve(&[sorted("G", &["group"], "modified desc")], &dir).unwrap();
    assert_eq!(names(&resolved), ["zulu", "alpha"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A malformed spec fails the load closed rather than silently reverting to the default order.
#[test]
fn a_malformed_spec_fails_closed() {
    let dir = tmpdir("sort-bad-spec");
    write_group(&dir, &[("a.md", "---\nname: a\ndescription: d\n---\nb\n")]);
    let err = resolve(&[sorted("G", &["group"], "name upward")], &dir).unwrap_err();
    assert!(matches!(err, ResolveError::Sort(_)), "{err:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
