//! Reading the paths a source file writes to modules of the crate (docs/README.md,
//! *Dependency direction*): text in, one [`Reference`] per path out, each below the crate
//! root.

/// A path in a file that names a module of the crate.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Reference {
    /// The file, relative to `src/`.
    pub(super) file: String,
    pub(super) line: usize,
    /// The path below the crate root: `host::Host`, `record::log::Payload`.
    pub(super) path: String,
}

impl Reference {
    /// The module the file is, as a path from the crate root: `record/log/summary.rs` is
    /// `record::log::summary`, so a group (`record`) and a module in one (`record::log`)
    /// both contain it by prefix.
    pub(super) fn importer(&self) -> String {
        module_path(&self.file).join("::")
    }

    /// The path the reference names. A group, a module in one or an item below it all
    /// contain it by prefix.
    pub(super) fn imported(&self) -> &str {
        &self.path
    }
}

/// The module path of a file below `src/`: `agent/step.rs` is `["agent", "step"]`,
/// and `clients/tui/mod.rs` is `["clients", "tui"]`.
fn module_path(file: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = file.trim_end_matches(".rs").split('/').collect();
    if parts.last() == Some(&"mod") {
        parts.pop();
    }
    parts
}

type Token = (String, usize);

/// The paths below the crate root that `text` names, each with the line it starts on. A
/// `super::` chain that leaves the file's module is resolved from the file's module path, so
/// it names the same path a `crate::` spelling would.
pub(super) fn references(file: &str, text: &str) -> Vec<Reference> {
    let tokens = tokens(&clean(text));
    let module = module_path(file);
    let is = |index: usize, text: &str| tokens.get(index).is_some_and(|(token, _)| token == text);
    let after_path_separator = |index: usize| index > 0 && is(index - 1, "::");
    let mut named: Vec<(Vec<String>, usize)> = Vec::new();
    // For each open brace, whether it opens `mod name {`.
    let mut braces: Vec<bool> = Vec::new();
    for (index, (token, line)) in tokens.iter().enumerate() {
        let mut paths = Vec::new();
        match token.as_str() {
            "{" => {
                let opens_mod =
                    index >= 2 && is(index - 2, "mod") && is_ident(&tokens[index - 1].0);
                braces.push(opens_mod);
            }
            "}" => {
                braces.pop();
            }
            "crate" if is(index + 1, "::") && !after_path_separator(index) => {
                tree(&tokens, index + 2, Vec::new(), &mut paths);
            }
            "super" if !after_path_separator(index) => {
                let mut hops = 0;
                while is(index + 2 * hops, "super") && is(index + 2 * hops + 1, "::") {
                    hops += 1;
                }
                // Each inline module deepens the path `super` climbs from; a chain that
                // stays inside the file's module names nothing outside it.
                let inline = braces.iter().filter(|opens_mod| **opens_mod).count();
                if hops > inline && hops - inline <= module.len() {
                    let kept = module.len() - (hops - inline);
                    let prefix = module[..kept].iter().map(ToString::to_string).collect();
                    tree(&tokens, index + 2 * hops, prefix, &mut paths);
                }
            }
            _ => {}
        }
        named.extend(paths.into_iter().map(|path| (path, *line)));
    }
    let mut references: Vec<Reference> = Vec::new();
    for (path, line) in named {
        if path.is_empty() {
            continue;
        }
        let path = path.join("::");
        if !references
            .iter()
            .any(|seen| seen.path == path && seen.line == line)
        {
            references.push(Reference {
                file: file.to_string(),
                line,
                path,
            });
        }
    }
    references
}

