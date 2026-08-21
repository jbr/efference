//! `/files/**` — the file viewer's read surface (docs/web-bind.md).
//!
//! Read-only, and human-facing: one GET reads a text file, one lists a directory.
//! This is the behaviour of running a static file server beside the daemon, moved
//! inside it, so the operator can read project files through the same page the
//! session is running in.
//!
//! Three things about it are load-bearing rather than incidental:
//!
//! - **Outside `/v1`, and cookie-only.** `/v1` is the control API, where every
//!   client is equal and a bearer holder is one of them. A bearer holder reading
//!   arbitrary working-directory files would bypass `[paths]` — including the
//!   injected deny over the session directory, which is what stops a session
//!   reaching another session's log (docs/permissions.md). The gate enforces the
//!   cookie requirement; this module is mounted only on the TCP spawn, so the UDS
//!   never serves it either.
//! - **Never the file's own content-type.** Every body here is
//!   `text/plain; charset=utf-8`. Serving a repository's `.html` as `text/html`
//!   would be same-origin script execution on a page holding a session credential
//!   for a channel that runs commands. `nosniff` is already set on every response
//!   by the bind's `SecurityHeaders`, so the two halves of that are covered
//!   together.
//! - **Refuse rather than degrade.** Non-UTF-8 and oversized files are errors, not
//!   partial renders: there is no useful browser rendering of either, and refusing
//!   them keeps range requests, truncation states, and windowed fetching out of
//!   both sides of the design.
//!
//! The refusal is why a body is read whole rather than streamed from the file.
//! Streaming commits to `200` with its first byte, after which "not text" can only
//! arrive as mojibake — a browser decodes invalid bytes to replacement characters
//! and never fails — so validating before the status is what makes the refusal a
//! status code at all. [`MAX_BYTES`] is what keeps that affordable: validation runs
//! at several GB/s, so the whole ceiling costs tens of microseconds against a
//! network round trip, and the poll's common answer carries no body at all. If the
//! ceiling ever lifts, streaming and prefix-only validation arrive together with
//! the range requests that lifting it would require.
//!
//! The root is the process working directory — session-stable, and the same root
//! the built-in tools resolve relative paths against (`src/policy.rs`). Path
//! resolution canonicalizes and then **re-checks containment**, because the
//! escape worth closing is a symlink inside the root pointing out of it, not `..`.
//!
//! Within the root nothing is withheld for being sensitive — dotfiles included, so a
//! cookie holder can read `.env`. That posture, and why a dotfile deny was weighed
//! and not built, is in docs/web-bind.md; the short of it is that concealing a path
//! from the listing while still serving it by name would imply a protection that is
//! not there, and the same cookie already reaches `/v1`, where the agent can be made
//! to read any file into the transcript.

use super::*;
use crate::log::Timestamp;
use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use trillium::{KnownHeaderName, Method};

/// The largest file served. Chosen against real source files rather than a round
/// number: a 5k-line single-file C implementation runs about 200 kB, and this
/// project's own convention keeps files near 500 lines (~20 kB). Anything past
/// this ceiling wants the client fetching only the line ranges on screen, which is
/// a different design — so the ceiling is where that argument starts, not a limit
/// the viewer is expected to meet (jbr, 2026-08-20).
pub(crate) const MAX_BYTES: u64 = 500 * 1024;

/// Names a listing never reports. The read route does **not** consult this: anything
/// you can name, you can read; the listing only keeps the tree from drowning in
/// build output. Whether this becomes gitignore-derived or operator-toggleable is
/// the navigation unit's question (backlog/web/file-view/navigation.md).
const IGNORED: [&str; 3] = [".git", "target", "node_modules"];

/// The viewer's read surface, rooted at one canonical directory. A root that could
/// not be canonicalized at boot serves nothing rather than falling back to a wider
/// one.
#[derive(Debug, Clone)]
pub(crate) struct Files {
    root: Option<Arc<PathBuf>>,
}

