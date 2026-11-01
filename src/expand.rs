//! Path env-var expansion (docs/tools.md) — `$TMPDIR` in structured tool paths.
//!
//! One expansion choke point sits upstream of classify/spot/exec
//! ([`crate::agent`]), so the deterministic fast-path, the spotter, and the
//! executor all see the *same expanded literal* — an unexpanded `$TMPDIR/foo` is
//! opaque to the spotter (it cannot judge scope on a variable), so
//! expand-before-spot is what makes an env-var path judgeable at all. Bash is
//! exempt: the shell expands in-child against the exec environment.
//!
//! The rules are deliberately narrower than shell expansion:
//!
//! - `$NAME` and `${NAME}`, `NAME = [A-Za-z_][A-Za-z0-9_]*` (ASCII only).
//! - A **listed** name substitutes its frozen boot-time value. The exposed set
//!   starts at exactly `{TMPDIR}` — the architecture is general, the allowlist
//!   is earned (widen it only when an agent actually reaches for another var).
//! - An unlisted or undefined name is a **hard error**, never expand-to-empty
//!   (`$TYPO/important` → `/important` is the footgun) and never leave-verbatim.
//! - `$$` is a literal `$` (the escape hatch for a genuine dollar in a path); a
//!   lone `$` not followed by a name start stays literal.
//! - No `${VAR:-default}` shell syntax, no tilde — a `~` stays literal.
//!
//! Hand-rolled rather than `shellexpand`: the crates in this space ship
//! `${VAR:-default}` (reintroducing "missing → silently something") and
//! Unicode-alphanumeric names, and none default to missing-is-an-error — that
//! rule is the point, so we own the ~40 lines that enforce it.
//!
//! The grant check stays **post-expansion**: expansion resolves the *name*,
//! `policy::path_granted` resolves the *permission* on the expanded literal —
//! so `$TMPDIR/x` gates exactly like the typed-out path would.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::policy;
use crate::record::log::ToolCall;

/// The frozen session env map the structured tools expand against. Built once at
/// boot (the daemon's own environment is stable for the session — an agent's
/// `export` happens in spawned subshells that don't propagate back); snapshotting
/// makes that guarantee explicit rather than incidental.
pub struct Expander {
    vars: BTreeMap<String, String>,
}

impl Expander {
    pub fn new(vars: BTreeMap<String, String>) -> Self {
        Self { vars }
    }

    /// Expand `$NAME` / `${NAME}` occurrences in one path string per the module
    /// rules. `Err` carries a model-legible message naming the variable and the
    /// allowed set.
    pub fn expand(&self, input: &str) -> Result<String, String> {
        let mut out = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '$' {
                out.push(c);
                continue;
            }
            match chars.peek() {
                // `$$` → one literal `$`.
                Some('$') => {
                    chars.next();
                    out.push('$');
                }
                // `${NAME}` — the brace form must close over a valid name;
                // anything else is an attempted expansion we won't guess at.
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for c in chars.by_ref() {
                        if c == '}' {
                            closed = true;
                            break;
                        }
                        name.push(c);
                    }
                    if !closed {
                        return Err(format!("unclosed `${{` in `{input}`"));
                    }
                    if !valid_name(&name) {
                        return Err(format!(
                            "`${{{name}}}` in `{input}` is not a plain variable name — \
                             only `$NAME`/`${{NAME}}` forms expand (no `:-` defaults)"
                        ));
                    }
                    out.push_str(self.lookup(&name, input)?);
                }
                // `$NAME`.
                Some(&c2) if c2.is_ascii_alphabetic() || c2 == '_' => {
                    let mut name = String::new();
                    while let Some(&c2) = chars.peek() {
                        if c2.is_ascii_alphanumeric() || c2 == '_' {
                            name.push(c2);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push_str(self.lookup(&name, input)?);
                }
                // A `$` at the end, or not followed by a name start: literal
                // (least surprising for real paths).
                _ => out.push('$'),
            }
        }
        Ok(out)
    }

    fn lookup(&self, name: &str, input: &str) -> Result<&str, String> {
        self.vars.get(name).map(String::as_str).ok_or_else(|| {
            let listed: Vec<&str> = self.vars.keys().map(String::as_str).collect();
            format!(
                "`${name}` in `{input}` is not an expandable variable — this session expands \
                 only {{{}}}. For a literal `$`, write `$$`.",
                listed.join(", ")
            )
        })
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Expand the path-typed fields of a structured tool call — and only those: a
/// file's *content* is never rewritten. The per-tool path key comes from
/// [`policy::path_spec`], the same single point of schema coupling the
/// classifier uses. Returns `Ok(None)` when the call is untouched (bash, a
/// path-less tool, or no `$` in any target) so the common case never clones.
pub fn expand_call(expander: &Expander, call: &ToolCall) -> Result<Option<ToolCall>, String> {
    if call.name == crate::record::names::tools::BASH {
        return Ok(None); // bash expands in-shell against the child env.
    }
    let Some((key, _, _)) = policy::path_spec(&call.name) else {
        return Ok(None);
    };
    let Some(value) = call.input.get(key) else {
        return Ok(None);
    };
    let expanded: Value = match value {
        Value::String(s) if s.contains('$') => Value::String(expander.expand(s)?),
        Value::Array(items)
            if items
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s.contains('$'))) =>
        {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(match item.as_str() {
                    Some(s) if s.contains('$') => Value::String(expander.expand(s)?),
                    // Non-string elements are malformed input — left for the
                    // classifier's malformed-call handling, not judged here.
                    _ => item.clone(),
                });
            }
            Value::Array(out)
        }
        _ => return Ok(None),
    };
    let mut input = call.input.clone();
    input[key] = expanded;
    Ok(Some(ToolCall::new(
        call.name.clone(),
        call.id.0.clone(),
        input,
    )))
}

#[cfg(test)]
mod test;
