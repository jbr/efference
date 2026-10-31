//! `[env]` table resolution (docs/env.md).

use crate::api::Error;
use crate::env_table::{EnvPolicy, Resolver};

/// Resolve every value the `[env]` table names, failing the boot on the first
/// unresolvable reference (docs/env.md). Loud rather than silent: a reference
/// that does not resolve would otherwise surface much later as a tool's opaque
/// auth error, with nothing connecting the two. The error names the *reference*,
/// which is a pointer and safe to print.
pub(super) fn resolve_env_table(
    resolver: &Resolver,
    table: &std::collections::BTreeMap<String, crate::env_table::EnvEntry>,
) -> Result<Vec<crate::env_table::ResolvedValue>, Error> {
    let (values, errors) = resolver.resolve_table(table);
    if !errors.is_empty() {
        // Each reference's failure rides the returned error: the create response
        // is where the operator reads it, and no print repeats it.
        let detail = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("could not resolve [env] references: {detail}").into());
    }
    Ok(values)
}

/// [`resolve_env_table`] as a name → value map, for the credential lookup that
/// runs before the session's policy exists.
pub(super) fn resolve_env_values(
    resolver: &Resolver,
    table: &std::collections::BTreeMap<String, crate::env_table::EnvEntry>,
) -> Result<std::collections::BTreeMap<String, String>, Error> {
    Ok(resolve_env_table(resolver, table)?
        .into_iter()
        .map(|v| (v.name, v.value))
        .collect())
}

/// The `[env]` table (docs/env.md), resolved against the caller's captured
/// environment: the child-env policy every spawn site applies (strip, and the
/// values the table names), and the session's masker. An unresolvable
/// precedence tie is a startup config error; a value under the maskability
/// floor is a warning naming the variable only. The session snapshot is a copy
/// of the origin, so its references are already in the resolver's cache — no
/// second vault prompt.
pub(super) fn build_env_policy(
    resolver: &std::sync::Arc<Resolver>,
    table: &std::collections::BTreeMap<String, crate::env_table::EnvEntry>,
    inference: &crate::inference::SessionInference,
    base_env: crate::env_table::EnvSnapshot,
) -> Result<EnvPolicy, Error> {
    let session_values = resolve_env_table(resolver, table)?;
    let host_credentials = inference.credential_variables().to_vec();
    let withheld: Vec<&String> = host_credentials
        .iter()
        .filter(|name| base_env.get(name).is_some())
        .filter(|name| table.get(*name).and_then(|entry| entry.strip) != Some(false))
        .collect();
    if !withheld.is_empty() {
        log::debug!(
            "env: withheld from this session's commands — the host's model credentials \
             ({}); a project that needs one sets `strip = false` for it under [env]",
            withheld
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let env_report = EnvPolicy::build_session(
        table,
        &host_credentials,
        base_env,
        &session_values,
        resolver.clone(),
    );
    for name in &env_report.forced_mask {
        log::debug!(
            "env: `{name}` resolves from a command, so its value is masked despite \
             `mask = false` — a value the harness fetched for the session is known to be \
             a secret"
        );
    }
    if !env_report.conflicts.is_empty() {
        // The conflicts ride the returned error: the create response is where
        // the operator reads them, and no print repeats them.
        let detail = env_report
            .conflicts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("ambiguous [env] table: {detail}").into());
    }
    for name in &env_report.below_floor {
        log::debug!(
            "env: `{name}` is set to fewer than {} characters and will not be masked — \
             too short a needle would shred unrelated output",
            crate::env_table::MASKABILITY_FLOOR
        );
    }
    Ok(env_report.policy)
}