/// Read the use-tree or path that starts at `tokens[index]` under `prefix`, pushing
/// each path it names; returns the index after it. `a::b` names `[a, b]`,
/// `a::{b, c::d, self}` names `[a, b]`, `[a, c, d]` and `[a]`, and `a::*` names `[a]`.
fn tree(tokens: &[Token], index: usize, prefix: Vec<String>, out: &mut Vec<Vec<String>>) -> usize {
    let Some((token, _)) = tokens.get(index) else {
        return index;
    };
    match token.as_str() {
        "{" => {
            let mut index = index + 1;
            while index < tokens.len() && tokens[index].0 != "}" {
                index = tree(tokens, index, prefix.clone(), out);
                // Past an `as alias`, to the separator.
                while index < tokens.len() && !matches!(tokens[index].0.as_str(), "," | "}") {
                    index += 1;
                }
                if index < tokens.len() && tokens[index].0 == "," {
                    index += 1;
                }
            }
            index + 1
        }
        "*" | "self" => {
            out.push(prefix);
            index + 1
        }
        token if is_ident(token) => {
            let mut path = prefix;
            path.push(token.to_string());
            if tokens.get(index + 1).is_some_and(|(next, _)| next == "::") {
                tree(tokens, index + 2, path, out)
            } else {
                out.push(path);
                index + 1
            }
        }
        // `a::b::<T>`: the path ended before the turbofish.
        _ => {
            out.push(prefix);
            index
        }
    }
}

