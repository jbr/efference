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
use crate::fswatch::{FsChange, FsWatch, WatchGuard};
use crate::log::Timestamp;
use ignores::{Ignores, Scope};
use list::{Options, listing, modified_at};
use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use trillium::{Handler, KnownHeaderName, Method};
use trillium_sse::{Sse, SseHandler, sse};

pub(crate) mod ignores;
mod list;

/// The largest file served. Chosen against real source files rather than a round
/// number: a 5k-line single-file C implementation runs about 200 kB, and this
/// project's own convention keeps files near 500 lines (~20 kB). Anything past
/// this ceiling wants the client fetching only the line ranges on screen, which is
/// a different design — so the ceiling is where that argument starts, not a limit
/// the viewer is expected to meet.
pub(crate) const MAX_BYTES: u64 = 500 * 1024;

/// The viewer's read surface, rooted at one canonical directory. A root that could
/// not be canonicalized at boot serves nothing rather than falling back to a wider
/// one.
#[derive(Debug, Clone)]
pub(crate) struct Files {
    root: Option<Arc<PathBuf>>,
    /// The core watcher behind the change streams (`crate::fswatch`). `None` if
    /// the watcher could not start, in which case streams are refused while
    /// reads keep working.
    watch: Option<Arc<FsWatch>>,
    /// The discovery ignore set (`ignores`): listings and directory streams
    /// consult it, the read route and single-file streams never do.
    ignores: Option<Arc<Ignores>>,
}

impl Files {
    /// Rooted at the process working directory, as the tools are.
    pub(crate) fn at_cwd() -> Self {
        match std::env::current_dir() {
            Ok(cwd) => Self::rooted_at(cwd),
            Err(_) => Self {
                root: None,
                watch: None,
                ignores: None,
            },
        }
    }

    /// Rooted at an explicit directory — the injection point for tests, which
    /// serve a scratch directory rather than whatever the test process's cwd is.
    pub(crate) fn rooted_at(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().canonicalize().ok().map(Arc::new);
        Self {
            ignores: root.as_ref().map(|root| Arc::new(Ignores::at(&***root))),
            root,
            watch: FsWatch::new().ok(),
        }
    }

