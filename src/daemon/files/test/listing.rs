//! Tests for directory listings and the discovery ignore set.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_a_directory_sorted_and_without_the_ignored_names() {
    let scratch = Scratch::new("list");
    scratch.write(".gitignore", "target/\n");
    scratch.write("beta.md", "b");
    scratch.write("alpha.rs", "a");
    scratch.write("target/debug/junk", "j");
    scratch.write("sub/nested.txt", "n");
    let server = server(&scratch).await;

    let conn = server.get("/files/").await;
    conn.assert_status(200)
        .assert_header("content-type", "application/json");
    let body: Value = serde_json::from_str(conn.body()).unwrap();
    // Sorted, and `target/` is absent because *this project's* `.gitignore`
    // says so — the set is gitignore-derived, not a name list the daemon
    // carries. `.gitignore` itself is listed: nothing is hidden for being a
    // dotfile.
    assert_eq!(names(&body), [".gitignore", "alpha.rs", "beta.md", "sub"]);
    assert_eq!(body["path"], "");
    assert_eq!(body["recursive"], false);
    assert_eq!(body["truncated"], false);
    assert_eq!(body["entries"][1]["kind"], "file");
    assert_eq!(body["entries"][3]["kind"], "dir");
}

/// The entry names of a listing body, in the order served.
fn names(body: &Value) -> Vec<&str> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_git_directory_is_never_listed() {
    // Unconditional, as git treats it: it appears in no `.gitignore` and no
    // listing wants it.
    let scratch = Scratch::new("git-dir");
    scratch.write(".git/config", "[core]");
    scratch.write("kept.txt", "k");
    let server = server(&scratch).await;

    let conn = server.get("/files/").await;
    assert_eq!(
        names(&serde_json::from_str(conn.body()).unwrap()),
        ["kept.txt"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_gitignore_governs_its_own_subtree() {
    // The deepest matching rule decides, git's precedence — and a `!` line
    // re-includes below an ignore above it.
    let scratch = Scratch::new("nested-ignore");
    scratch.write(".gitignore", "*.log\n");
    scratch.write("sub/.gitignore", "!keep.log\nlocal/\n");
    scratch.write("sub/keep.log", "k");
    scratch.write("sub/drop.log", "d");
    scratch.write("sub/local/thing.txt", "t");
    let server = server(&scratch).await;

    let conn = server.get("/files/sub/").await;
    assert_eq!(
        names(&serde_json::from_str(conn.body()).unwrap()),
        [".gitignore", "keep.log"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_git_excluded_path_is_absent_from_listings_but_still_served() {
    // The case the ignore set is known to bite: a directory hidden through
    // `.git/info/exclude`. Naming is the permission, so it stays
    // readable — and followable — while being undiscoverable.
    let scratch = Scratch::new("excluded");
    scratch.write(".git/info/exclude", "plans/\n");
    scratch.write("plans/roadmap.md", "still readable");
    let server = server(&scratch).await;

    let conn = server.get("/files/").await;
    assert_eq!(
        names(&serde_json::from_str(conn.body()).unwrap()),
        Vec::<&str>::new()
    );
    server
        .get("/files/plans/roadmap.md")
        .await
        .assert_status(200)
        .assert_body("still readable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recursive_listing_names_entries_by_path() {
    let scratch = Scratch::new("recursive");
    scratch.write(".gitignore", "target/\n");
    scratch.write("a.rs", "a");
    scratch.write("sub/b.rs", "b");
    scratch.write("sub/deep/c.rs", "c");
    scratch.write("target/junk", "j");
    let server = server(&scratch).await;

    let conn = server.get("/files/?recursive=1").await;
    let body: Value = serde_json::from_str(conn.body()).unwrap();
    assert_eq!(body["recursive"], true);
    assert_eq!(
        names(&body),
        [
            ".gitignore",
            "a.rs",
            "sub",
            "sub/b.rs",
            "sub/deep",
            "sub/deep/c.rs"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recursive_depth_bounds_the_walk() {
    let scratch = Scratch::new("depth");
    scratch.write("sub/deep/c.rs", "c");
    let server = server(&scratch).await;

    let conn = server.get("/files/?recursive=1&depth=2").await;
    let body: Value = serde_json::from_str(conn.body()).unwrap();
    assert_eq!(names(&body), ["sub", "sub/deep"]);
}

#[test]
fn listing_options_are_clamped_rather_than_refused() {
    // A read surface a human pokes at from a URL bar: an absurd or unparseable
    // value is the default, never a 400.
    assert_eq!(
        Options::from_query("recursive=1&depth=9999"),
        Options {
            recursive: true,
            depth: list::MAX_DEPTH
        }
    );
    assert_eq!(
        Options::from_query(""),
        Options {
            recursive: false,
            depth: 1
        }
    );
    assert_eq!(
        Options::from_query("recursive=0&depth=nonsense"),
        Options {
            recursive: false,
            depth: 1
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ignored_name_is_still_readable_by_path() {
    let scratch = Scratch::new("ignored-read");
    scratch.write(".gitignore", "target/\n");
    scratch.write("target/note.txt", "still readable");
    let server = server(&scratch).await;

    // The ignore set is discovery's, not the read route's: anything you can
    // name, you can read.
    server
        .get("/files/target/note.txt")
        .await
        .assert_status(200)
        .assert_body("still readable");
}

/// A trailing slash names nothing: a directory is a directory because the
/// filesystem says so, and both spellings of one are the same resource.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_spellings_of_a_directory_list_it() {
    let scratch = Scratch::new("spellings");
    scratch.write("sub/nested.txt", "n");
    let server = server(&scratch).await;

    for path in ["/files/sub", "/files/sub/"] {
        let conn = server.get(path).await;
        conn.assert_status(200);
        let body: Value = serde_json::from_str(conn.body()).unwrap();
        assert_eq!(body["path"], "sub", "{path}");
        assert_eq!(body["entries"][0]["name"], "nested.txt", "{path}");
    }
}

/// Including the root, whose slashless spelling is the empty wildcard capture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_spellings_of_the_root_list_it() {
    let scratch = Scratch::new("root-spellings");
    scratch.write("a.txt", "a");
    let server = server(&scratch).await;

    for path in ["/files", "/files/"] {
        let conn = server.get(path).await;
        conn.assert_status(200);
        let body: Value = serde_json::from_str(conn.body()).unwrap();
        assert_eq!(body["path"], "", "{path}");
        assert_eq!(body["entries"][0]["name"], "a.txt", "{path}");
    }
}

/// The listing's shape rides in the query, and is read off whichever spelling
/// asked — there is no redirect left to preserve it across.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_query_shapes_either_spelling() {
    let scratch = Scratch::new("spelling-query");
    scratch.write("sub/deep/nested.txt", "n");
    let server = server(&scratch).await;

    for path in ["/files/sub?recursive=1", "/files/sub/?recursive=1"] {
        let conn = server.get(path).await;
        conn.assert_status(200);
        let body: Value = serde_json::from_str(conn.body()).unwrap();
        assert_eq!(body["recursive"], true, "{path}");
        let names: Vec<&str> = body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"deep/nested.txt"), "{names:?} for {path}");
    }
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

/// A listing's bytes (docs/web-bind.md): the fields in the order the type declares them, an
/// entry's `kind` following a symlink while `symlink` says the entry is one, a link whose
/// target is gone listed as `other` with no size or time, and the directory's own path
/// beside its entries. Only files and links are listed, since a directory's own size is
/// the filesystem's to choose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listing_keeps_its_bytes() {
    let scratch = Scratch::new("bytes");
    for (name, text, at) in [
        ("a.txt", "a", 1_754_300_000_000),
        ("sub/b.rs", "bb", 1_754_300_060_000),
    ] {
        let file = std::fs::File::options()
            .write(true)
            .open(scratch.write(name, text))
            .expect("open");
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_millis(at))
            .expect("set the modification time");
    }
    let nested = server(&scratch).await;
    let listing = nested.get("/files/sub/").await;
    listing.assert_status(200);
    assert_eq!(
        listing.body(),
        r#"{"path":"sub","recursive":false,"truncated":false,"entries":[{"name":"b.rs","kind":"file","size":2,"modified_at":1754300060000,"symlink":false}]}"#
    );

    let flat = Scratch::new("bytes-flat");
    let file = std::fs::File::options()
        .write(true)
        .open(flat.write("a.txt", "a"))
        .expect("open");
    file.set_modified(std::time::UNIX_EPOCH + Duration::from_millis(1_754_300_000_000))
        .expect("set the modification time");
    std::os::unix::fs::symlink(flat.path().join("a.txt"), flat.path().join("link"))
        .expect("symlink");
    std::os::unix::fs::symlink(flat.path().join("gone"), flat.path().join("dangling"))
        .expect("symlink");
    let server = server(&flat).await;
    let listing = server.get("/files/").await;
    listing.assert_status(200);
    assert_eq!(
        listing.body(),
        concat!(
            r#"{"path":"","recursive":false,"truncated":false,"entries":["#,
            r#"{"name":"a.txt","kind":"file","size":1,"modified_at":1754300000000,"symlink":false},"#,
            r#"{"name":"dangling","kind":"other","size":0,"modified_at":null,"symlink":true},"#,
            r#"{"name":"link","kind":"file","size":1,"modified_at":1754300000000,"symlink":true}]}"#
        )
    );
}