fn is_ident(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `text` with comments and the contents of literals blanked, newlines kept so
/// lines still count. A string literal survives only when the whole literal is a
/// `crate::` path.
fn clean(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let at = |index: usize| chars.get(index).copied();
    let mut i = 0;
    while i < chars.len() {
        match (chars[i], at(i + 1)) {
            ('/', Some('/')) => {
                let end = chars[i..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(chars.len(), |n| i + n);
                blank(&mut out, &chars[i..end]);
                i = end;
            }
            ('/', Some('*')) => {
                let (mut depth, mut end) = (0, i);
                while end < chars.len() {
                    match (chars[end], at(end + 1)) {
                        ('/', Some('*')) => (depth, end) = (depth + 1, end + 2),
                        ('*', Some('/')) => (depth, end) = (depth - 1, end + 2),
                        _ => end += 1,
                    }
                    if depth == 0 {
                        break;
                    }
                }
                blank(&mut out, &chars[i..end]);
                i = end;
            }
            ('r', Some('"' | '#')) if starts_raw_string(&chars, i) => {
                let hashes = chars[i + 1..].iter().take_while(|c| **c == '#').count();
                if at(i + 1 + hashes) == Some('"') {
                    let close: Vec<char> = std::iter::once('"')
                        .chain(std::iter::repeat_n('#', hashes))
                        .collect();
                    let body = i + 2 + hashes;
                    let end = chars[body..]
                        .windows(close.len())
                        .position(|window| window == close.as_slice())
                        .map_or(chars.len(), |found| body + found + close.len());
                    string(&mut out, &chars[i..end]);
                    i = end;
                } else {
                    out.push('r');
                    i += 1;
                }
            }
            ('"', _) => {
                let mut end = i + 1;
                while end < chars.len() && chars[end] != '"' {
                    end += if chars[end] == '\\' { 2 } else { 1 };
                }
                let end = (end + 1).min(chars.len());
                string(&mut out, &chars[i..end]);
                i = end;
            }
            // A character literal with an escape runs to its closing quote.
            ('\'', Some('\\')) => {
                let end = chars
                    .get(i + 3..)
                    .and_then(|rest| rest.iter().position(|c| *c == '\''))
                    .map_or(chars.len(), |found| i + 3 + found + 1);
                blank(&mut out, &chars[i..end]);
                i = end;
            }
            // `'x'`; a lifetime has no closing quote two characters on.
            ('\'', Some(_)) if at(i + 2) == Some('\'') => {
                blank(&mut out, &chars[i..i + 3]);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Whether the `r` at `chars[i]` opens a raw string (`r"…"`, `br#"…"#`) rather than
/// ending an identifier.
fn starts_raw_string(chars: &[char], i: usize) -> bool {
    match i.checked_sub(1).map(|before| chars[before]) {
        None => true,
        Some('b') => i < 2 || !is_word(chars[i - 2]),
        Some(before) => !is_word(before),
    }
}

/// `skipped` as spaces, its newlines kept.
fn blank(out: &mut String, skipped: &[char]) {
    out.extend(skipped.iter().map(|c| if *c == '\n' { '\n' } else { ' ' }));
}

/// A string literal into `out`: whole when its content is a `crate::` path, blank
/// otherwise.
fn string(out: &mut String, literal: &[char]) {
    let first = literal.iter().position(|c| *c == '"');
    let last = literal.iter().rposition(|c| *c == '"');
    let content: String = match (first, last) {
        (Some(first), Some(last)) if first < last => literal[first + 1..last].iter().collect(),
        _ => String::new(),
    };
    let is_path = content.starts_with("crate::")
        && content
            .split("::")
            .all(|segment| !segment.is_empty() && segment.chars().all(is_word));
    if is_path {
        out.extend(literal);
    } else {
        blank(out, literal);
    }
}

/// Identifiers, `::`, and single punctuation marks, each with its line.
fn tokens(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if is_word(c) {
            let start = i;
            while i < chars.len() && is_word(chars[i]) {
                i += 1;
            }
            tokens.push((chars[start..i].iter().collect(), line));
        } else if c == ':' && chars.get(i + 1) == Some(&':') {
            tokens.push(("::".to_string(), line));
            i += 2;
        } else {
            tokens.push((c.to_string(), line));
            i += 1;
        }
    }
    tokens
}

fn paths(file: &str, text: &str) -> Vec<String> {
    references(file, text)
        .into_iter()
        .map(|reference| reference.path)
        .collect()
}

/// Each way the tree writes a path to a module of the crate root is read, and the
/// ways it does not (a comment, a `super` that stays inside its module) are not.
// The length is the table: one assertion per spelling of a path, read as the scan's grammar.
#[allow(clippy::too_many_lines)]
#[test]
fn the_scan_reads_each_spelling() {
    let none = Vec::<String>::new();
    // Qualified paths and `use`.
    assert_eq!(paths("agent.rs", "use crate::host::Host;"), ["host::Host"]);
    assert_eq!(
        paths("agent.rs", "fn f() { crate::host::refresh_and_emit(s); }"),
        ["host::refresh_and_emit"]
    );
    assert_eq!(
        paths("agent.rs", "let x = crate::host::Host::new::<u8>();"),
        ["host::Host::new"]
    );
    assert_eq!(
        paths("agent.rs", "use crate::host::Host as H;"),
        ["host::Host"]
    );
    // Groups under a module, nested groups, `self`, globs, and across lines.
    assert_eq!(
        paths(
            "agent.rs",
            "use crate::host::{\n    BlockedReason,\n    Session as S,\n    stall::{Reason, self},\n};"
        ),
        [
            "host::BlockedReason",
            "host::Session",
            "host::stall::Reason",
            "host::stall",
        ]
    );
    assert_eq!(
        paths("agent.rs", "use crate::host::{self, Host};"),
        ["host", "host::Host"]
    );
    assert_eq!(paths("agent.rs", "use crate::host::*;"), ["host"]);
    // A group at the root.
    assert_eq!(
        paths(
            "agent.rs",
            "use crate::{host::{Host, Session}, log::Payload, tui};"
        ),
        ["host::Host", "host::Session", "log::Payload", "tui"]
    );
    // `super` from a file at the root's depth, and from a deeper one: a chain that stops
    // below the root names a path below the module it reaches.
    assert_eq!(paths("agent.rs", "use super::host::Host;"), ["host::Host"]);
    assert_eq!(
        paths("tui/mod.rs", "use super::{host::Host, log};"),
        ["host::Host", "log"]
    );
    assert_eq!(
        paths("agent/step.rs", "use super::super::host::Host;"),
        ["host::Host"]
    );
    assert_eq!(
        paths("agent/step.rs", "use super::host::Host;"),
        ["agent::host::Host"]
    );
    assert_eq!(
        paths("record/log.rs", "use super::persist::Log;"),
        ["record::persist::Log"]
    );
    assert_eq!(
        paths("record/log.rs", "use super::super::host::Host;"),
        ["host::Host"]
    );
    assert_eq!(
        paths("record/log/summary.rs", "use super::super::persist::Log;"),
        ["record::persist::Log"]
    );
    assert_eq!(paths("agent.rs", "use super::*;"), none);
    assert_eq!(
        paths("agent.rs", "pub(super) fn f() {} pub(crate) fn g() {}"),
        none
    );
    // An inline module's `super` is the file's module, which names nothing outside it.
    assert_eq!(
        paths("agent.rs", "mod inner { use super::host::Host; }"),
        none
    );
    assert_eq!(
        paths("record/log.rs", "mod inner { use super::persist::Log; }"),
        none
    );
    assert_eq!(
        paths(
            "agent.rs",
            "mod inner { fn f() { super::super::host::Host } }"
        ),
        ["host::Host"]
    );
    assert_eq!(
        paths(
            "record/log.rs",
            "mod inner { use super::super::persist::Log; }"
        ),
        ["record::persist::Log"]
    );
    // A brace inside a literal does not open or close anything.
    assert_eq!(
        paths(
            "agent.rs",
            "let j = \"{\"; mod inner { use super::host::Host; }"
        ),
        none
    );
    // An attribute path is inside a string.
    assert_eq!(
        paths(
            "inference.rs",
            "#[schemars(schema_with = \"crate::api::Field::name_schema\")]"
        ),
        ["api::Field::name_schema"]
    );
    // Prose that mentions a path in a string is not a path.
    assert_eq!(
        paths("agent.rs", "let m = \"see crate::host::Host\";"),
        none
    );
    // Comments are not code, whichever kind.
    assert_eq!(
        paths(
            "agent.rs",
            "// crate::host::Host\n/// [`crate::host::Host`]\n//! crate::host\n/* crate::host /* nested crate::clients */ crate::routes */"
        ),
        none
    );
    // A `//` or a quote inside a literal does not hide the code after it.
    assert_eq!(
        paths("agent.rs", "let u = \"https://x\"; crate::host::f();"),
        ["host::f"]
    );
    assert_eq!(
        paths("agent.rs", "let q = '\"'; crate::host::f();"),
        ["host::f"]
    );
    assert_eq!(
        paths("agent.rs", "let q = r#\"a \" // b\"#; crate::host::f();"),
        ["host::f"]
    );
    assert_eq!(
        paths("agent.rs", "let q = '\\''; crate::host::f();"),
        ["host::f"]
    );
    assert_eq!(
        paths("agent.rs", "fn f<'a>(x: &'a u8) { crate::host::f(); }"),
        ["host::f"]
    );
    // Line numbers count through comments.
    assert_eq!(
        references(
            "agent.rs",
            "// one\n/* two\nthree */\nuse crate::host::Host;"
        )[0]
        .line,
        4
    );
}

/// A file's key is its module path from the crate root, so a row names a group or a module
/// in one by prefix.
#[test]
fn a_file_is_keyed_by_its_module_path() {
    let importer = |file: &str| {
        Reference {
            file: file.to_string(),
            line: 1,
            path: String::new(),
        }
        .importer()
    };
    assert_eq!(importer("agent.rs"), "agent");
    assert_eq!(importer("clients/tui/mod.rs"), "clients::tui");
    assert_eq!(importer("agent/step.rs"), "agent::step");
    assert_eq!(importer("record/log.rs"), "record::log");
    assert_eq!(importer("record/log/summary.rs"), "record::log::summary");
}