    /// The ignore chain governing one directory. Discovery surfaces only —
    /// see `ignores`.
    fn scope(&self, dir: &Path) -> Scope {
        self.ignores.as_ref().expect("root checked").scope(dir)
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

    /// The one refusal every path check shares: a path that does not resolve
    /// under the root, whichever check found it out.
    fn not_found() -> (ErrorCode, String) {
        (
            ErrorCode::NotFound,
            "no such path under the working directory".into(),
        )
    }

    /// `GET` with an Accept that names `text/event-stream`: change pokes for one
    /// file, over the core watcher (`crate::fswatch`; docs/web-bind.md). Each
    /// event is the file's root-relative path and nothing else — the client
    /// answers with the conditional GET it would otherwise poll with, so the
    /// stream replaces only the timer.
    ///
    /// The *parent directory* must resolve; the file itself need not exist. The
    /// watch is directory-level (editors save via rename-replace, and notify
    /// cannot watch a missing path), which is also what lets a held stream
    /// report a deleted file's restoration, and a stream opened on a refusal
    /// screen report the file's arrival.
    ///
    /// A URL naming a **directory** streams that directory's own entries
    /// instead: the tree holds one per expanded directory, so the annotations
    /// on it update themselves (docs/web-bind.md). Non-recursive, like the
    /// file case — a tree that has not expanded a directory has no use for its
    /// contents' churn — and filtered by the discovery ignore set, so a build
    /// directory cannot flood a stream nobody can see the results of.
    ///
    /// Every refusal is an `Err` here so the caller answers with `fail` in one
    /// place; the `Ok` is the watch and subscription the `Sse` stage turns into
    /// a stream.
    async fn prepare_stream(&self, rest: &str) -> Result<StreamTarget, (ErrorCode, String)> {
        let watch = self.watch.as_ref().ok_or_else(|| {
            (
                ErrorCode::Internal,
                "the change watcher is not running".into(),
            )
        })?;
        let root = self.root.as_ref().expect("root checked").clone();
        let (parent, name) = rest
            .rsplit_once('/')
            .ok_or_else(|| (ErrorCode::BadRequest, "stream a file path".into()))?;
        if name.is_empty() {
            let dir = self.resolve(parent).ok_or_else(Self::not_found)?;
            return self.prepare_dir_stream(watch, root, dir).await;
        }
        let name = percent_decode_str(name)
            .decode_utf8()
            .map_err(|_| Self::not_found())?;
        if name == "." || name == ".." || name.contains('/') || name.contains('\0') {
            return Err(Self::not_found());
        }
        let dir = self.resolve(parent).ok_or_else(Self::not_found)?;
        if !tokio::fs::metadata(&dir).await.is_ok_and(|m| m.is_dir()) {
            return Err(Self::not_found());
        }
        let mut file = dir.join(name.as_ref());
        if tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_dir()) {
            // The slashless directory spelling. The read route redirects it;
            // a stream has no redirect an `EventSource` would follow usefully,
            // so it is served directly.
            let resolved = self.resolve(rest).ok_or_else(Self::not_found)?;
            return self.prepare_dir_stream(watch, root, resolved).await;
        }
        // The poke filter compares byte-wise against notify's reported (disk)
        // spelling, so when the path resolves, adopt its canonical form *and
        // watch the canonical parent*: a wrong-case URL spelling on APFS, and a
        // symlink resolving into a nested directory of the root, each otherwise
        // open a stream that never fires — the events either carry the disk
        // spelling or land under a directory the given parent's non-recursive
        // watch cannot see. Containment is
        // re-checked as the read route's `resolve` does it: a link out of the
        // root is refused, never served-but-deaf. A not-yet-existing file keeps
        // the given spelling and parent — its eventual creator is usually the
        // process the URL's spelling came from. One inherent loss either way:
        // after adoption, a rename over the *link itself* emits under the
        // link's spelling and is missed — either spelling loses one of the two
        // edit paths, and the adopted one follows the read route.
        let watched_dir = match tokio::fs::canonicalize(&file).await {
            Ok(canonical) => {
                if !canonical.starts_with(root.as_path()) {
                    return Err(Self::not_found());
                }
                let parent = canonical.parent().unwrap_or(&dir).to_path_buf();
                file = canonical;
                parent
            }
            Err(_) => dir,
        };
        let rel = rel_path(&root, &file);
        let guard = watch
            .watch_dir(&watched_dir)
            .map_err(|e| (ErrorCode::Internal, format!("cannot watch: {e}")))?;
        // Subscribe after registering the watch. Neither order is airtight on
        // its own — a change between registration and subscription is seen by
        // notify but dropped by the broadcast (no receiver yet); the reverse
        // order loses the change entirely — which is why the client's side of
        // the contract matters: the viewer conditional-GETs on stream *open*
        // (docs/web-client.md), so anything that landed before the
        // subscription is picked up by that fetch, and anything after it
        // pokes.
        let rx = watch.subscribe();
        Ok(StreamTarget::File {
            rx,
            guard,
            file,
            rel,
        })
    }

    /// A directory-scope stream: one poke per changed entry *of this
    /// directory*, ignored entries filtered out. The path is already resolved
    /// and contained; canonicalizing again is what makes the byte-wise event
    /// comparison work, for the same reason the file case adopts the canonical
    /// spelling.
    async fn prepare_dir_stream(
        &self,
        watch: &Arc<FsWatch>,
        root: Arc<PathBuf>,
        dir: PathBuf,
    ) -> Result<StreamTarget, (ErrorCode, String)> {
        if !tokio::fs::metadata(&dir).await.is_ok_and(|m| m.is_dir()) {
            return Err(Self::not_found());
        }
        let scope = self.scope(&dir);
        let guard = watch
            .watch_dir(&dir)
            .map_err(|e| (ErrorCode::Internal, format!("cannot watch: {e}")))?;
        // Subscribe after registering, as the file case does, and for the same
        // reason: the consumer answers stream *open* with the listing it would
        // otherwise poll for, so nothing that landed during setup is lost.
        let rx = watch.subscribe();
        Ok(StreamTarget::Dir {
            rx,
            guard,
            dir,
            root,
            scope,
        })
    }
}

/// A path as the wire spells it: root-relative, with CR/LF percent-encoded.
/// SSE field values cannot carry raw line terminators: trillium-sse renders LF
/// as a legal multi-line data field, but a lone CR in a file name (legal on
/// disk) would reach the wire intact and browsers split fields on it — a
/// workspace file name forging pokes for arbitrary paths.
fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// The idle-comment interval, as `/v1/events` uses: proxies and keepalive
/// middleboxes see traffic on a quiet stream.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// One change poke: `event: change`, data = the file's root-relative path.
struct ChangeEvent {
    data: String,
}

impl Eventable for ChangeEvent {
    fn data(&self) -> Option<&str> {
        Some(&self.data)
    }
    fn event_type(&self) -> Option<&str> {
        Some("change")
    }
}

