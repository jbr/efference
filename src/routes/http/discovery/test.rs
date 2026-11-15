use super::*;
use crate::config::policy::Directories;
use crate::host::starting::{Starting, StartingParts};
use std::sync::Arc;
use trillium_testing::TestServer;

/// A scratch directory and the guard that removes it.
fn scratch() -> (PathBuf, tempfile::TempDir) {
    let dir = crate::platform::testing::temp_dir("discovery");
    let path = dir.path().to_path_buf();
    (path, dir)
}

/// A resolved directory with no problem, for the listing tests that do not exercise
/// prefix parsing.
fn at(directory: &Path, spelling: &str, filter: &str) -> Resolution {
    Resolution {
        prefix: String::new(),
        spelling: spelling.to_string(),
        directory: directory.to_path_buf(),
        filter: filter.to_string(),
        problem: None,
    }
}

fn names(answer: &PathCandidates) -> Vec<&str> {
    answer
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect()
}

/// A test server whose only conn state is a starting session over `directories`.
async fn starting_server(directories: &Path) -> TestServer<impl trillium::Handler> {
    let starting = Starting::new(StartingParts {
        id: "s".to_string(),
        directories: Directories::at(directories),
        resume: false,
        created_by: None,
        detached: false,
    });
    TestServer::new((
        move |conn: Conn| {
            let starting = Arc::clone(&starting);
            async move { conn.with_state(starting) }
        },
        control_routes(),
    ))
    .await
}

// The permission-denied branch of `problem` is the same io-error path as a missing
// directory and a file named as a directory; it is not exercised here because a
// `chmod` fixture reads differently as root, and a test that passes only as one user
// is not coverage.

#[test]
fn prefixes_resolve_to_one_directory_and_a_filter() {
    let working = Path::new("/w");
    let home = Some(Path::new("/h"));
    let cases = [
        ("", "/w", "", ""),
        (".", "/w", ".", ""),
        ("..", "/", "..", ""),
        ("~", "/h", "~", ""),
        ("~/", "/h", "~/", ""),
        ("~/src/ma", "/h/src", "~/src/", "ma"),
        ("~bob/", "/w/~bob", "~bob/", ""),
        ("/usr/lo", "/usr", "/usr/", "lo"),
        ("ma", "/w", "", "ma"),
        ("a/b", "/w/a", "a/", "b"),
        ("./src/", "/w/src", "./src/", ""),
        ("/../x/", "/x", "/../x/", ""),
    ];
    for (prefix, directory, spelling, filter) in cases {
        let resolved = resolve(prefix, working, home);
        assert_eq!(
            resolved.directory,
            PathBuf::from(directory),
            "directory for {prefix:?}"
        );
        assert_eq!(resolved.spelling, spelling, "spelling for {prefix:?}");
        assert_eq!(resolved.filter, filter, "filter for {prefix:?}");
        assert!(resolved.problem.is_none(), "problem for {prefix:?}");
    }
}

#[test]
fn a_tilde_user_is_not_a_home_spelling() {
    let resolved = resolve("~bob", Path::new("/w"), Some(Path::new("/h")));
    assert_eq!(resolved.spelling, "");
    assert_eq!(resolved.filter, "~bob");
    assert_eq!(resolved.directory, PathBuf::from("/w"));

    let with_slash = resolve("~bob/", Path::new("/w"), Some(Path::new("/h")));
    assert_eq!(with_slash.spelling, "~bob/");
    assert_eq!(with_slash.filter, "");
    assert_eq!(with_slash.directory, PathBuf::from("/w/~bob"));
}

#[test]
fn no_home_makes_a_tilde_prefix_a_problem() {
    let resolved = resolve("~/src/", Path::new("/w"), None);
    assert_eq!(resolved.directory, PathBuf::from("/w"));
    assert_eq!(resolved.problem.as_deref(), Some(HOME_UNKNOWN));
}

