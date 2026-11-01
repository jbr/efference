//! The `[env]` table — stripping at spawn, masking at append (docs/env.md).
//!
//! One name-keyed table in `efference.toml`. Each entry answers two independent
//! questions asked at two different points:
//!
//! - **`strip`** — at subprocess spawn: does the child environment include this
//!   variable? Applied by [`ChildEnv`] at every spawn site.
//! - **`mask`** — at log append: does the log ever store this variable's value?
//!   Applied by [`Masker`] (see [`mask`]), which every text-bearing payload
//!   passes through on its way into [`crate::record::persist::Log`].
//!
//! An entry may also say where its value **comes from** — a literal, or a
//! reference resolved by running a command ([`resolve`]). A resolved value is
//! injected into every child environment by the same [`ChildEnv`] that strips,
//! and a value that came out of a command is masked by construction.
//!
//! The two are a 2×2, not a scale: strip-without-mask is the hygiene cell
//! (`COLORTERM`, harness self-vars), and mask-without-strip is the ordinary
//! secret a tool still needs.
//!
//! Masking is **irreversible filtering upstream of the log**, not a
//! [`WireState`](crate::record::log::WireState) — the value never becomes `raw`, so
//! there is no reversible verdict to flip. See docs/log-and-projection.md for
//! why that inversion is deliberate.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;

mod mask;
pub use mask::{MARKER_PREFIX_CHARS, MASKABILITY_FLOOR, Masker, marker};

mod child;
pub use child::ChildEnv;

mod snapshot;
pub use snapshot::EnvSnapshot;

mod resolve;
pub use resolve::{RESOLVE_TIMEOUT, ResolveError, ResolvedValue, Resolver, SCHEMES, Source};

mod launch;
pub use launch::{HOOK_OUTPUT_LIMIT, HOOK_TIMEOUT, LaunchError, environment_at};

#[cfg(test)]
mod test;

/// One `[env]` entry as the operator wrote it. `mask` and `strip` are `Option`
/// because **omission means inherit**: a user entry at a key that also has a
/// built-in default keeps the built-in's answer for whatever it does not name,
/// so `"*_KEY" = { strip = true }` composes with the inherited `mask = true`
/// rather than disabling it. Disabling is possible, but only by saying so
/// (`mask = false`).
///
/// Two spellings parse: the inline table (`{ mask = true }`, `{ value = "…" }`,
/// `{ cmd = "…" }`) and a **bare string**, which names the value and is
/// scheme-sniffed (`NAME = "op://Vault/Item/field"`). A bare *boolean*
/// (`"*_KEY" = false`) remains a parse error: it cannot say which of the two
/// questions it is answering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvEntry {
    pub mask: Option<bool>,
    pub strip: Option<bool>,
    /// `{ value = "…" }` — a literal, used verbatim. Never scheme-sniffed, which
    /// makes it the escape hatch for a literal that happens to look like a
    /// registered reference.
    pub value: Option<String>,
    /// `{ cmd = "…" }` — the value is this command's stdout. The primitive every
    /// registered scheme is sugar over ([`resolve::SCHEMES`]).
    pub cmd: Option<String>,
    /// The bare-string form, before sniffing. Not a TOML key — an entry written
    /// as an inline table can only reach the two explicit fields above, so the
    /// sniffed and the forced-literal spellings stay distinguishable.
    pub bare: Option<String>,
}

// The inline-table spelling of [`EnvEntry`], kept separate so `deny_unknown_fields` still names
// a typo'd key (`{ msak = true }`) instead of falling through to the bare-string arm. The `///`
// docs on its fields are the published project schema's hover text, and its own is the union's
// (docs/config-schemas.md).
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "EnvTable")]
struct EnvEntryFields {
    /// Keep this variable's value out of the session log: it is replaced with a marker
    /// wherever it appears.
    #[serde(default)]
    mask: Option<bool>,
    /// Leave this variable out of every subprocess the session starts.
    #[serde(default)]
    strip: Option<bool>,
    /// A literal value, used as written; never resolved as a reference.
    #[serde(default)]
    value: Option<String>,
    /// A command whose output is the value, run when the session starts. The value is
    /// masked.
    #[serde(default)]
    cmd: Option<String>,
}

