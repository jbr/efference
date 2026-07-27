//! The on-disk TOML shape and its validation into a [`Config`] (docs/permissions.md).

use super::*;

/// The literal TOML shape, before validation. `deny_unknown_fields` makes a typo'd key fail
/// loudly — the schema is the contract. [`validate`] folds this into a [`Config`].
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct RawConfig {
    #[serde(default)]
    scope: Option<String>,
    #[serde(rename = "watch-for", default)]
    watch_for: Vec<String>,
    #[serde(rename = "trust-threshold", default)]
    trust_threshold: Option<f64>,
    /// `[commands]` table: each value is `true` (clear). `false` is rejected in [`validate`]
    /// (deny is unimplemented and the fold is additive-only — it can't subtract).
    #[serde(default)]
    commands: BTreeMap<String, bool>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    paths: BTreeMap<String, RawPathValue>,
    #[serde(default)]
    extends: Vec<String>,
    /// `[context]` table. An [`IndexMap`] (not `BTreeMap`) so **document order is preserved** — the
    /// entries render as preamble sections in the order the operator wrote them. Each value is the
    /// `true` sugar or an inline table ([`RawContextValue`]); [`validate`] interprets each into a
    /// [`ContextEntry`].
    #[serde(default)]
    context: IndexMap<String, RawContextValue>,
}

/// A `[context]` value: the `true`/`false` sugar (a bare read-only path) or an inline table
/// ([`RawContextEntry`]). Interpreted by [`validate_context_entry`].
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum RawContextValue {
    /// `"./CLAUDE.md" = true` → a bare read-only path (the key is the path).
    Sugar(bool),
    /// A table: a labeled group (`{ path|paths = … }`), a dynamic section (`{ exec = … }`), or a
    /// bare path carrying `update` (the key is the path, no `path`/`paths`).
    Table(RawContextEntry),
}

/// The inline-table form of a `[context]` value. Which shape it is (labeled group / exec / bare
/// updatable path) is disambiguated by which keys are present — see [`validate_context_entry`].
#[derive(Deserialize)]
pub(super) struct RawContextEntry {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    exec: Option<String>,
    #[serde(default)]
    update: bool,
    #[serde(default)]
    create: bool,
    #[serde(default)]
    sort: Option<String>,
}

/// A `[paths]` value: either the `true`/`false` sugar (`true` = read+write) or an explicit
/// `{ read = …, write = … }` inline table.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum RawPathValue {
    /// `"." = true` → read+write; `false` → no access.
    Sugar(bool),
    /// `"/refs" = { read = true }` — omitted perms default `false`.
    Perms {
        #[serde(default)]
        read: bool,
        #[serde(default)]
        write: bool,
    },
}

impl RawPathValue {
    fn into_perms(self) -> Perms {
        match self {
            RawPathValue::Sugar(true) => Perms {
                read: true,
                write: true,
            },
            RawPathValue::Sugar(false) => Perms {
                read: false,
                write: false,
            },
            RawPathValue::Perms { read, write } => Perms { read, write },
        }
    }
}

/// A failed validation of an otherwise-parseable TOML file — semantically wrong, not
/// syntactically. Carried by [`LoadError::Validate`].
#[derive(Debug)]
pub enum ValidationError {
    /// A `[commands]` key set to `false`. Additive-only policy can't subtract, so a deny would
    /// silently do nothing — we reject it instead of surprising the author.
    CommandDeny(String),
    /// A `[commands]` key that does not lex to a single simple command (it has operators,
    /// redirections, expansions, or an env-assignment prefix). The fast-path can only soundly
    /// prefix-match simple commands, so an un-lexable key fails the whole file — no silent skip.
    CommandNotSimple(String),
    /// A `[context]` entry set to `false`. There is nothing to disable — an unwanted entry is
    /// simply not listed — so a `false` is rejected rather than silently doing nothing.
    ContextDisabled(String),
    /// A `[context]` `exec` entry that also carries `path`/`paths`/grants/`sort` — the source is
    /// ambiguous (command stdout *or* files?). An `exec` section has no files to grant or sort.
    ContextExecConflict(String),
    /// A group-only key (`create` or `sort`, named in the payload) on a **bare path** (a `[context]`
    /// entry whose key *is* the path, with no `path`/`paths`). Both are about a *location of many
    /// files*; a single file has nowhere to create into and nothing to sort.
    ContextBarePathGrant(String, &'static str),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::CommandDeny(key) => write!(
                f,
                "command `{key}` is set to false, but command-deny is not implemented \
                 (config is additive-only); remove the entry instead"
            ),
            ValidationError::CommandNotSimple(key) => write!(
                f,
                "command key `{key}` is not a single simple command (no operators, \
                 redirections, expansions, or env-assignments) — the fast-path can only \
                 prefix-match simple commands"
            ),
            ValidationError::ContextDisabled(key) => write!(
                f,
                "context entry `{key}` is set to false; there is nothing to disable — remove the \
                 entry instead of listing it as off"
            ),
            ValidationError::ContextExecConflict(key) => write!(
                f,
                "context entry `{key}` mixes `exec` with `path`/`paths`/grants/`sort` — an exec \
                 section's body is the command's stdout and has no files to grant or sort"
            ),
            ValidationError::ContextBarePathGrant(key, which) => write!(
                f,
                "context entry `{key}` uses `{which}` on a bare path — `{which}` needs a labeled \
                 group with `path`/`paths` (a single file has nowhere to create into / nothing to \
                 sort)"
            ),
        }
    }
}