#[test]
fn an_original_joins_the_typed_spelling_and_the_name() {
    assert_eq!(original("", "a"), "a");
    assert_eq!(original("./", "a"), "./a");
    assert_eq!(original("~", "a"), "~/a");
    assert_eq!(original("..", "a"), "../a");
    assert_eq!(original("~/src/", "a"), "~/src/a");
}

#[test]
fn a_listing_is_sorted_and_carries_absolute_paths() {
    let (dir, _guard) = scratch();
    std::fs::create_dir(dir.join("b-dir")).unwrap();
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    std::fs::write(dir.join("c.txt"), "c").unwrap();

    let answer = listing(&at(&dir, "", ""), 10);
    assert_eq!(names(&answer), ["a.txt", "b-dir", "c.txt"]);
    assert_eq!(answer.entries[0].original, "a.txt");
    assert_eq!(
        answer.entries[0].path,
        dir.join("a.txt").display().to_string()
    );
    assert_eq!(answer.entries[0].target, ReferenceTargetKind::File);
    assert!(!answer.entries[0].symlink);
    assert_eq!(answer.entries[1].target, ReferenceTargetKind::Directory);
    assert!(!answer.truncated);
    assert!(answer.problem.is_none());
}

#[test]
fn the_filter_matches_before_sorting_and_stating() {
    let (dir, _guard) = scratch();
    for name in ["a1", "a2", "b"] {
        std::fs::write(dir.join(name), name).unwrap();
    }
    let answer = listing(&at(&dir, "", "a"), 10);
    assert_eq!(names(&answer), ["a1", "a2"]);
}

#[test]
fn an_exact_name_still_lists() {
    let (dir, _guard) = scratch();
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    std::fs::write(dir.join("b.txt"), "b").unwrap();
    let answer = listing(&at(&dir, "", "a.txt"), 10);
    assert_eq!(names(&answer), ["a.txt"]);
}

#[test]
fn a_bare_dot_file_is_listed() {
    let (dir, _guard) = scratch();
    std::fs::write(dir.join(".env"), "x").unwrap();
    let answer = listing(&at(&dir, "", ".env"), 10);
    assert_eq!(names(&answer), [".env"]);
}

#[test]
fn a_limit_caps_candidates_and_sets_truncated() {
    let (dir, _guard) = scratch();
    for name in ["a", "b", "c"] {
        std::fs::write(dir.join(name), name).unwrap();
    }
    let answer = listing(&at(&dir, "", ""), 2);
    assert_eq!(names(&answer), ["a", "b"]);
    assert!(answer.truncated);
}

#[test]
fn a_zero_limit_returns_nothing_and_reports_truncation() {
    let (dir, _guard) = scratch();
    std::fs::write(dir.join("a"), "a").unwrap();
    let answer = listing(&at(&dir, "", ""), 0);
    assert!(answer.entries.is_empty());
    assert!(answer.truncated);
}

#[test]
fn a_directory_whose_path_is_not_utf8_is_a_problem_without_a_rewritten_path() {
    use std::os::unix::ffi::OsStringExt;
    // Constructed rather than created: APFS will not make such a name, but the guard
    // runs before any read, so the branch is exercised on every host.
    let directory = PathBuf::from(std::ffi::OsString::from_vec(b"/nonexistent/\xff".to_vec()));
    let answer = listing(&at(&directory, "", ""), 10);
    assert_eq!(answer.problem.as_deref(), Some(NON_UTF8));
    assert_eq!(answer.directory, "");
    assert!(answer.entries.is_empty());
}

#[test]
fn a_symlinks_kind_is_its_target_and_symlink_is_true() {
    let (dir, _guard) = scratch();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::os::unix::fs::symlink(dir.join("sub"), dir.join("link")).unwrap();
    std::os::unix::fs::symlink(dir.join("gone"), dir.join("dangling")).unwrap();

    let answer = listing(&at(&dir, "", ""), 10);
    assert_eq!(names(&answer), ["link", "sub"]);
    let link = &answer.entries[0];
    assert_eq!(link.target, ReferenceTargetKind::Directory);
    assert!(link.symlink);
}

