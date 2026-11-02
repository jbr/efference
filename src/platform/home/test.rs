use super::*;

#[test]
fn a_path_under_home_is_written_with_a_tilde() {
    let home = Some(Path::new("/home/someone"));
    assert_eq!(
        under_home(Path::new("/home/someone/code/thing"), home),
        "~/code/thing"
    );
    assert_eq!(under_home(Path::new("/home/someone"), home), "~");
    assert_eq!(under_home(Path::new("/var/tmp"), home), "/var/tmp");
    assert_eq!(
        under_home(Path::new("/home/someone-else"), home),
        "/home/someone-else"
    );
    assert_eq!(
        under_home(Path::new("/home/someone"), None),
        "/home/someone"
    );
}

#[test]
fn a_home_with_a_trailing_slash_still_matches() {
    assert_eq!(
        under_home(
            Path::new("/home/someone/code"),
            Some(Path::new("/home/someone/"))
        ),
        "~/code"
    );
}

#[test]
fn a_leading_tilde_is_read_as_the_home() {
    let home = Some(Path::new("/home/someone"));
    let expand = |path: &str| expand(Path::new(path), home);
    assert_eq!(expand("~"), Path::new("/home/someone"));
    assert_eq!(expand("~/"), Path::new("/home/someone"));
    assert_eq!(expand("~/x"), Path::new("/home/someone/x"));
    assert_eq!(expand("~/x/y"), Path::new("/home/someone/x/y"));
}

#[test]
fn any_other_path_is_left_as_written() {
    let home = Some(Path::new("/home/someone"));
    for path in ["/var/tmp", "x/y", "~other/x", "~other", "a/~/b", "./~"] {
        assert_eq!(expand(Path::new(path), home), Path::new(path), "{path}");
    }
    assert_eq!(expand(Path::new("~/x"), None), Path::new("~/x"));
}