impl<'de> Deserialize<'de> for EnvEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor;
        impl<'de> serde::de::Visitor<'de> for EntryVisitor {
            type Value = EnvEntry;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "an inline table (`{ mask = true, strip = false }`, `{ value = \"…\" }`, \
                     `{ cmd = \"…\" }`) or a value string (`\"op://Vault/Item/field\"`)",
                )
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<EnvEntry, E> {
                Ok(EnvEntry {
                    bare: Some(v.to_string()),
                    ..EnvEntry::default()
                })
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<EnvEntry, A::Error> {
                let fields =
                    EnvEntryFields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(EnvEntry {
                    mask: fields.mask,
                    strip: fields.strip,
                    value: fields.value,
                    cmd: fields.cmd,
                    bare: None,
                })
            }
        }
        deserializer.deserialize_any(EntryVisitor)
    }
}

/// The two spellings [`EnvEntry`]'s hand-written `Deserialize` accepts, for the
/// published schema (docs/env.md): a value string or the inline table.
impl schemars::JsonSchema for EnvEntry {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EnvEntry".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        /// One variable's policy: an inline table, or a string that sets its value. A
        /// string in a registered reference scheme, such as `"op://Vault/Item/field"`, is
        /// resolved when the session starts, and its value is masked. In the table, an
        /// omitted `mask` or `strip` keeps the built-in answer for the name.
        #[derive(schemars::JsonSchema)]
        #[schemars(untagged)]
        #[allow(dead_code)] // never constructed: it exists to derive the schema
        enum Spellings {
            Value(String),
            Table(EnvEntryFields),
        }
        Spellings::json_schema(generator)
    }
}

/// A resolved entry: both questions answered for one variable name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnvRule {
    pub mask: bool,
    pub strip: bool,
}

/// The built-in table entries (docs/env.md). Default-on is right for a safety
/// feature, and being *entries* — rather than code — keeps them visible and
/// overridable by an exact-name entry, which beats a glob.
pub const BUILT_IN: &[(&str, EnvRule)] = &[
    (
        "*_CREDENTIALS",
        EnvRule {
            mask: true,
            strip: false,
        },
    ),
    (
        "*_KEY",
        EnvRule {
            mask: true,
            strip: false,
        },
    ),
    (
        "*_PASSWORD",
        EnvRule {
            mask: true,
            strip: false,
        },
    ),
    (
        "*_SECRET",
        EnvRule {
            mask: true,
            strip: false,
        },
    ),
    (
        "*_TOKEN",
        EnvRule {
            mask: true,
            strip: false,
        },
    ),
];

/// The merged table — built-ins first, then the operator's entries folded in at
/// their keys. Resolution is by [`Table::resolve`], never by file order (TOML
/// tables are semantically unordered).
#[derive(Debug, Clone, Default)]
pub struct Table {
    entries: BTreeMap<String, EnvRule>,
}

impl Table {
    /// Merge the operator's `[env]` entries over the built-ins. At a key both
    /// declare, an omitted field inherits the built-in's answer.
    pub fn merge(user: &BTreeMap<String, EnvEntry>) -> Self {
        Self::merge_with_host(user, &[])
    }

    /// [`merge`](Self::merge) with the host's credential variables as further
    /// built-ins: each an exact name that strips and masks (docs/env.md). An exact
    /// name outranks the `*_KEY` glob, and an operator entry at the same name
    /// inherits the fields it omits — so `strip = false` opts one back in and
    /// keeps it masked.
    pub fn merge_with_host(user: &BTreeMap<String, EnvEntry>, host_credentials: &[String]) -> Self {
        let mut entries: BTreeMap<String, EnvRule> = BUILT_IN
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect();
        for name in host_credentials {
            entries.insert(
                name.clone(),
                EnvRule {
                    mask: true,
                    strip: true,
                },
            );
        }
        for (key, entry) in user {
            let inherited = entries.get(key).copied().unwrap_or_default();
            entries.insert(
                key.clone(),
                EnvRule {
                    mask: entry.mask.unwrap_or(inherited.mask),
                    strip: entry.strip.unwrap_or(inherited.strip),
                },
            );
        }
        Self { entries }
    }

