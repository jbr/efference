//! `efference update --check` (docs/update-check.md): ask efference.net once whether a
//! newer efference is out, and say how to install it.
//!
//! The check runs in this process, not through a host: running the verb is the
//! permission, so it asks whatever `[updates]` says, and it needs no host running.

use super::*;
use efference::update_check::{INSTALL_COMMAND, Release, UpdateCheck};

/// The verb. Without `--check` it refuses, naming the two things that exist: the
/// check, and the install command that updates.
pub(crate) async fn run(check: bool) -> CliResult {
    if !check {
        return Err(format!(
            "efference cannot replace its own binary yet. `efference update --check` says \
             whether a newer version is out, and running the install command again \
             installs it:\n  {INSTALL_COMMAND}"
        )
        .into());
    }
    let check = UpdateCheck::published();
    let newest = check
        .newest()
        .await
        .map_err(|why| format!("could not check for a newer version: {why}"))?;
    print!("{}", report(check.installed_version(), &newest)?);
    Ok(())
}

/// What `--check` prints for the `installed` version and the verified `newest`
/// release.
pub(crate) fn report(installed: &str, newest: &Release) -> Result<String, CliError> {
    let parse = |version: &str| {
        semver::Version::parse(version).map_err(|e| format!("version {version:?}: {e}"))
    };
    let (ours, theirs) = (parse(installed)?, parse(&newest.version)?);
    let mut out = format!(
        "installed: efference {installed}\nnewest:    efference {}\n",
        newest.version
    );
    match theirs.cmp(&ours) {
        std::cmp::Ordering::Greater => out.push_str(&format!(
            "\nTo update, run the install command again:\n  {INSTALL_COMMAND}\n"
        )),
        std::cmp::Ordering::Equal => out.push_str("\nThis is the newest release.\n"),
        std::cmp::Ordering::Less => {
            out.push_str("\nThis build is newer than the newest release.\n")
        }
    }
    Ok(out)
}

#[cfg(test)]
mod test;
