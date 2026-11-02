use super::*;

/// The `cwd` and the issues a preview body decodes to under `home`.
fn decode(cwd: &str, home: &str) -> (Option<PathBuf>, Vec<(String, String)>) {
    let decoded = decode_session_options(&serde_json::json!({"cwd": cwd}), Some(Path::new(home)))
        .expect("an object body decodes");
    let issues = decoded
        .issues
        .into_iter()
        .map(|issue| (issue.field.into_owned(), issue.message))
        .collect();
    (decoded.cwd, issues)
}

#[test]
fn a_leading_tilde_in_cwd_is_the_hosts_home() {
    for (typed, expanded) in [
        ("~", "/home/host"),
        ("~/", "/home/host"),
        ("~/x", "/home/host/x"),
        ("/srv/x", "/srv/x"),
    ] {
        let (cwd, issues) = decode(typed, "/home/host");
        assert_eq!(cwd.as_deref(), Some(Path::new(expanded)), "{typed}");
        assert!(issues.is_empty(), "{typed}: {issues:?}");
    }
}

#[test]
fn a_relative_or_other_users_cwd_is_still_an_issue() {
    for typed in ["x/y", "~other/x", "~other"] {
        let (cwd, issues) = decode(typed, "/home/host");
        assert_eq!(cwd, None, "{typed}");
        assert_eq!(
            issues,
            [(
                "cwd".to_string(),
                "must be an absolute path or start with ~".to_string()
            )],
            "{typed}"
        );
    }
}
