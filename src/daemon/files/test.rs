//! Tests for `/files/**` (docs/web-bind.md), over the same in-memory transport the
//! rest of the daemon's HTTP tests use.
//!
//! Two of these are the point rather than coverage: that a symlink pointing out of
//! the root is refused (the containment re-check after `canonicalize`), and that a
//! file's own type never reaches `content-type`.

use super::*;
use trillium_testing::TestServer;

/// A scratch directory, removed when the guard drops. Canonicalized on the way out
/// because macOS hands back `/var/…` for a `/private/var/…` temp dir, and the
/// handler compares canonical paths.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let n = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("efference-files-{name}-{n:016x}"));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir.canonicalize().expect("canonical scratch"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, rel: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&path, contents).expect("write");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A status nothing else here answers with, so "the file surface passed this
/// through" is distinguishable from "the file surface answered 404".
const FELL_THROUGH: u16 = 418;

/// The handler with a marker behind it, standing in for the web client's surface,
/// which is mounted after it on the real stack.
async fn server(scratch: &Scratch) -> TestServer<impl trillium::Handler> {
    TestServer::new((Files::rooted_at(scratch.path()), |conn: Conn| async move {
        conn.with_status(FELL_THROUGH).halt()
    }))
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_a_text_file_as_plain_text() {
    let scratch = Scratch::new("read");
    scratch.write("index.html", "<h1>not markup here</h1>");
    let server = server(&scratch).await;

    server
        .get("/files/index.html")
        .await
        .assert_status(200)
        // The file is HTML. It is served as text, because this origin holds a
        // session credential and must never mount a repository file as markup.
        .assert_header("content-type", "text/plain; charset=utf-8")
        .assert_body("<h1>not markup here</h1>");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_symlink_out_of_the_root_is_not_found() {
    let outside = Scratch::new("outside");
    let secret = outside.write("secret.txt", "not yours");
    let scratch = Scratch::new("symlink");
    // Inside the root by name, outside it once resolved: the case the containment
    // re-check after `canonicalize` exists for.
    std::os::unix::fs::symlink(&secret, scratch.path().join("escape.txt")).expect("symlink");
    let server = server(&scratch).await;

    server.get("/files/escape.txt").await.assert_status(404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dot_dot_never_serves_anything_above_the_root() {
    let scratch = Scratch::new("climb");
    scratch.write("a/b.txt", "b");
    let server = server(&scratch).await;

    // Over HTTP the guard is belt-and-braces: URL parsing removes dot segments
    // before the request goes out — percent-encoded ones too — so
    // `/files/../../etc/hosts` arrives as `/etc/hosts`, which this surface does not
    // claim, and `/files/a/../a/b.txt` arrives already flattened. Both spellings are
    // asserted as "did not serve a file above the root" rather than as one specific
    // status, because which layer refuses them is the URL crate's business, not this
    // handler's contract.
    server
        .get("/files/a/../a/b.txt")
        .await
        .assert_status(200)
        .assert_body("b");
    for climbing in ["/files/../../etc/hosts", "/files/%2e%2e/%2e%2e/etc/hosts"] {
        let conn = server.get(climbing).await;
        assert_ne!(
            conn.status(),
            200,
            "{climbing} served a file above the root"
        );
    }

    // The guard itself, where it does the work — a caller that is not a normalizing
    // URL parser. No amount of climbing leaves the root.
    let files = Files::rooted_at(scratch.path());
    assert!(files.resolve("/../..").is_none());
    assert!(files.resolve("/a/../../..").is_none());
    assert!(files.resolve("/a/../a/b.txt").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_utf8_is_refused_as_unsupported_content() {
    let scratch = Scratch::new("binary");
    scratch.write("blob.bin", [0xff, 0xfe, 0x00, 0x01]);
    let server = server(&scratch).await;

    server
        .get("/files/blob.bin")
        .await
        .assert_status(415)
        .assert_body_contains(r#""code":"unsupported_content""#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_past_the_cap_is_refused_as_too_large() {
    let scratch = Scratch::new("large");
    scratch.write("big.txt", "x".repeat(MAX_BYTES as usize + 1));
    let server = server(&scratch).await;

    server
        .get("/files/big.txt")
        .await
        .assert_status(413)
        .assert_body_contains(r#""code":"too_large""#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unchanged_file_revalidates_without_a_body() {
    let scratch = Scratch::new("etag");
    scratch.write("stable.txt", "unchanged");
    let server = server(&scratch).await;

    let first = server.get("/files/stable.txt").await;
    let etag = first.header("etag").expect("etag").to_owned();

    // The viewer polls this route to notice edits, so the unchanged answer — the
    // common one — carries no body.
    server
        .get("/files/stable.txt")
        .with_request_header("if-none-match", etag)
        .await
        .assert_status(304)
        .assert_body("");

    // A different validator is a miss, not a match.
    server
        .get("/files/stable.txt")
        .with_request_header("if-none-match", "\"0-0\"")
        .await
        .assert_status(200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_changes_the_validator() {
    let scratch = Scratch::new("revalidate");
    scratch.write("moving.txt", "before");
    let server = server(&scratch).await;
    let etag = server
        .get("/files/moving.txt")
        .await
        .header("etag")
        .expect("etag")
        .to_owned();

    // Same length, different content: the validator has to move on mtime alone, or
    // the viewer would poll forever and never see the edit.
    std::thread::sleep(std::time::Duration::from_millis(5));
    scratch.write("moving.txt", "after!");

    server
        .get("/files/moving.txt")
        .with_request_header("if-none-match", etag)
        .await
        .assert_status(200)
        .assert_body("after!");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_a_directory_sorted_and_without_the_ignored_names() {
    let scratch = Scratch::new("list");
    scratch.write("beta.md", "b");
    scratch.write("alpha.rs", "a");
    scratch.write("target/debug/junk", "j");
    scratch.write("sub/nested.txt", "n");
    let server = server(&scratch).await;

    let conn = server.get("/files/").await;
    conn.assert_status(200)
        .assert_header("content-type", "application/json");
    let body: Value = serde_json::from_str(conn.body()).unwrap();
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    // Sorted, and `target/` is absent: a listing that is stable across reads and
    // does not drown a tree in build output.
    assert_eq!(names, ["alpha.rs", "beta.md", "sub"]);
    assert_eq!(body["path"], "");
    assert_eq!(body["entries"][0]["kind"], "file");
    assert_eq!(body["entries"][2]["kind"], "dir");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ignored_name_is_still_readable_by_path() {
    let scratch = Scratch::new("ignored-read");
    scratch.write("target/note.txt", "still readable");
    let server = server(&scratch).await;

    // The ignore set is the listing's, not the read route's: anything you can name,
    // you can read.
    server
        .get("/files/target/note.txt")
        .await
        .assert_status(200)
        .assert_body("still readable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_without_a_trailing_slash_redirects() {
    let scratch = Scratch::new("redirect");
    scratch.write("sub/nested.txt", "n");
    let server = server(&scratch).await;

    server
        .get("/files/sub")
        .await
        .assert_status(301)
        .assert_header("location", "/files/sub/");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_listing_names_its_own_path() {
    let scratch = Scratch::new("nested");
    scratch.write("sub/nested.txt", "n");
    let server = server(&scratch).await;

    let conn = server.get("/files/sub/").await;
    let body: Value = serde_json::from_str(conn.body()).unwrap();
    assert_eq!(body["path"], "sub");
    assert_eq!(body["entries"][0]["name"], "nested.txt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn percent_encoded_segments_decode() {
    let scratch = Scratch::new("encoded");
    scratch.write("a file.txt", "spaced");
    let server = server(&scratch).await;

    server
        .get("/files/a%20file.txt")
        .await
        .assert_status(200)
        .assert_body("spaced");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_encoded_separator_does_not_re_enter_path_parsing() {
    let scratch = Scratch::new("sep");
    scratch.write("a/b.txt", "b");
    let server = server(&scratch).await;

    // `%2f` decodes to a separator *after* the split that was supposed to bound
    // segments; honouring it would reopen traversal below that bound.
    server.get("/files/a%2fb.txt").await.assert_status(404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paths_outside_the_prefix_pass_through_unclaimed() {
    let scratch = Scratch::new("passthrough");
    scratch.write("held.txt", "h");
    let server = server(&scratch).await;

    // Not `/files` or `/files/…`: claiming it would shadow the web client's own
    // surface, which is mounted after this handler.
    server
        .get("/filesomething")
        .await
        .assert_status(FELL_THROUGH);
    server.get("/").await.assert_status(FELL_THROUGH);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_are_refused() {
    let scratch = Scratch::new("readonly");
    scratch.write("held.txt", "h");
    let server = server(&scratch).await;

    server.post("/files/held.txt").await.assert_status(400);
    assert_eq!(
        std::fs::read_to_string(scratch.path().join("held.txt")).unwrap(),
        "h"
    );
}

/// A raw client over the same handler shape as [`server`], for the stream tests:
/// `TestServer`'s assertions buffer the whole body, which an event stream never
/// ends.
async fn raw_client(scratch: &Scratch) -> trillium_client::Client {
    let mut handler = (Files::rooted_at(scratch.path()), |conn: Conn| async move {
        conn.with_status(FELL_THROUGH).halt()
    });
    let context = trillium_testing::init(&mut handler).await;
    trillium_client::Client::new(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    )
    .with_base("http://localhost/")
}

/// Read the stream until `needle` appears in the body, or fail after a timeout.
async fn read_until(conn: &mut trillium_client::Conn, needle: &str) -> String {
    use futures_lite::AsyncReadExt;
    let mut body = Vec::new();
    let mut buf = [0u8; 1024];
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !String::from_utf8_lossy(&body).contains(needle) {
            let n = conn
                .response_body()
                .read(&mut buf)
                .await
                .expect("read body");
            assert_ne!(n, 0, "the stream ended before {needle:?}");
            body.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{needle:?} within the timeout; got {:?}",
            String::from_utf8_lossy(&body)
        )
    });
    String::from_utf8_lossy(&body).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_event_stream_accept_streams_change_pokes() {
    let scratch = Scratch::new("stream");
    scratch.write("watched.txt", "first");
    let client = raw_client(&scratch).await;

    let mut conn = client
        .get("/files/watched.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));
    assert!(
        conn.response_headers()
            .get_str(KnownHeaderName::ContentType)
            .is_some_and(|t| t.contains("text/event-stream")),
        "an event-stream content type"
    );

    scratch.write("watched.txt", "second");
    let body = read_until(&mut conn, "event: change").await;
    assert!(body.contains("data: watched.txt"), "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_on_a_missing_file_reports_its_arrival() {
    // The watch is on the parent directory, so the file need not exist when the
    // stream opens — a viewer sitting on a refusal screen hears the restore.
    let scratch = Scratch::new("arrival");
    let client = raw_client(&scratch).await;

    let mut conn = client
        .get("/files/coming.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));

    scratch.write("coming.txt", "arrived");
    let body = read_until(&mut conn, "event: change").await;
    assert!(body.contains("data: coming.txt"), "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_default_accept_gets_the_file_never_a_stream() {
    // The negotiation is explicit-only: `*/*` — a fetch's default — must get
    // the body, or every plain read would stream (docs/web-bind.md).
    let scratch = Scratch::new("default-accept");
    scratch.write("plain.txt", "the body");
    let server = server(&scratch).await;

    server
        .get("/files/plain.txt")
        .with_request_header(KnownHeaderName::Accept, "*/*")
        .await
        .assert_status(200)
        .assert_header("content-type", "text/plain; charset=utf-8")
        .assert_body("the body");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_streams_are_refused_for_now() {
    let scratch = Scratch::new("dir-stream");
    scratch.write("sub/nested.txt", "n");
    let server = server(&scratch).await;

    server
        .get("/files/sub/")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .assert_status(400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_outside_the_root_is_not_found() {
    let scratch = Scratch::new("stream-escape");
    let server = server(&scratch).await;

    // As with the read route's `..` test: URL parsing flattens dot segments
    // before the request goes out, so the assertion is "did not open a stream",
    // whichever layer refused. The handler's own guard is `resolve` on the
    // parent, shared with the read route.
    let conn = server
        .get("/files/../outside.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await;
    assert_ne!(conn.status(), trillium::Status::Ok);
}

/// On case-insensitive APFS, a wrong-case URL resolves and reads fine, but
/// notify reports the on-disk spelling — the filter must adopt the canonical
/// final segment at open or the stream never fires (found in review,
/// 2026-08-21). macOS-only: on a case-sensitive fs the wrong-case path simply
/// doesn't exist.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_case_stream_still_fires_where_the_fs_is_case_insensitive() {
    let scratch = Scratch::new("case");
    scratch.write("cased.txt", "first");
    let client = raw_client(&scratch).await;

    let mut conn = client
        .get("/files/CASED.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));

    scratch.write("cased.txt", "second");
    let body = read_until(&mut conn, "event: change").await;
    // The event data carries the disk spelling, not the URL's.
    assert!(body.contains("data: cased.txt"), "{body}");
}

/// The feature's central lifetime claim, pinned through the HTTP layer: a
/// closed stream connection releases its watch (the guard drops with the
/// stream's state). Also pins `data:` at nested depth — `rel` was otherwise
/// only exercised at the root. The drop is noticed when the server next
/// writes, so the loop keeps poking the file until the count falls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_stream_releases_its_watch() {
    let scratch = Scratch::new("teardown");
    scratch.write("sub/held.txt", "h");
    let files = Files::rooted_at(scratch.path());
    let watch = files.watch.clone().expect("watcher");
    let mut handler = (files, |conn: Conn| async move {
        conn.with_status(FELL_THROUGH).halt()
    });
    let context = trillium_testing::init(&mut handler).await;
    let client = trillium_client::Client::new(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    )
    .with_base("http://localhost/");

    let mut conn = client
        .get("/files/sub/held.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));
    assert_eq!(watch.watched_count(), 1);

    scratch.write("sub/held.txt", "h2");
    let body = read_until(&mut conn, "event: change").await;
    assert!(body.contains("data: sub/held.txt"), "{body}");

    drop(conn);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while watch.watched_count() != 0 {
            scratch.write("sub/held.txt", "again");
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    })
    .await
    .expect("the watch must release after the stream closes");
}

/// A symlink resolving into a *nested* directory of the root: the canonical
/// target's parent is watched (a non-recursive watch on the link's own
/// directory would never see the target's events), and the event data carries
/// the canonical spelling — the read route's resolution, applied to the
/// stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_symlink_into_a_nested_directory_streams_its_target() {
    let scratch = Scratch::new("nested-link");
    let target = scratch.write("sub/target.txt", "t");
    std::os::unix::fs::symlink(&target, scratch.path().join("link.txt")).expect("symlink");
    let client = raw_client(&scratch).await;

    let mut conn = client
        .get("/files/link.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));

    scratch.write("sub/target.txt", "changed");
    let body = read_until(&mut conn, "event: change").await;
    assert!(body.contains("data: sub/target.txt"), "{body}");
}

/// A stream through a symlink out of the root is refused, as the read route
/// refuses the read — never served-but-deaf.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_through_a_symlink_out_of_the_root_is_refused() {
    let outside = Scratch::new("stream-outside");
    let secret = outside.write("secret.txt", "not yours");
    let scratch = Scratch::new("stream-escape-link");
    std::os::unix::fs::symlink(&secret, scratch.path().join("escape.txt")).expect("symlink");
    let server = server(&scratch).await;

    server
        .get("/files/escape.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .assert_status(404);
}

/// A lone CR in a file name (legal on disk) must not reach the SSE wire raw:
/// browsers split fields on it, which would let a workspace file name forge
/// pokes for arbitrary paths. The `rel` boundary percent-encodes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_carriage_return_in_a_name_cannot_forge_events() {
    let scratch = Scratch::new("cr-name");
    scratch.write("evil\rname.txt", "x");
    let client = raw_client(&scratch).await;

    let mut conn = client
        .get("/files/evil%0Dname.txt")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));

    scratch.write("evil\rname.txt", "y");
    let body = read_until(&mut conn, "event: change").await;
    assert!(body.contains("data: evil%0Dname.txt"), "{body:?}");
    assert!(
        !body.contains('\r'),
        "a raw CR reached the SSE wire: {body:?}"
    );
}
