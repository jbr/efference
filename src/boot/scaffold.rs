//! The session's own `config.toml`: its first contents and the grants layered over it
//! (docs/permissions.md).

use std::path::Path;

use super::{BASE_DIR, CONFIG_NAME};
use crate::api::Error;
use crate::config::{ConfigSource, LoadedConfig};

/// Layer the harness's self-surface protection over the operator's grants (docs/permissions.md).
///
/// The session dir holds this session's control socket, its append-only log, and — under the same
/// base — every *other* session's dir; all of it sits inside the operator's `.` grant, so nothing
/// stops the session from writing its own log (corrupting the log) or POSTing to its own control
/// socket (approving its own gates, injecting a user turn) without this. Deny the whole base dir,
/// then re-allow read-only on two things of *this* session's:
///
/// - its `config.toml` — the sanctioned scope-expansion path: the session reads its own perimeter
///   and *edits* the file, which is a write the fast-path won't clear, so it gates for the
///   operator;
/// - its attachment store — the files the operator attached, which the notice tells the model to
///   pull with `read` / `read_image` (docs/inbox.md). Without the re-allow, every pull gates.
///
/// Inserted after the operator config so an equal-specificity operator key is overridden;
/// `policy::path_granted`'s longest-prefix rule still lets a strictly deeper operator grant win.
pub(super) fn self_surface_grants(
    paths: &mut std::collections::BTreeMap<String, crate::config::Perms>,
    session_dir: &Path,
) {
    let read_only = crate::config::Perms {
        read: true,
        write: false,
    };
    paths.insert(BASE_DIR.to_string(), crate::config::Perms::default());
    for path in [
        session_dir.join(CONFIG_NAME),
        crate::attachments::dir_for_session(session_dir),
    ] {
        paths.insert(path.to_string_lossy().into_owned(), read_only);
    }
}

/// Written into a fresh session's `config.toml` when there's no origin to snapshot — a header that
/// teaches the schema, plus the one default that isn't empty: an `Environment at boot` `[context]`
/// exec entry, orientation expressed as config rather than hardcoded (docs/context.md). It's a real,
/// visible line in the session's own file, so a session that `cat`s its config sees it and an
/// operator can edit or delete it (docs/permissions.md).
pub const CONFIG_SCAFFOLD: &str = "\
# Your session's permissions config. Edit it to request changes — edits are reviewed before they
# take effect, and a malformed file is rejected wholesale (fail-closed): if an edit doesn't seem to
# \"take\", check that the file still parses. Unknown keys are rejected. Schema (all keys optional):
#   scope = \"...\"                       # STRING, top-level — what this session is for (NOT a [scope] table)
#   watch-for = [\"...\"]                 # hazards the permission spotter should watch for
#   trust-threshold = 0.5               # where the watch-window/gate cutoffs sit (optional float)
#   tools = [\"read\", \"list\", \"search\"]  # structured tools you may use
#   [commands]                          # bash prefixes → clear/watch/gate/deny (true = clear, false = deny),
#                                       #   e.g. cargo = true, \"git diff\" = true, \"git push\" = \"deny\".
#                                       #   The longest matching prefix governs, so a broad grant can carry
#                                       #   carve-outs: git = true with \"git push\" = \"deny\".
#   [paths]                             # filesystem grants, e.g. \".\" = true or \"/refs\" = { read = true }
#   [context]                           # knowledge the session boots with (docs/context.md), e.g.
#                                       #   \"CLAUDE.md\" = true, Label = { exec = \"...\" } (stdout at boot),
#                                       #   or Label = { builtin = \"guide\" } (the harness's own docs)
#   [env]                               # per-variable env policy (docs/env.md): strip = keep it out of
#                                       #   spawned children, mask = never store its value in the log.
#                                       #   e.g. \"*_KEY\" = { mask = true } or \"CI_*\" = { strip = true }.
#                                       #   *_KEY/*_SECRET/*_TOKEN/*_PASSWORD/*_CREDENTIALS mask by default.
#                                       #   An entry may also name the value: RUST_LOG = { value = \"debug\" }
#                                       #   (literal), SOME_KEY = \"op://Vault/Item/field\" (resolved at boot
#                                       #   and always masked), or { cmd = \"...\" } for any other vault.

# What this session is for. The permission spotter judges each move against it, so an
# ordinary look around the project reads as part of the task (docs/permissions.md).
scope = \"Set this project up for efference: look over the project to learn how it is built and laid out, ask the operator what they want sessions here to be allowed and to know, write efference.toml and AGENTS.md at the project root, and keep efference's files out of version control in .gitignore or .git/info/exclude, as the operator chooses. Stay out of any file the operator asks you to avoid.\"

