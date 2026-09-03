//! `efference check` — validate a config template offline (docs/permissions.md,
//! docs/context.md).
//!
//! The same two steps `serve` performs before it mints a session: parse and
//! validate the file (unknown keys, unsound `[commands]` keys, malformed
//! `[context]` entries), then resolve every `[context]` entry from the current
//! directory — a listed file that cannot be read, or an `exec` entry that fails,
//! stops a boot, so it fails this check. `exec` entries do run. Nothing is
//! minted, bound, or sent: this is the terminus for a "set this project up"
//! task, not a session.

use super::*;

pub(crate) fn check(path: Option<PathBuf>) -> CliResult {
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
    Ok(())
}
