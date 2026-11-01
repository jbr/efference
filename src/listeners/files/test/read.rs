//! Tests for reading one file, and what the route refuses.

use super::*;
use crate::listeners::handler::App;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_apps_keep_independent_file_roots() {
    let one = Scratch::new("session-one");
    let two = Scratch::new("session-two");
    one.write("identity.txt", "one");
    two.write("identity.txt", "two");
    let App::Session { files: first } = App::session(one.path()) else {
        panic!("session app");
    };
    let App::Session { files: second } = App::session(two.path()) else {
        panic!("session app");
    };
    let first = TestServer::new(handler(first)).await;
    let second = TestServer::new(handler(second)).await;
    first
        .get("/files/identity.txt")
        .await
        .assert_status(200)
        .assert_body("one");
    second
        .get("/files/identity.txt")
        .await
        .assert_status(200)
        .assert_body("two");
    first.get("/files/identity.txt").await.assert_body("one");
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
    std::thread::sleep(Duration::from_millis(5));
    scratch.write("moving.txt", "after!");

    server
        .get("/files/moving.txt")
        .with_request_header("if-none-match", etag)
        .await
        .assert_status(200)
        .assert_body("after!");
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
    // Every method, not just the one a form could send: the surface is read-only
    // whatever the verb, and a method the routes did not name must not answer
    // with a router's own 405 either.
    server.put("/files/held.txt").await.assert_status(400);
    server.delete("/files/held.txt").await.assert_status(400);
    server.patch("/files/held.txt").await.assert_status(400);
    server.post("/files/").await.assert_status(400);
    server.post("/files").await.assert_status(400);
    assert_eq!(
        std::fs::read_to_string(scratch.path().join("held.txt")).unwrap(),
        "h"
    );
}
