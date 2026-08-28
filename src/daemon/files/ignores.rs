//! The discovery ignore set (docs/web-bind.md): gitignore-derived, and one set
//! governing every surface that *discovers* paths — the flat listing, the
//! recursive listing behind the file finder, and the directory-scope change
//! streams the tree holds.
//!
//! Two properties are load-bearing:
//!
//! - **Discovery only.** The read route and the single-file change stream never
//!   consult this: naming a path is the permission (docs/web-bind.md), so an
//!   ignored file stays readable, watchable and followable — it is only absent
//!   from listings. That is what keeps a `.git/info/exclude`-ed directory of
//!   planning documents viewable through an `open ⤢` while the tree stays quiet.
//! - **Repository-aware, deliberately.** The set is whatever git would ignore
//!   here, so a project states its own answer in the file it already keeps for
//!   the purpose, rather than the daemon carrying a hardcoded `target` /
//!   `node_modules` list that is wrong for every project that is not this one.
//!   The accepted consequence: outside a repository — or for a file git cannot
//!   see — discovery does not show it. The re-include knob is an efference.toml
//!   setting when that bites, not client-side state.
//!
//! `.git` itself is ignored unconditionally, as git treats it: it appears in no
//! `.gitignore` and no listing wants it.
//!
//! Precedence follows git's: the deepest `.gitignore` that matches decides, a
//! whitelist (`!pattern`) at that depth wins, and the user's global ignore file
//! sits lowest. A [`Scope`] is the matcher chain from the root down to one
//! directory, built per request rather than cached, so an edited `.gitignore`
//! takes effect on the next listing with no invalidation machinery.

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The root's ignore configuration: the user's global ignore file, resolved
/// once, plus the root the chains are built from.
#[derive(Debug)]
pub(crate) struct Ignores {
    root: PathBuf,
    global: Arc<Gitignore>,
}

impl Ignores {
    pub(crate) fn at(root: impl Into<PathBuf>) -> Self {
        let (global, _) = Gitignore::global();
        Self {
            root: root.into(),
            global: Arc::new(global),
        }
    }

    /// The matcher chain governing one directory: every `.gitignore` from the
    /// root down to it, shallowest first, plus the root's `.git/info/exclude`.
    /// A `dir` outside the root yields the root's own chain — callers resolve
    /// and containment-check before they get here.
    pub(crate) fn scope(&self, dir: &Path) -> Scope {
        let mut chain = vec![Arc::new(matcher_for(&self.root, true))];
        if let Ok(rest) = dir.strip_prefix(&self.root) {
            let mut at = self.root.clone();
            for segment in rest.components() {
                at.push(segment);
                chain.push(Arc::new(matcher_for(&at, false)));
            }
        }
        Scope {
            global: Arc::clone(&self.global),
            chain,
        }
    }
}

/// One directory's `.gitignore` — plus, at the root, the repository-local
/// `.git/info/exclude`, which is where a path gets hidden without touching a
/// tracked file.
fn matcher_for(dir: &Path, is_root: bool) -> Gitignore {
    let mut builder = GitignoreBuilder::new(dir);
    let _ = builder.add(dir.join(".gitignore"));
    if is_root {
        let _ = builder.add(dir.join(".git/info/exclude"));
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// The chain in effect for one directory. [`Scope::descend`] extends it for a
/// child directory, which is how the recursive listing carries the set down
/// without rebuilding it from the root at every level.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    global: Arc<Gitignore>,
    /// Root-first; the last matching entry decides.
    chain: Vec<Arc<Gitignore>>,
}

impl Scope {
    /// Whether discovery hides this path. `is_dir` matters: `target/` matches a
    /// directory and not a file of the same name.
    pub(crate) fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        if path.file_name().is_some_and(|name| name == ".git") {
            return true;
        }
        for matcher in self.chain.iter().rev() {
            match matcher.matched(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        self.global.matched(path, is_dir).is_ignore()
    }

    /// The scope inside `dir`, a child of this scope's directory.
    pub(crate) fn descend(&self, dir: &Path) -> Scope {
        let mut chain = self.chain.clone();
        chain.push(Arc::new(matcher_for(dir, false)));
        Scope {
            global: Arc::clone(&self.global),
            chain,
        }
    }
}