/// Parse TOML text into a [`Config`] (decode → validate). The `path` labels diagnostics only.
pub(super) fn parse(path: &Path, text: &str) -> Result<Config, LoadError> {
    let raw: RawConfig =
        toml_edit::de::from_str(text).map_err(|e| LoadError::Parse(path.to_path_buf(), e))?;
    validate(raw).map_err(|e| LoadError::Validate(path.to_path_buf(), e))
}

/// Fold a [`RawConfig`] into the materialized [`Config`], failing closed on any unsound entry.
/// The two soundness gates live here: every `[commands]` key must lex to a simple command
/// (reusing the fast-path's own lexer so the key and the checked command agree token-for-token),
/// and a `false` command value is rejected (additive-only can't deny).
pub(super) fn validate(raw: RawConfig) -> Result<Config, ValidationError> {
    let mut commands = BTreeSet::new();
    for (key, granted) in raw.commands {
        if !granted {
            return Err(ValidationError::CommandDeny(key));
        }
        if crate::policy::simple_command_tokens(&key).is_none() {
            return Err(ValidationError::CommandNotSimple(key));
        }
        commands.insert(key);
    }
    let paths = raw
        .paths
        .into_iter()
        .map(|(k, v)| (k, v.into_perms()))
        .collect();
    let mut context = Vec::with_capacity(raw.context.len());
    for (key, value) in raw.context {
        context.push(validate_context_entry(key, value)?);
    }
    Ok(Config {
        scope: raw.scope,
        watch_for: raw.watch_for,
        trust_threshold: raw.trust_threshold,
        commands,
        tools: raw.tools.into_iter().collect(),
        paths,
        extends: raw.extends,
        context,
    })
}

/// Interpret one `[context]` `(key, value)` into a [`ContextEntry`], failing closed on an illegal
/// shape (docs/context.md). The value's shape decides the kind:
///
/// - `true` → a bare read-only path (the key is the path, no heading).
/// - a table with `exec` → a labeled dynamic section (no files/grants/sort allowed alongside).
/// - a table with `path`/`paths` → a labeled group (may carry `update`/`create`/`sort`).
/// - a table with neither → a bare path (the key is the path) that may carry only `update`;
///   `create`/`sort` are group-only and rejected here.
pub(super) fn validate_context_entry(
    key: String,
    value: RawContextValue,
) -> Result<ContextEntry, ValidationError> {
    let entry = match value {
        RawContextValue::Sugar(true) => ContextEntry {
            heading: None,
            source: ContextSource::Files(vec![key]),
            grants: ContextGrants::default(),
            sort: None,
        },
        RawContextValue::Sugar(false) => return Err(ValidationError::ContextDisabled(key)),
        RawContextValue::Table(t) => {
            let has_files = t.path.is_some() || !t.paths.is_empty();
            if let Some(exec) = t.exec {
                // A dynamic section is a labeled group whose body is command stdout: it has no
                // files, so file grants and a sort would be meaningless — reject the mix.
                if has_files || t.update || t.create || t.sort.is_some() {
                    return Err(ValidationError::ContextExecConflict(key));
                }
                ContextEntry {
                    heading: Some(key),
                    source: ContextSource::Exec(exec),
                    grants: ContextGrants::default(),
                    sort: None,
                }
            } else if has_files {
                // Labeled group: the key is a heading; the location is `path` then any `paths`.
                let mut files = Vec::new();
                files.extend(t.path);
                files.extend(t.paths);
                ContextEntry {
                    heading: Some(key),
                    source: ContextSource::Files(files),
                    grants: ContextGrants {
                        update: t.update,
                        create: t.create,
                    },
                    sort: t.sort,
                }
            } else {
                // Bare path carrying grants: the key itself is the path, no heading. Only `update`
                // is legal here — `create`/`sort` are about a location of many files.
                if t.create {
                    return Err(ValidationError::ContextBarePathGrant(key, "create"));
                }
                if t.sort.is_some() {
                    return Err(ValidationError::ContextBarePathGrant(key, "sort"));
                }
                ContextEntry {
                    heading: None,
                    source: ContextSource::Files(vec![key]),
                    grants: ContextGrants {
                        update: t.update,
                        create: false,
                    },
                    sort: None,
                }
            }
        }
    };
    Ok(entry)
}
