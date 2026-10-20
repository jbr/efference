//! Filesystem grant resolution: which paths a call touches and whether they're granted.

use super::*;

/// The per-tool path rule — the acknowledged wart (see the module docs): which JSON input key holds
/// the target path, whether it is a read or a write, and the fallback when the key is absent (the
/// optional-path tools default to cwd `"."`; `None` means the path is required). Tools absent from
/// this map (`undo_edit`, `retarget_edit`, `consolidate`, the process-monitor/inbox tools) carry no path
/// and clear on the name grant alone. The string literals are centralized here so the coupling to
/// each built-in's schema lives in one place, shared by both clearance sources.
pub(crate) fn path_spec(name: &str) -> Option<(&'static str, Perm, Option<&'static str>)> {
    Some(match name {
        // Reads: the target must be under a read-or-write grant.
        "read" => ("paths", Perm::Read, None),
        "read_image" => ("path", Perm::Read, None),
        "list" => ("path", Perm::Read, Some(".")),
        "search" => ("paths", Perm::Read, Some(".")),
        "find_anchor" => ("file_path", Perm::Read, None),
        // Writes: the target must be under a write grant. `edit` is where the edit pipeline names
        // its file, so write is enforced here; `undo_edit` (no path) clears on the name grant.
        "write" => ("path", Perm::Write, None),
        "edit" => ("file_path", Perm::Write, None),
        _ => return None,
    })
}

/// Extract the path target(s) at `input[key]`: a single string or a non-empty array of
/// strings (every element must be a string). `default` supplies a fallback when the key is
/// absent (the optional-path tools default to cwd `"."`); `None` means the path is required.
/// Returns `None` for a missing required key, an empty array, or any non-string value —
/// a malformed call never clears.
pub(super) fn input_targets<'a>(
    input: &'a Value,
    key: &str,
    default: Option<&'a str>,
) -> Option<Vec<&'a str>> {
    match input.get(key) {
        Some(Value::String(s)) => Some(vec![s.as_str()]),
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return None;
            }
            items.iter().map(Value::as_str).collect()
        }
        Some(_) => None, // present but not a string/array → malformed
        None => default.map(|d| vec![d]),
    }
}

/// Whether `target` falls under some `[context]` entry path carrying `perm`
/// (docs/context.md). Read is always on for a listed path — a file exactly, a
/// location's whole subtree — by the same lexical rule as [`path_granted`]. A write needs the
/// covering entry's `update` (target exists) or `create` (target is new) grant; the existence
/// probe is the one filesystem touch in the classifier — it is what tells create from update,
/// and is harmless here because a verdict is derived state, never on the wire
/// (docs/log-and-projection.md). `exec` entries have no files and grant nothing.
pub(super) fn context_granted(role: &Config, target: &str, perm: Perm, dirs: &Directories) -> bool {
    // The guide overlay's virtual prefix (docs/context.md, [`crate::guide`]) is read-granted
    // unconditionally — the mount exists whether or not the config lists it, and the
    // documents are the recovery path for a session whose config knows nothing else. Never
    // write-granted: there is nothing on disk to write, and a harness granting itself write
    // on its own documents would be the self-authorization the config design forbids.
    if crate::guide::is_virtual(target) {
        return perm == Perm::Read;
    }
    // Grants match the judged (project-counterpart) target; the existence probe looks at
    // the real path the model named.
    let real = lexical_normalize(&absolutize(target, dirs.working()));
    let target = dirs.target(target);
    role.context.iter().any(|entry| {
        let crate::config::ContextSource::Files(paths) = &entry.source else {
            return false;
        };
        paths.iter().any(|p| target.starts_with(dirs.grant(p)))
            && match perm {
                Perm::Read => true,
                Perm::Write => {
                    (entry.grants.update && real.exists())
                        || (entry.grants.create && !real.exists())
                }
            }
    })
}

/// Whether `path` (a target, resolved by [`Directories::target`]) is granted `perm` by the
/// role's `[paths]` map. `write` implies `read`. Resolution is **longest-prefix-wins**: among the grants whose
/// subtree contains the target, the *most specific* (deepest) one governs — so a nested
/// grant carves an exclusion out of a broader parent (allow `.`, deny `.efference/`,
/// re-allow `.efference/<id>/config.toml` read-only). A no-access grant (`false` sugar,
/// `Perms::default()`) is a real deny once it's the governing prefix. The harness injects
/// its self-surface layers (docs/permissions.md) *after* the operator's config, so an
/// exact-key operator grant is overwritten and a safety deny wins at equal specificity;
/// an operator can still override by naming a *strictly deeper* path. Path matching is
/// lexical (`..`/`.` collapsed without touching the filesystem), so a symlink inside a
/// grant that points outside is treated as inside — acceptable under the error-not-deception
/// threat model (docs/permissions.md).
pub(super) fn path_granted(role: &Config, path: &str, perm: Perm, dirs: &Directories) -> bool {
    // A guide-overlay path names a document in the binary, not a location on disk
    // ([`crate::guide`]). It must never resolve through this table: joined onto the working directory it
    // would look like an ordinary relative path and a broad `[paths]` grant would then
    // appear to grant *write* on it. Its own read grant is in [`context_granted`].
    if crate::guide::is_virtual(path) {
        return false;
    }
    path_granted_at(role, &dirs.target(path), perm, dirs)
}

/// [`path_granted`] for a target that is **already** absolute, normalized, and judged
/// ([`Directories::judged`]) — the shape the bash redirection carve-out produces, where
/// the write target may have been resolved against a `cd`'s directory rather than the
/// working directory ([`commands::classify_redirect_write`]). `dirs` still resolves the
/// *grant* keys, against the project.
///
/// [`commands::classify_redirect_write`]: super::commands
pub(super) fn path_granted_at(
    role: &Config,
    target: &Path,
    perm: Perm,
    dirs: &Directories,
) -> bool {
    match governing_grant(role, target, dirs) {
        Some((_, perms)) => match perm {
            Perm::Read => perms.read || perms.write, // write implies read
            Perm::Write => perms.write,
        },
        None => false, // no grant covers the target at all
    }
}

/// The `[paths]` grant that decides an absolute, normalized `target` — the deepest one
/// whose subtree contains it (judged, per [`Directories::judged`]) — resolved to its
/// absolute form, with its perms. `None` when no grant covers the target.
pub(crate) fn governing_grant<'a>(
    role: &'a Config,
    target: &Path,
    dirs: &Directories,
) -> Option<(PathBuf, &'a Perms)> {
    role.paths
        .iter()
        .filter_map(|(grant_path, perms)| {
            let grant = dirs.grant(grant_path);
            target.starts_with(&grant).then_some((grant, perms))
        })
        .max_by_key(|(grant, _)| grant.components().count())
}

/// Make `path` absolute by joining it onto `cwd` if it is relative.
pub(crate) fn absolutize(path: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Lexically collapse `.` and `..` without touching the filesystem (so it works for
/// not-yet-existing write targets). `..` pops a trailing normal component but never
/// escapes past the root/prefix.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str())
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a real directory name; `..` at/above the root is a no-op.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
        }
    }
    out
}
