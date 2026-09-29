//! `efference check` — validate a config template and enumerate the trust
//! perimeter it establishes (docs/permissions.md, docs/context.md, docs/tools.md).
//!
//! The same two steps boot performs before it mints a session: parse and
//! validate the file (unknown keys, unsound `[commands]` keys, malformed
//! `[context]` entries), then resolve every `[context]` entry from the current
//! directory — a listed file that cannot be read, or an `exec` entry that fails,
//! stops a boot, so it fails this check. `exec` entries do run. Then it mounts
//! every `[mcp.servers.*]` stanza and reports, per server, the tools it
//! advertises against the file's `tools` grants — the computed perimeter, so an
//! onboarding agent writes the grant with the tool table in front of it.
//!
//! Mounting reuses stored credentials (`auth::stored_header` against the same
//! `KeychainStore` boot uses) and never runs the authorization dance: a server
//! that answers `401` is reported as requiring authorization — run
//! `efference serve` — and counts as not mounted. A required server that does
//! not mount fails the check, same as boot; an `optional = true` one is
//! reported and tolerated. Nothing is minted or bound: this is the terminus
//! for a "set this project up" task, not a session.

use super::*;

pub(crate) async fn check(path: Option<PathBuf>) -> CliResult {
    let path = path.unwrap_or_else(|| PathBuf::from(efference::config::DEFAULT_PATH));
    let loaded = match efference::config::load(Some(&path)) {
        Ok(loaded) => loaded,
        Err(e) => {
            e.eprint();
            return Err(format!("{} does not validate", path.display()).into());
        }
    };
    let role = loaded.role;
    let cwd = std::env::current_dir()?;
    let resolved = match efference::context::resolve(&role.context, &cwd) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(format!(
                "{} parses, but a [context] entry does not resolve from {}",
                path.display(),
                cwd.display()
            )
            .into());
        }
    };
    let preamble = resolved.render().unwrap_or_default();
    println!("ok: {} validates", path.display());
    println!(
        "  [context]: {} section(s), {} bytes rendered into every request's preamble",
        resolved.sections.len(),
        preamble.len()
    );
    println!(
        "  grants: {} tool(s), {} command prefix(es), {} path(s)",
        role.tools.len(),
        role.commands.len(),
        role.paths.len()
    );
    if role.scope.is_none() {
        println!("  note: no `scope` — the spotter judges moves against nothing");
    }
    // Mount every declared server and report the computed perimeter: what each
    // one advertises, and whether the file's `tools` clears it. The per-server
    // tool list is the diff of `definitions()` across that server's own mount —
    // namespaced `<server>__<tool>` by construction, so attribution is exact.
    // Stored credentials are reused; the dance is not run (see module docs).
    if !role.mcp_servers.is_empty() {
        let store = efference::mcp::auth::store::KeychainStore::new(std::sync::Arc::new(
            efference::secrets::Secrets::detect()?,
        ));
        let mut toolbox = Toolbox::bash_and_mcps(cwd.clone(), None)?;
        let mut failed: Option<(String, String)> = None;
        for (name, entry) in &role.mcp_servers {
            let header = efference::mcp::auth::stored_header(&store, name).await?;
            let before = toolbox.definitions().len();
            let mount = toolbox
                .mount_connected(name, &entry.transport, &cwd, header.as_deref())
                .await;
            if let Err(e) = mount {
                let reason = if e
                    .downcast_ref::<efference::mcp::auth::AuthorizationRequired>()
                    .is_some()
                {
                    "requires authorization — run `efference serve`".to_string()
                } else {
                    format!("{e:#}")
                };
                if entry.optional {
                    println!("  [mcp:{name}]: did not mount and is optional — {reason}");
                    continue;
                }
                failed = Some((name.clone(), reason));
                break;
            }
            let definitions = toolbox.definitions();
            let mounted = &definitions[before..];
            let advertised: Vec<&str> = mounted
                .iter()
                .filter_map(|d| d.get("name")?.as_str())
                .collect();
            let weights: Vec<usize> = mounted.iter().map(|d| d.to_string().len()).collect();
            let total: usize = weights.iter().sum();
            let cleared = advertised
                .iter()
                .filter(|tool| efference::policy::tool_granted(&role, tool))
                .count();
            println!(
                "  [mcp:{name}]: {} tool(s), {} cleared by `tools`; {} bytes of definitions",
                advertised.len(),
                cleared,
                total
            );
            for (tool, weight) in advertised.iter().zip(&weights) {
                println!(
                    "    {} {} ({} B)",
                    grant_mark(&role, tool),
                    efference::mcp::display_name(tool),
                    weight
                );
            }
            if cleared < advertised.len() {
                println!(
                    "    note: {} advertised tool(s) not cleared — add exact grants or `{name}.*`, or leave them uncleared deliberately",
                    advertised.len() - cleared,
                );
            }
        }
        if let Some((name, reason)) = failed {
            return Err(format!(
                "{} validates, but mcp server `{name}` did not mount: {reason}; fix it, or set `optional = true` under [mcp.servers.{name}]",
                path.display(),
            )
            .into());
        }
    }
    Ok(())
}

/// The mark printed beside each advertised tool: granted by the file's `tools`
/// (exact name or the terminal-`.*` namespace glob) or not. The decision is
/// `policy::tool_granted`'s; this is only its one-character rendering.
fn grant_mark(role: &efference::config::Config, wire_name: &str) -> &'static str {
    if efference::policy::tool_granted(role, wire_name) {
        "+"
    } else {
        "-"
    }
}

#[cfg(test)]
mod test;