/// The per-connection poke stream: the broadcast filtered to one file, owning
/// the [`WatchGuard`] so the watch lives exactly as long as the connection. A
/// lagged receiver pokes rather than skips — the client's answer to any poke
/// is a conditional GET, so a spurious one costs a 304 and a missed one would
/// cost correctness.
fn changes(
    rx: tokio::sync::broadcast::Receiver<FsChange>,
    guard: WatchGuard,
    file: PathBuf,
    rel: String,
) -> impl Stream<Item = ChangeEvent> + Send + Sync + 'static {
    futures_lite::stream::unfold(
        (rx, guard, file, rel),
        |(mut rx, guard, file, rel)| async move {
            loop {
                match rx.recv().await {
                    Ok(FsChange { path }) if path == file => {
                        let event = ChangeEvent { data: rel.clone() };
                        return Some((event, (rx, guard, file, rel)));
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => {
                        let event = ChangeEvent { data: rel.clone() };
                        return Some((event, (rx, guard, file, rel)));
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        },
    )
}

/// The per-connection poke stream for a directory: the broadcast filtered to
/// this directory's own entries, minus the ones discovery hides. `is_dir` at
/// poke time is best-effort — a deleted entry cannot be stat'd — which only
/// affects whether a directory-only pattern (`target/`) suppresses the poke
/// for a path that no longer exists; the cost of getting it wrong is one
/// spurious poke.
fn dir_changes(
    rx: tokio::sync::broadcast::Receiver<FsChange>,
    guard: WatchGuard,
    dir: PathBuf,
    root: Arc<PathBuf>,
    scope: Scope,
) -> impl Stream<Item = ChangeEvent> + Send + Sync + 'static {
    futures_lite::stream::unfold(
        (rx, guard, dir, root, scope),
        |(mut rx, guard, dir, root, scope)| async move {
            loop {
                let data = match rx.recv().await {
                    Ok(FsChange { path }) => {
                        if path.parent() != Some(dir.as_path()) {
                            continue;
                        }
                        if scope.is_ignored(&path, path.is_dir()) {
                            continue;
                        }
                        rel_path(&root, &path)
                    }
                    // A lagged receiver pokes with the directory itself: the
                    // consumer's answer is to re-list, which is what it would
                    // do for any entry of it.
                    Err(RecvError::Lagged(_)) => rel_path(&root, &dir),
                    Err(RecvError::Closed) => return None,
                };
                return Some((ChangeEvent { data }, (rx, guard, dir, root, scope)));
            }
        },
    )
}

/// The validator for a file's bytes: length and mtime, which together change on
/// every edit that changes content. Not a hash — hashing every poll to save the
/// occasional false revalidation is the wrong trade for a local file.
fn etag(meta: &std::fs::Metadata) -> String {
    let modified = modified_at(meta).map_or(0, |Timestamp(ms)| ms);
    format!("\"{:x}-{:x}\"", meta.len(), modified)
}

/// A prepared stream: the watch and subscription for one file or directory,
/// handed from the validating stage to the `Sse` stage through conn state.
enum StreamTarget {
    File {
        rx: tokio::sync::broadcast::Receiver<FsChange>,
        guard: WatchGuard,
        file: PathBuf,
        rel: String,
    },
    Dir {
        rx: tokio::sync::broadcast::Receiver<FsChange>,
        guard: WatchGuard,
        dir: PathBuf,
        root: Arc<PathBuf>,
        scope: Scope,
    },
}

/// The request path with `/files` stripped — empty or `/`-led — set by the
/// validating stage once it has claimed the request, so the later stages need
/// no prefix check of their own: absent, the request was never ours.
struct FilesPath(String);

type ChangeStream = Pin<Box<dyn Stream<Item = ChangeEvent> + Send + Sync + 'static>>;

/// The `Sse` stage's event source: the prepared target becomes the poke stream.
struct Connect;

impl SseHandler for Connect {
    type Event = ChangeEvent;
    type EventStream = ChangeStream;

    async fn connect(&self, conn: &mut Conn) -> ChangeStream {
        match conn.take_state::<StreamTarget>() {
            Some(StreamTarget::File {
                rx,
                guard,
                file,
                rel,
            }) => Box::pin(changes(rx, guard, file, rel)),
            Some(StreamTarget::Dir {
                rx,
                guard,
                dir,
                root,
                scope,
            }) => Box::pin(dir_changes(rx, guard, dir, root, scope)),
            // Unreachable: the stage below only runs with a target in state.
            None => Box::pin(futures_lite::stream::empty()),
        }
    }
}

/// The `Sse` handler, run only when the validating stage prepared a stream.
/// `Sse` negotiates on `Accept` alone, and this handler sits in a stack that
/// serves every path the router did not claim — unguarded, any such request
/// naming `text/event-stream` would be answered with an empty stream instead
/// of falling through. Everything but `run` (init's runtime capture for the
/// heartbeat, and the post-header upgrade that drives the stream) is
/// delegated whole.
#[derive(trillium_macros::Handler)]
struct StreamStage {
    #[handler(except = run)]
    sse: Sse<Connect>,
}

impl StreamStage {
    async fn run(&self, conn: Conn) -> Conn {
        if conn.state::<StreamTarget>().is_none() {
            return conn;
        }
        self.sse.run(conn).await
    }
}

impl Files {
    /// The `/files` surface as a handler: three stages in a tuple.
    ///
    /// 1. **Validate** — claims `/files` and `/files/...` (never `/filesomething`),
    ///    refuses non-GET and a missing root, and for a request negotiating a
    ///    stream prepares the watch, refusing here so the `Sse` stage never sees
    ///    a request it cannot serve.
    /// 2. **Stream** — the `Sse` handler over that prepared target.
    /// 3. **Read** — the body: a file, a listing, or the directory redirect.
    ///
    /// Trillium's `Sse` drives the stream through the handler's upgrade hook,
    /// so it is mounted as a handler of its own rather than called from inside
    /// one — delegating `run` alone would leave the stream undriven.
    pub(crate) fn handler(self) -> impl trillium::Handler {
        let validate = self.clone();
        let reader = self;
        (
            move |conn: Conn| {
                let files = validate.clone();
                async move {
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
                    if files.root.is_none() {
                        return fail(
                            conn,
                            ErrorCode::Internal,
                            "no working directory to serve files from",
                        );
                    }
                    // Content negotiation (docs/web-bind.md): a request whose Accept
                    // *explicitly* names `text/event-stream` gets a stream of change pokes
                    // instead of the body. Explicit-only — `*/*` (a fetch's default) gets
                    // the file, so only a client that asked to stream ever streams. The
                    // `Sse` stage applies the same rule to the same header.
                    let streams = conn
                        .request_headers()
                        .get_str(KnownHeaderName::Accept)
                        .is_some_and(|accept| accept.contains("text/event-stream"));
                    if !streams {
                        return conn.with_state(FilesPath(rest));
                    }
                    match files.prepare_stream(&rest).await {
                        Ok(target) => conn.with_state(FilesPath(rest)).with_state(target),
                        Err((code, message)) => fail(conn, code, message),
                    }
                }
            },
            StreamStage {
                sse: sse(Connect).with_heartbeat(HEARTBEAT),
            },
            move |mut conn: Conn| {
                let files = reader.clone();
                async move {
                    let Some(FilesPath(rest)) = conn.take_state::<FilesPath>() else {
                        return conn;
                    };
                    let Some(path) = files.resolve(&rest) else {
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
                            // The query carries the listing's shape (`?recursive=1`), so
                            // the redirect must keep it or the client's options vanish on
                            // a spelling correction.
                            let query = conn.querystring();
                            let location = match query.is_empty() {
                                true => format!("{}/", conn.path()),
                                false => format!("{}/?{query}", conn.path()),
                            };
                            return conn
                                .with_status(301)
                                .with_response_header(KnownHeaderName::Location, location)
                                .halt();
                        }
                        let options = Options::from_query(conn.querystring());
                        let scope = files.scope(&path);
                        let root = files.root.as_ref().expect("root checked").clone();
                        let Some(listing) = listing(&path, &root, scope, options).await else {
                            return fail(conn, ErrorCode::NotFound, "cannot list this directory");
                        };
                        return conn
                            .with_status(200)
                            .with_response_header(KnownHeaderName::ContentType, "application/json")
                            .with_body(serde_json::to_string(&listing).unwrap_or_default())
                            .halt();
                    }
                    read(conn, &path, &meta).await
                }
            },
        )
    }
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
    // The stat-based ceiling above refuses cheaply without touching the file;
    // this re-check closes the stat→read window a racing local writer could
    // grow the file through — the one gap in "refuse rather than degrade".
    if bytes.len() as u64 > MAX_BYTES {
        return fail(
            conn,
            ErrorCode::TooLarge,
            format!(
                "{} bytes exceeds the {MAX_BYTES}-byte view limit",
                bytes.len()
            ),
        );
    }
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