    /// Every key in the effective table, in sorted order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, EnvRule)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), *v))
    }

    /// Resolve one variable name against the table.
    ///
    /// **Precedence:** an exact-name key beats every glob; among matching globs
    /// the one with more literal (non-`*`) characters wins. A tie between globs
    /// at the same literal count is *unresolvable* — it is reported as a
    /// [`Conflict`] for the caller to turn into a startup config error. The
    /// rule returned in that case is the union (`mask`/`strip` OR-ed), so a
    /// caller that cannot fail still fails closed rather than silently
    /// dropping the entry.
    pub fn resolve(&self, name: &str) -> (EnvRule, Option<Conflict>) {
        if let Some((_, rule)) = self
            .entries
            .get_key_value(name)
            .filter(|(key, _)| !key.contains('*'))
        {
            return (*rule, None);
        }
        let mut best: Option<usize> = None;
        let mut winners: Vec<(&str, EnvRule)> = Vec::new();
        for (key, rule) in &self.entries {
            if !key.contains('*') || !glob_match(key, name) {
                continue;
            }
            let literals = key.chars().filter(|c| *c != '*').count();
            match best {
                Some(b) if literals < b => continue,
                Some(b) if literals == b => winners.push((key, *rule)),
                _ => {
                    best = Some(literals);
                    winners.clear();
                    winners.push((key, *rule));
                }
            }
        }
        match winners.len() {
            0 => (EnvRule::default(), None),
            1 => (winners[0].1, None),
            _ => {
                let union = winners
                    .iter()
                    .fold(EnvRule::default(), |acc, (_, r)| EnvRule {
                        mask: acc.mask || r.mask,
                        strip: acc.strip || r.strip,
                    });
                let conflict = Conflict {
                    name: name.to_string(),
                    keys: winners.iter().map(|(k, _)| (*k).to_string()).collect(),
                };
                (union, Some(conflict))
            }
        }
    }
}

