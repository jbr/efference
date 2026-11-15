//! Additive path grants for a submitted turn's filesystem references
//! (docs/permissions.md, docs/control-api.md).
//!
//! A reference is a request, not a grant: the submission gate validates the target's
//! form, and admission turns it into a [`ConfigChange::Path`] through the same
//! machinery a gate or an fs-watch commit uses. This module is the pure half — what
//! the policy in force does not already cover — so it can be tested without a session
//! or a file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::{Config, Perms};
use crate::record::log::{
    ConfigChange, ReferenceAccess, ReferenceTarget, UserContent, UserSegment,
};

use super::governing_grant;
use super::paths::context_granted;
use super::{Directories, Perm, apply_config_change};

/// One grant an admission needs: the `[paths]` key it is written at, the spelling the
/// submitter used (for a notice naming what was refused), and the change itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReferenceGrant {
    pub key: String,
    pub original: String,
    pub change: ConfigChange,
}

/// A reference admission could not turn into a key, with the reason a notice states:
/// the granted path has no UTF-8 spelling a TOML key can carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnusableReference {
    pub original: String,
    pub reason: String,
}

/// The path grants a document's filesystem references add to `role` (docs/permissions.md).
///
/// Requests for the same target **combine** — a read chip and a write chip on one
/// directory ask for read+write once — and the result is **additive**: a target already
/// covered for the requested access yields nothing, a grant never removes a permission
/// (an exact key that already carries write keeps it when a read reference passes), and
/// a grant is written at the target itself, never widened to a parent. The reference's
/// target is the stored absolute `path`, judged through [`Directories::judged`] — never
/// re-derived from `original`, which may name a working directory that has since moved
/// (docs/control-api.md).
///
/// Coverage is asked of both clearance sources the classifier uses, so a reference the
/// `[context]` listing or a broader `[paths]` grant already authorizes adds nothing.
/// A deny at equal or broader specificity is *not* coverage: the grant is written at the
/// target, which is deeper (or, at the session's own `.efference` root, equal and later
/// in the fold), so referencing a normally denied location is how a session widens its
/// perimeter for one investigation.
///
/// Each grant is computed against **the policy as the batch so far extends it**, not
/// against `role` alone, and the targets are visited in [`PathBuf`] order — which puts a
/// path before everything beneath it. Both properties are needed for the batch to stay
/// additive as a whole: a write grant on a directory admits a read reference to a file
/// inside it (so no narrower key is emitted at all), where computing both independently
/// against `role` would emit a read-only key at the file that folds *after* the directory
/// grant and silently takes its write away from that one file. Pre-existing deeper denies
/// keep working: the fold is longest-prefix, so a deny under a granted directory still
/// wins where it sits.
pub(crate) fn filesystem_grants(
    content: &UserContent,
    role: &Config,
    dirs: &Directories,
) -> (Vec<ReferenceGrant>, Vec<UnusableReference>) {
    let wanted = requested_access(content, dirs);
    // The fold the log will perform, performed here first: each emitted grant is applied to
    // this copy before the next target is judged.
    let mut work = role.clone();
    let mut grants = Vec::new();
    let mut unusable = Vec::new();
    for (target, (original, requested)) in wanted {
        if covered(&target, requested, &work, dirs) {
            continue;
        }
        let Some(key) = dirs.grant_key(&target) else {
            unusable.push(UnusableReference {
                original,
                reason: format!("{} is not valid UTF-8", target.display()),
            });
            continue;
        };
        let perms = union(existing_at(&target, &work, dirs), requested.into());
        let change = ConfigChange::Path {
            path: key.clone(),
            read: perms.read,
            write: perms.write,
        };
        apply_config_change(&mut work, &change);
        grants.push(ReferenceGrant {
            change,
            key,
            original,
        });
    }
    (grants, unusable)
}

/// Whether any occurrence in `content` names a filesystem target — the admission fast
/// path's question, asked before it takes the policy section.
pub(crate) fn has_filesystem_references(content: &UserContent) -> bool {
    content
        .references
        .values()
        .any(|definition| matches!(definition.target, ReferenceTarget::Filesystem { .. }))
}

/// Judged target → (first spelling seen, combined request), for every filesystem
/// occurrence. BTreeMap keeps the emission order deterministic, which the tests (and a
/// reader of the log) rely on.
fn requested_access(
    content: &UserContent,
    dirs: &Directories,
) -> BTreeMap<PathBuf, (String, ReferenceAccess)> {
    let mut wanted: BTreeMap<PathBuf, (String, ReferenceAccess)> = BTreeMap::new();
    for segment in &content.content {
        let UserSegment::Reference(id) = segment else {
            continue;
        };
        let Some(definition) = content.references.get(id) else {
            continue;
        };
        let ReferenceTarget::Filesystem {
            original,
            path,
            access,
            ..
        } = &definition.target
        else {
            continue;
        };
        // The stored path is authoritative; `original` is presentation.
        let target = dirs.judged(std::path::Path::new(path));
        // `write` alone normalizes to read+write; neither was refused at submission.
        let Some(access) = access.normalized() else {
            continue;
        };
        wanted
            .entry(target)
            .and_modify(|(_, requested)| {
                requested.read |= access.read;
                requested.write |= access.write;
            })
            .or_insert((original.clone(), access));
    }
    wanted
}

/// Whether the policy in force already authorizes `requested` on `target` — by a
/// `[paths]` grant (longest-prefix, so a deeper deny is not coverage) or a `[context]`
/// entry covering it.
fn covered(
    target: &std::path::Path,
    requested: ReferenceAccess,
    role: &Config,
    dirs: &Directories,
) -> bool {
    let read_ok = governing_grant(role, target, dirs)
        .is_some_and(|(_, perms)| perms.read || perms.write)
        || context_granted(role, &target.to_string_lossy(), Perm::Read, dirs);
    if !read_ok {
        return false;
    }
    !requested.write
        || governing_grant(role, target, dirs).is_some_and(|(_, perms)| perms.write)
        || context_granted(role, &target.to_string_lossy(), Perm::Write, dirs)
}

/// The permissions every grant key that resolves to `target` already carries. Keys can
/// be spelled more than one way (`.` and the project's absolute path both resolve to the
/// project root), so this asks by resolved path rather than by the key we would write.
fn existing_at(target: &std::path::Path, role: &Config, dirs: &Directories) -> Perms {
    role.paths
        .iter()
        .filter(|(other, _)| dirs.grant(other) == target)
        .fold(Perms::default(), |acc, (_, perms)| union(acc, *perms))
}

/// Additive union of two permission sets. `write` implies `read` throughout the policy,
/// so a write on either side implies read on the result.
fn union(left: Perms, right: Perms) -> Perms {
    let write = left.write || right.write;
    Perms {
        read: write || left.read || right.read,
        write,
    }
}

impl From<ReferenceAccess> for Perms {
    fn from(access: ReferenceAccess) -> Self {
        Self {
            read: access.read || access.write,
            write: access.write,
        }
    }
}
