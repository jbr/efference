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