#[test]
fn a_missing_directory_is_a_problem_with_no_entries() {
    let (dir, _guard) = scratch();
    let answer = listing(&at(&dir.join("nope"), "nope/", ""), 10);
    assert_eq!(answer.problem.as_deref(), Some("no such directory"));
    assert!(answer.entries.is_empty());
}

#[test]
fn a_file_named_as_a_directory_is_a_problem() {
    let (dir, _guard) = scratch();
    std::fs::write(dir.join("file"), "x").unwrap();
    let answer = listing(&at(&dir.join("file"), "file/", ""), 10);
    assert_eq!(answer.problem.as_deref(), Some("not a directory"));
    assert!(answer.entries.is_empty());
}

#[test]
fn the_query_parses_limits_and_rejects_the_rest() {
    assert_eq!(query("").unwrap(), (String::new(), DEFAULT_LIMIT));
    assert_eq!(query("prefix=./&limit=7").unwrap(), ("./".to_string(), 7));
    assert_eq!(query("limit=0").unwrap().1, 0);
    assert_eq!(query("limit=9999").unwrap().1, MAX_LIMIT);
    assert!(query("limit=x").is_err());
    assert!(query("limit=-1").is_err());
    assert!(query("limit=").is_err());
    let long = format!("prefix={}", "a".repeat(MAX_PREFIX + 1));
    assert!(query(&long).is_err());
}

#[tokio::test]
async fn a_starting_session_serves_path_candidates() {
    let (dir, _guard) = scratch();
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    std::fs::write(dir.join("b.txt"), "b").unwrap();
    let server = starting_server(&dir).await;

    let answer = server.get("/v1/discovery/paths?prefix=a").await;
    answer.assert_status(200);
    let body: Value = serde_json::from_str(answer.body()).unwrap();
    assert_eq!(body["directory"], dir.display().to_string());
    assert_eq!(body["entries"].as_array().unwrap().len(), 1);
    assert_eq!(body["entries"][0]["name"], "a.txt");
    assert_eq!(body["entries"][0]["original"], "a.txt");
    assert_eq!(
        body["entries"][0]["path"],
        dir.join("a.txt").display().to_string()
    );
}

#[tokio::test]
async fn a_missing_directory_is_a_problem_on_a_200() {
    let (dir, _guard) = scratch();
    let server = starting_server(&dir).await;
    let answer = server.get("/v1/discovery/paths?prefix=nope/").await;
    answer.assert_status(200);
    let body: Value = serde_json::from_str(answer.body()).unwrap();
    assert_eq!(body["problem"], "no such directory");
    assert_eq!(body["entries"], serde_json::json!([]));
}

#[tokio::test]
async fn a_zero_limit_returns_no_candidates() {
    let (dir, _guard) = scratch();
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    let server = starting_server(&dir).await;
    let answer = server.get("/v1/discovery/paths?prefix=a&limit=0").await;
    answer.assert_status(200);
    let body: Value = serde_json::from_str(answer.body()).unwrap();
    assert_eq!(body["entries"], serde_json::json!([]));
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn an_overlong_prefix_is_a_bad_request() {
    let server = TestServer::new(control_routes()).await;
    let answer = server
        .get(&format!(
            "/v1/discovery/paths?prefix={}",
            "a".repeat(MAX_PREFIX + 1)
        ))
        .await;
    answer.assert_status(400);
}

#[tokio::test]
async fn an_unparseable_limit_is_a_bad_request() {
    let server = TestServer::new(control_routes()).await;
    let answer = server.get("/v1/discovery/paths?limit=x").await;
    answer.assert_status(400);
}

#[tokio::test]
async fn agent_candidates_without_a_host_have_a_problem() {
    let server = TestServer::new(control_routes()).await;
    let answer = server.get("/v1/discovery/agents?prefix=@on").await;
    answer.assert_status(200);
    let body: Value = serde_json::from_str(answer.body()).unwrap();
    assert_eq!(
        body["problem"],
        "this listener serves one session; agent candidates need a host"
    );
    assert_eq!(body["agents"], serde_json::json!([]));
    assert_eq!(body["prefix"], "@on");
}
