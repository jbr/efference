//! Tests for the change-poke streams.

use super::*;

/// A raw client over the same handler shape as [`server`], for the stream tests:
/// `TestServer`'s assertions buffer the whole body, which an event stream never
/// ends.
async fn raw_client(scratch: &Scratch) -> trillium_client::Client {
    let mut handler = (
        Files::rooted_at(scratch.path()).handler(),
        |conn: Conn| async move { conn.with_status(FELL_THROUGH).halt() },
    );
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
    tokio::time::timeout(Duration::from_secs(10), async {
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
async fn a_directory_stream_pokes_for_its_own_entries() {
    let scratch = Scratch::new("dir-stream");
    scratch.write("sub/nested.txt", "n");
    let client = raw_client(&scratch).await;

    let mut conn = client
        .get("/files/sub/")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));

    // Read past any poke the setup's own write produced: file events arrive late
    // (FSEvents coalesces on a latency), so `sub/nested.txt` can land after the
    // stream opens and be the first change. The needle is the arrival.
    scratch.write("sub/arrived.txt", "a");
    let body = read_until(&mut conn, "data: sub/arrived.txt").await;
    assert!(body.contains("event: change"), "{body}");
}

/// The stream route takes the filesystem's answer too, so neither spelling of a
/// directory is a file stream — the root's two spellings included, which is the
/// case with no name after the last separator to read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_spelling_of_a_directory_streams_it() {
    let scratch = Scratch::new("dir-stream-spellings");
    scratch.write("sub/nested.txt", "n");
    let client = raw_client(&scratch).await;

    for (path, arrival, event) in [
        ("/files/sub", "sub/one.txt", "data: sub/one.txt"),
        ("/files/sub/", "sub/two.txt", "data: sub/two.txt"),
        ("/files", "top.txt", "data: top.txt"),
        ("/files/", "other.txt", "data: other.txt"),
    ] {
        let mut conn = client
            .get(path)
            .with_request_header(KnownHeaderName::Accept, "text/event-stream")
            .await
            .expect("open stream");
        assert_eq!(conn.status().map(|s| s as u16), Some(200), "{path}");

        // Read past any poke the setup's own writes produced: the needle is the
        // arrival this iteration is waiting for, not merely the first change.
        scratch.write(arrival, "a");
        read_until(&mut conn, event).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_stream_is_silent_about_ignored_entries() {
    // One set governs listing, tree and watch: a build directory cannot flood a
    // stream whose consumer could not show the result anyway.
    let scratch = Scratch::new("dir-stream-ignored");
    scratch.write(".gitignore", "*.tmp\n");
    scratch.write("seen.txt", "s");
    let client = raw_client(&scratch).await;

    let mut conn = client
        .get("/files/")
        .with_request_header(KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open stream");
    assert_eq!(conn.status().map(|s| s as u16), Some(200));

    scratch.write("noise.tmp", "ignored");
    scratch.write("seen.txt", "changed");
    // Read through to the expected poke rather than the first one: the setup
    // writes above can still be in flight at open, and a poke for `.gitignore`
    // arriving first is not the silence under test.
    let body = read_until(&mut conn, "data: seen.txt").await;
    assert!(!body.contains("noise.tmp"), "{body}");
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
/// final segment at open or the stream never fires. macOS-only: on a
/// case-sensitive fs the wrong-case path simply doesn't exist.
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
    let mut handler = (files.handler(), |conn: Conn| async move {
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
    tokio::time::timeout(Duration::from_secs(10), async {
        while watch.watched_count() != 0 {
            scratch.write("sub/held.txt", "again");
            tokio::time::sleep(Duration::from_millis(150)).await;
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