# The kickoff: in a session that has said nothing yet, the harness sends this as the opening
# turn, so the agent speaks first (docs/context.md). Like the preamble, it is not shown as a
# turn; what you see is the reply. Delete the line for a session that waits for you.
first-turn = \"efference opened this session in a directory with no efference.toml, after the operator chose to set it up. They have not said anything yet.\"

# A session here is setting the project up, which means reading it and writing efference.toml
# and AGENTS.md into it: the built-in tools and the directory are granted so that work is not
# a string of approvals. This session's own config (this file) stays review-only, and
# `.efference/` stays denied, whatever is granted here (docs/permissions.md).
tools = [\"efference.*\"]

# The one command cleared by name: validating the template the session is about to write
# (docs/operating.md). Every other command goes through the spotter.
[commands]
\"efference check\" = true

[paths]
\".\" = true

# Orientation as config: this section's stdout is frozen into the session preamble at boot.
[context]
\"Environment at boot\" = { exec = 'echo \"working directory: $PWD\"; echo \"platform: $(uname -sm)\"; git rev-parse --is-inside-work-tree >/dev/null 2>&1 && echo \"git: a repository\" || echo \"git: not a repository\"; echo \"top-level entries:\"; ls -A' }
# The harness's own usage documents, listed as read-on-demand stubs. This is what makes a
# session with no project config recoverable: it starts knowing how to learn everything
# else, including how to write this file. Delete the line to stop listing them; the
# documents stay readable at their paths either way.
EfferenceGuide = { builtin = \"guide\" }
# The setup task itself, in full rather than as a stub, so the first turn starts with it in
# hand. `guide/<slug>` inlines any one document.
\"Setting up a project\" = { builtin = \"guide/new-project\" }
";

/// Snapshot the origin config into the session's own `config.toml` *once* (a fresh session). The
/// origin (`--config`/the `efference.toml` probe) is a template consumed at boot; thereafter the
/// per-session snapshot is the live, editable, watched policy — so two sessions sharing an origin
/// diverge freely, and the session can `cat` its config to see its perimeter and edit it to
/// request changes. The `else` branch (a snapshot that already exists) is unreachable while
/// `mint_session_dir` refuses an existing dir; it is what a resumption design would re-enter.
pub(super) fn snapshot_config(origin: &LoadedConfig, session_config: &Path) -> Result<(), Error> {
    if !session_config.exists() {
        // The origin was validated above (fail fast on a malformed template, before binding
        // anything); copy it verbatim (comments intact) — or scaffold an empty config when
        // there's none.
        match &origin.source {
            ConfigSource::Path(p) => {
                std::fs::copy(p, session_config)?;
                eprintln!(
                    "config: snapshot of {} → {}",
                    p.display(),
                    session_config.display()
                );
            }
            ConfigSource::Default => {
                std::fs::write(session_config, CONFIG_SCAFFOLD)?;
                eprintln!("config: scaffolded {}", session_config.display());
            }
        }
    } else {
        eprintln!("config: reusing {}", session_config.display());
    }
    Ok(())
}

/// Load the session's own config with the self-surface grants applied.
///
/// The per-session snapshot *is* the live config now (a malformed hand-edit on resume fails fast
/// here). Everything downstream — the preamble, fs-watch, `/state`, persisted config-edits — keys
/// off this path, not the origin.
pub(super) fn load_session_config(
    session_config: &Path,
    session_dir: &Path,
) -> Result<LoadedConfig, Error> {
    let mut loaded = match crate::config::load(Some(session_config)) {
        Ok(l) => l,
        Err(e) => {
            e.eprint();
            return Err("could not load session config".into());
        }
    };

    self_surface_grants(&mut loaded.role.paths, session_dir);
    Ok(loaded)
}

/// Mint the per-session scratch dir (docs/tools.md) and grant it rw.
///
/// Minted mkdtemp-style, exported to every tool child as `$TMPDIR`, and always granted rw — the
/// sanctioned answer to driven sessions reaching for `/tmp` (the most-repeated gate in the
/// spotter-log survey). It lives outside `.` and outside the session dir, so it collides with
/// neither the operator's grants nor the self-surface deny.
pub(super) fn mint_scratch(
    minted: &mut super::mint::Minted,
    loaded: &mut LoadedConfig,
) -> Result<std::path::PathBuf, Error> {
    let scratch = super::mint::mint_scratch_dir()?;
    // Minted by exclusive create under a random name, so — like the session dir — it
    // belongs to this attempt and a later failure may remove it.
    minted.scratch = Some(scratch.clone());
    eprintln!("scratch: {} (exported as $TMPDIR)", scratch.display());
    loaded.role.paths.insert(
        scratch.to_string_lossy().into_owned(),
        crate::config::Perms {
            read: true,
            write: true,
        },
    );
    Ok(scratch)
}
