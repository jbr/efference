//! Directory listings for `/files` (docs/web-bind.md) — the flat one the tree
//! expands a directory with, and the recursive one the path filter matches
//! names against.
//!
//! `GET /files/<dir>/` lists that directory. `?recursive=1` walks below it,
//! `&depth=N` bounds the walk, and both are capped here rather than trusted:
//! the request that would walk a monorepo whole is one URL away, and the answer
//! has to stay a single JSON body the client parses in one go. Past either cap
//! the listing reports `truncated` instead of erroring — a filter over most of
//! the tree is useful, and saying the set is partial is what keeps it honest.
//!
//! In a recursive listing an entry's `name` is its path **relative to the
//! directory listed**, which is what the finder matches on; `recursive` on the
//! listing says which spelling to expect. A flat listing's names stay bare.
//!
//! Symlinked directories are listed but never descended: the walk would not
//! terminate, and a link's target is one request away by its own path.

use super::ignores::Scope;
use crate::log::Timestamp;
use serde::Serialize;
use std::path::Path;

/// The deepest a recursive listing walks. A finder wants the project's path
/// names, not a vendored dependency tree seven levels down; the cap is where
/// "list everything" stops being a listing.
pub(crate) const MAX_DEPTH: usize = 8;

/// The most entries one listing reports. Past this the walk stops and says so.
pub(crate) const MAX_ENTRIES: usize = 10_000;

/// One entry of a directory listing. `kind` follows symlinks (a link to a
/// directory lists as a directory) while `symlink` records that the entry itself
/// is one, so a client can show the indirection without a second request.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Bare in a flat listing; relative to the listed directory in a recursive
    /// one.
    name: String,
    kind: Kind,
    /// Bytes on disk. Reported for directories too, where it is the directory's
    /// own size and means nothing to a reader — kept only because omitting it
    /// per-kind would make the shape conditional.
    size: u64,
    modified_at: Option<Timestamp>,
    symlink: bool,
}

#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    File,
    Dir,
    /// A device, socket, fifo — or a symlink whose target does not resolve.
    Other,
}

/// A directory's listing. `path` is the directory's own path relative to the
/// root, empty at the root itself, so a client can render a breadcrumb without
/// reconstructing it from the URL.
#[derive(Serialize, Debug)]
pub(crate) struct Listing {
    path: String,
    /// Whether entry names are paths (recursive) or bare names (flat).
    recursive: bool,
    /// A cap was hit: the set is a prefix of what is there, not the whole of it.
    truncated: bool,
    entries: Vec<Entry>,
}

/// What the query string asked for, clamped to what this route will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Options {
    pub(crate) recursive: bool,
    /// Levels below the listed directory, at least 1 (its own children).
    pub(crate) depth: usize,
}

impl Options {
    /// `?recursive=1&depth=N`. An unparseable or absent value is the default,
    /// never an error: this is a read surface a human pokes at from a URL bar.
    pub(crate) fn from_query(query: &str) -> Self {
        let mut recursive = false;
        let mut depth = None;
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "recursive" => recursive = !matches!(value, "0" | "false"),
                "depth" => depth = value.parse::<usize>().ok(),
                _ => {}
            }
        }
        let depth = depth.unwrap_or(if recursive { MAX_DEPTH } else { 1 });
        Self {
            recursive,
            depth: depth.clamp(1, MAX_DEPTH),
        }
    }
}

pub(crate) fn modified_at(meta: &std::fs::Metadata) -> Option<Timestamp> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|since| Timestamp(since.as_millis() as u64))
}

/// Walk `dir` under `scope`, to the depth `options` allows.
pub(crate) async fn listing(
    dir: &Path,
    root: &Path,
    scope: Scope,
    options: Options,
) -> Option<Listing> {
    let depth = if options.recursive { options.depth } else { 1 };
    let mut queue = vec![(dir.to_path_buf(), scope, 0usize, String::new())];
    let mut entries = Vec::new();
    let mut truncated = false;

    // The first pop is the requested directory itself, at level 0.
    while let Some((at, scope, level, prefix)) = queue.pop() {
        let Ok(mut read_dir) = tokio::fs::read_dir(&at).await else {
            // The directory the request named must be listable; one found
            // during the walk may have been removed or be unreadable, which is
            // a hole in the listing rather than a failed request.
            if level == 0 {
                return None;
            }
            continue;
        };
        while let Ok(Some(item)) = read_dir.next_entry().await {
            let name = item.file_name().to_string_lossy().into_owned();
            let path = item.path();
            // `metadata` follows the link; `file_type` on the DirEntry does not,
            // which is how an entry can be both a symlink and a dir.
            let symlink = item.file_type().await.is_ok_and(|t| t.is_symlink());
            let (kind, size, modified) = match tokio::fs::metadata(&path).await {
                Ok(meta) if meta.is_dir() => (Kind::Dir, meta.len(), modified_at(&meta)),
                Ok(meta) if meta.is_file() => (Kind::File, meta.len(), modified_at(&meta)),
                Ok(meta) => (Kind::Other, meta.len(), modified_at(&meta)),
                // A dangling symlink: listed, since hiding it would misreport
                // the directory, but nothing can be said about its target.
                Err(_) => (Kind::Other, 0, None),
            };
            if scope.is_ignored(&path, kind == Kind::Dir) {
                continue;
            }
            let rel = format!("{prefix}{name}");
            if kind == Kind::Dir && !symlink && level + 1 < depth {
                let child = scope.descend(&path);
                queue.push((path, child, level + 1, format!("{rel}/")));
            }
            entries.push(Entry {
                name: rel,
                kind,
                size,
                modified_at: modified,
                symlink,
            });
            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                queue.clear();
                break;
            }
        }
        if truncated {
            break;
        }
    }

    // Sorted so a listing is stable across reads and two clients agree on
    // order; in a recursive listing that also groups each directory with its
    // own contents, since the names are paths.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Some(Listing {
        path: dir
            .strip_prefix(root)
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .into_owned(),
        recursive: options.recursive,
        truncated,
        entries,
    })
}