impl Files {
    /// Rooted at the process working directory, as the tools are.
    pub(crate) fn at_cwd() -> Self {
        match std::env::current_dir() {
            Ok(cwd) => Self::rooted_at(cwd),
            Err(_) => Self { root: None },
        }
    }

    /// Rooted at an explicit directory — the injection point for tests, which
    /// serve a scratch directory rather than whatever the test process's cwd is.
    pub(crate) fn rooted_at(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().canonicalize().ok().map(Arc::new),
        }
    }

    /// The filesystem path a URL path names, or `None` if it names nothing servable.
    ///
    /// `..` is popped lexically and cannot climb past the root, which handles the
    /// obvious traversal. The check that matters is the one *after* `canonicalize`:
    /// a symlink inside the root resolves to wherever it points, and only comparing
    /// the resolved path against the root catches one that points outside. (This is
    /// the gap in `trillium-static`'s `resolve_fs_path`, which checks containment
    /// before canonicalizing and not after — correct enough for a public asset
    /// directory, wrong here.)
    fn resolve(&self, url_path: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let mut path = PathBuf::clone(root);
        for raw in url_path.split('/') {
            if raw.is_empty() {
                continue;
            }
            let segment = percent_decode_str(raw).decode_utf8().ok()?;
            match segment.as_ref() {
                "." => {}
                ".." => {
                    if path.as_path() == root.as_path() {
                        return None;
                    }
                    path.pop();
                }
                // A decoded separator or NUL would re-enter path parsing after the
                // split that was supposed to bound it.
                s if s.contains('/') || s.contains('\0') => return None,
                s => path.push(s),
            }
        }
        let resolved = path.canonicalize().ok()?;
        resolved.starts_with(root.as_path()).then_some(resolved)
    }
}

/// One entry of a directory listing. `kind` follows symlinks (a link to a directory
/// lists as a directory) while `symlink` records that the entry itself is one, so a
/// client can show the indirection without a second request.
#[derive(Serialize, Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    kind: Kind,
    /// Bytes on disk. Reported for directories too, where it is the directory's own
    /// size and means nothing to a reader — kept only because omitting it per-kind
    /// would make the shape conditional.
    size: u64,
    modified_at: Option<Timestamp>,
    symlink: bool,
}

#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Kind {
    File,
    Dir,
    /// A device, socket, fifo — or a symlink whose target does not resolve.
    Other,
}

/// A directory's listing. `path` is the directory's own path relative to the root,
/// empty at the root itself, so a client can render a breadcrumb without
/// reconstructing it from the URL.
#[derive(Serialize, Debug)]
struct Listing {
    path: String,
    entries: Vec<Entry>,
}

fn modified_at(meta: &std::fs::Metadata) -> Option<Timestamp> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|since| Timestamp(since.as_millis() as u64))
}

/// The validator for a file's bytes: length and mtime, which together change on
/// every edit that changes content. Not a hash — hashing every poll to save the
/// occasional false revalidation is the wrong trade for a local file.
fn etag(meta: &std::fs::Metadata) -> String {
    let modified = modified_at(meta).map_or(0, |Timestamp(ms)| ms);
    format!("\"{:x}-{:x}\"", meta.len(), modified)
}