/// Two glob keys matched one variable name with the same literal-character
/// count, so neither is more specific. Reported at startup as a config error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub name: String,
    pub keys: Vec<String>,
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` matches [env] keys {} equally (same literal-character count) — \
             precedence is exact name, then more literal characters; add an exact \
             `{}` entry to decide it",
            self.name,
            self.keys
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(" and "),
            self.name
        )
    }
}

/// `*` matches any run of characters (including none); every other character is
/// literal. Deliberately the whole syntax — a `[commands]`-style lexer has no
/// job here, and `?`/character classes buy nothing an operator has asked for.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    // The first segment is anchored at the start.
    let Some(stripped) = rest.strip_prefix(parts[0]) else {
        return false;
    };
    rest = stripped;
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate().skip(1) {
        if i == last {
            // The final segment is anchored at the end.
            return rest.len() >= part.len() && rest.ends_with(part);
        }
        if part.is_empty() {
            continue;
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// The resolved `[env]` policy for a session: the effective table, the child
/// environment derived from it against the caller's snapshot, the session's
/// [`Masker`], and the [`Resolver`] whose cache a later config edit re-reads.
#[derive(Debug, Clone)]
pub struct EnvPolicy {
    table: Table,
    /// The host's credential variables, merged as built-ins (docs/env.md) —
    /// kept so a config edit's re-merge keeps them.
    host_credentials: Vec<String>,
    base: EnvSnapshot,
    child_env: ChildEnv,
    masker: Arc<Masker>,
    resolver: Arc<Resolver>,
}

/// What [`EnvPolicy::build`] found while resolving. `conflicts` is a startup
/// config error; `below_floor` is a startup warning naming the variable (name
/// only — printing the short value would be the leak the floor exists to avoid);
/// `forced_mask` names entries whose `mask = false` was overruled because the
/// value came out of a command.
pub struct EnvReport {
    pub policy: EnvPolicy,
    pub conflicts: Vec<Conflict>,
    pub below_floor: Vec<String>,
    pub forced_mask: Vec<String>,
}

impl Default for EnvPolicy {
    fn default() -> Self {
        Self::build(&BTreeMap::new()).policy
    }
}

impl EnvPolicy {
    /// Resolve the table against this process's environment, with no reference
    /// resolution — the shape callers that never spawn a vault command use
    /// (the dev views). Boot uses [`build_snapshot`](Self::build_snapshot).
    pub fn build(user: &BTreeMap<String, EnvEntry>) -> EnvReport {
        Self::build_with_values(user, &[], Arc::new(Resolver::default()))
    }

    /// [`build`](Self::build) with the table's values already resolved
    /// ([`Resolver::resolve_table`]), capturing the local process environment.
    /// Hosted preparation uses [`build_snapshot`](Self::build_snapshot) instead.
    pub fn build_with_values(
        user: &BTreeMap<String, EnvEntry>,
        resolved: &[ResolvedValue],
        resolver: Arc<Resolver>,
    ) -> EnvReport {
        Self::build_snapshot(
            user,
            EnvSnapshot::capture().expect("valid local environment"),
            resolved,
            resolver,
        )
    }

    /// [`build`](Self::build) over an explicit environment snapshot — the interface
    /// the tests drive, and what keeps resolution a pure function of
    /// `(table, environment, resolved values)`.
    pub fn build_from(user: &BTreeMap<String, EnvEntry>, vars: &[(String, String)]) -> EnvReport {
        let base = EnvSnapshot::new(vars.iter().cloned().collect()).expect("valid environment");
        let resolver = match std::env::current_dir() {
            Ok(cwd) => Resolver::with_environment(cwd, base.clone()),
            Err(error) => Resolver::new(Box::new(move |_| {
                Err(format!("could not resolve cwd: {error}"))
            })),
        };
        Self::build_snapshot(user, base, &[], Arc::new(resolver))
    }

    /// The core: table precedence over `vars`, then the resolved values folded
    /// in as injections and (for a value that came out of a command) needles.
    pub fn build_from_with(
        user: &BTreeMap<String, EnvEntry>,
        vars: &[(String, String)],
        resolved: &[ResolvedValue],
        resolver: Arc<Resolver>,
    ) -> EnvReport {
        Self::build_snapshot(
            user,
            EnvSnapshot::new(vars.iter().cloned().collect()).expect("valid environment"),
            resolved,
            resolver,
        )
    }

    /// Resolve against the caller's complete base, retained for later masking edits.
    pub fn build_snapshot(
        user: &BTreeMap<String, EnvEntry>,
        base: EnvSnapshot,
        resolved: &[ResolvedValue],
        resolver: Arc<Resolver>,
    ) -> EnvReport {
        Self::build_session(user, &[], base, resolved, resolver)
    }

    /// [`build_snapshot`](Self::build_snapshot) for a hosted session: the host's
    /// credential variables join the table as built-ins that strip and mask, so a
    /// session's tools do not inherit the keys the host reaches its models with
    /// unless the project opts one back in (docs/env.md).
    pub fn build_session(
        user: &BTreeMap<String, EnvEntry>,
        host_credentials: &[String],
        base: EnvSnapshot,
        resolved: &[ResolvedValue],
        resolver: Arc<Resolver>,
    ) -> EnvReport {
        let table = Table::merge_with_host(user, host_credentials);
        let mut conflicts = Vec::new();
        let mut below_floor = Vec::new();
        let mut strip = Vec::new();
        let mut to_mask: Vec<(String, String)> = Vec::new();
        for (name, value) in base.iter() {
            let (rule, conflict) = table.resolve(name);
            if let Some(conflict) = conflict {
                conflicts.push(conflict);
            }
            if rule.strip {
                strip.push(name.clone());
            }
            let masked = rule.mask;
            if masked {
                if value.chars().count() < MASKABILITY_FLOOR {
                    below_floor.push(name.clone());
                } else {
                    to_mask.push((name.clone(), value.clone()));
                }
            }
        }
        // The values the table names (docs/env.md): injected into every child,
        // and — when the value came out of a command — masked by construction,
        // because the harness *knows* it is a secret without any `*_KEY` glob.
        // `mask = false` on such an entry does not disable it; it is reported.
        let mut forced_mask = Vec::new();
        let mut inject: BTreeMap<String, String> = BTreeMap::new();
        for value in resolved {
            let rule = table.resolve(&value.name).0;
            let masked = value.secret || rule.mask;
            // Overruling is only worth saying out loud when the operator asked
            // for the opposite in as many words.
            if value.secret && user.get(&value.name).and_then(|e| e.mask) == Some(false) {
                forced_mask.push(value.name.clone());
            }
            if masked {
                if value.value.chars().count() < MASKABILITY_FLOOR {
                    below_floor.push(value.name.clone());
                } else {
                    to_mask.push((value.name.clone(), value.value.clone()));
                }
            }
            inject.insert(value.name.clone(), value.value.clone());
        }
        strip.sort();
        strip.dedup();
        let masker = Arc::new(Masker::default());
        masker.absorb(&to_mask);
        EnvReport {
            policy: Self {
                table,
                host_credentials: host_credentials.to_vec(),
                child_env: ChildEnv::from_snapshot(base.clone(), strip).with_inject(inject),
                base,
                masker,
                resolver,
            },
            conflicts,
            below_floor,
            forced_mask,
        }
    }

    /// Re-resolve against the session's base environment after a config edit and
    /// **add** whatever is newly maskable, including the values of any
    /// references the edited table names. The needle set is append-only for
    /// the session: a value that has since rotated out is still a secret, and
    /// still sits in files the session may read.
    ///
    /// References resolve through the session's cached [`Resolver`], so an
    /// unchanged reference costs nothing and a vault does not re-prompt. A
    /// reference that fails to resolve is returned for the caller to hand back
    /// to whoever made the edit — never swallowed, and never left to surface as
    /// a tool's opaque auth error later.
    ///
    /// Injection is *not* re-derived here, for the same reason strip is not: the
    /// child environment is fixed at boot with the spawn sites that hold it, so
    /// a value added by an edit reaches children on the next session.
    pub fn absorb_config(&self, user: &BTreeMap<String, EnvEntry>) -> Vec<ResolveError> {
        let table = Table::merge_with_host(user, &self.host_credentials);
        let mut to_mask: Vec<(String, String)> = self
            .base
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .filter(|(name, value)| {
                table.resolve(name).0.mask && value.chars().count() >= MASKABILITY_FLOOR
            })
            .collect();
        let (resolved, errors) = self.resolver.resolve_table(user);
        for value in resolved {
            let masked = value.secret || table.resolve(&value.name).0.mask;
            if masked && value.value.chars().count() >= MASKABILITY_FLOOR {
                to_mask.push((value.name, value.value));
            }
        }
        self.masker.absorb(&to_mask);
        errors
    }

    pub fn table(&self) -> &Table {
        &self.table
    }

    /// The strip + inject policy every spawn site applies (docs/env.md).
    pub fn child_env(&self) -> ChildEnv {
        self.child_env.clone()
    }

    /// The session's reference resolver, shared so a re-resolution hits the
    /// cache rather than a vault.
    pub fn resolver(&self) -> Arc<Resolver> {
        Arc::clone(&self.resolver)
    }

    pub fn masker(&self) -> Arc<Masker> {
        Arc::clone(&self.masker)
    }

    /// Mask a string that is about to be used somewhere other than a log append
    /// — the boot preamble, which rides the request's `system` field and is
    /// snapshotted into the log, and must be byte-identical in both.
    pub fn mask(&self, text: &str) -> String {
        self.masker.mask(text).into_owned()
    }

    /// Mask a resolved `[context]` listing before the preamble is rendered from
    /// it, so the rendered `system` string and the `Preamble` snapshot of it
    /// stay byte-identical (docs/context.md).
    pub fn mask_context(&self, resolved: &mut crate::record::log::context::Resolved) {
        self.masker.mask_context(resolved);
    }
}