impl trillium::Handler for Files {
    async fn run(&self, conn: Conn) -> Conn {
        // Claim `/files` and `/files/...`, but not `/filesomething`.
        let Some(rest) = conn.path().strip_prefix("/files") else {
            return conn;
        };
        if !(rest.is_empty() || rest.starts_with('/')) {
            return conn;
        }
        let rest = rest.to_owned();
        if conn.method() != Method::Get {
            return fail(
                conn,
                ErrorCode::BadRequest,
                "/files is read-only; GET a path or a directory",
            );
        }
        if self.root.is_none() {
            return fail(
                conn,
                ErrorCode::Internal,
                "no working directory to serve files from",
            );
        }
        let Some(path) = self.resolve(&rest) else {
            return fail(
                conn,
                ErrorCode::NotFound,
                "no such path under the working directory",
            );
        };
        let Ok(meta) = tokio::fs::metadata(&path).await else {
            return fail(conn, ErrorCode::NotFound, "no such path");
        };
        if meta.is_dir() {
            // A listing's relative links only compose under a trailing slash, so the
            // slashless spelling is a redirect rather than a second served shape.
            if !rest.ends_with('/') {
                let location = format!("{}/", conn.path());
                return conn
                    .with_status(301)
                    .with_response_header(KnownHeaderName::Location, location)
                    .halt();
            }
            return list(conn, &path, self.root.as_ref().expect("root checked")).await;
        }
        read(conn, &path, &meta).await
    }
}

async fn list(conn: Conn, dir: &Path, root: &Path) -> Conn {
    let Ok(mut read_dir) = tokio::fs::read_dir(dir).await else {
        return fail(conn, ErrorCode::NotFound, "cannot list this directory");
    };
    let mut entries = Vec::new();
    while let Ok(Some(item)) = read_dir.next_entry().await {
        let name = item.file_name().to_string_lossy().into_owned();
        if IGNORED.contains(&name.as_str()) {
            continue;
        }
        // `metadata` follows the link; `file_type` on the DirEntry does not, which is
        // how an entry can be both a symlink and a dir.
        let symlink = item.file_type().await.is_ok_and(|t| t.is_symlink());
        let (kind, size, modified) = match tokio::fs::metadata(item.path()).await {
            Ok(meta) if meta.is_dir() => (Kind::Dir, meta.len(), modified_at(&meta)),
            Ok(meta) if meta.is_file() => (Kind::File, meta.len(), modified_at(&meta)),
            Ok(meta) => (Kind::Other, meta.len(), modified_at(&meta)),
            // A dangling symlink: listed, since hiding it would misreport the
            // directory, but nothing can be said about its target.
            Err(_) => (Kind::Other, 0, None),
        };
        entries.push(Entry {
            name,
            kind,
            size,
            modified_at: modified,
            symlink,
        });
    }
    // Sorted so a listing is stable across reads and two clients agree on order.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let listing = Listing {
        path: dir
            .strip_prefix(root)
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .into_owned(),
        entries,
    };
    conn.with_status(200)
        .with_response_header(KnownHeaderName::ContentType, "application/json")
        .with_body(serde_json::to_string(&listing).unwrap_or_default())
        .halt()
}

async fn read(conn: Conn, path: &Path, meta: &std::fs::Metadata) -> Conn {
    if meta.len() > MAX_BYTES {
        return fail(
            conn,
            ErrorCode::TooLarge,
            format!(
                "{} bytes exceeds the {MAX_BYTES}-byte view limit",
                meta.len()
            ),
        );
    }
    let validator = etag(meta);
    if conn
        .request_headers()
        .get_str(KnownHeaderName::IfNoneMatch)
        .is_some_and(|given| given.split(',').any(|tag| tag.trim() == validator))
    {
        // The viewer polls this route to notice edits, so the unchanged case is the
        // common one and answers without a body.
        return conn
            .with_status(304)
            .with_response_header(KnownHeaderName::Etag, validator)
            .halt();
    }
    let Ok(bytes) = tokio::fs::read(path).await else {
        return fail(conn, ErrorCode::NotFound, "cannot read this file");
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return fail(
            conn,
            ErrorCode::UnsupportedContent,
            "not valid UTF-8; this surface serves text only",
        );
    };
    conn.with_status(200)
        // Never the file's own type — see the module docs.
        .with_response_header(KnownHeaderName::ContentType, "text/plain; charset=utf-8")
        .with_response_header(KnownHeaderName::Etag, validator)
        .with_body(text)
        .halt()
}

#[cfg(test)]
mod test;
