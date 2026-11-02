//! What `efference update --check` prints (docs/update-check.md).

use super::*;

fn release(version: &str) -> Release {
    Release {
        version: version.into(),
        tag: format!("v{version}"),
        url: format!("https://efference.net/releases/v{version}"),
    }
}

/// A newer release prints both versions and the install command.
#[test]
fn a_newer_release_prints_the_install_command() {
    assert_eq!(
        report("0.1.0", &release("0.2.0")).unwrap(),
        "installed: efference 0.1.0\n\
         newest:    efference 0.2.0\n\
         \n\
         To update, run the install command again:\n  \
         curl -fsSL https://efference.net/install.sh | sh\n"
    );
}

/// The newest release installed, or a build ahead of it, prints no command.
#[test]
fn the_newest_or_a_later_build_prints_no_command() {
    let current = report("0.2.0", &release("0.2.0")).unwrap();
    assert!(
        current.ends_with("\nThis is the newest release.\n"),
        "{current}"
    );
    let ahead = report("0.3.0-dev", &release("0.2.0")).unwrap();
    assert!(
        ahead.ends_with("\nThis build is newer than the newest release.\n"),
        "{ahead}"
    );
    assert!(!ahead.contains("install.sh"));
}

/// The verb refuses without `--check`, and says what to run instead.
#[tokio::test]
async fn update_without_check_names_the_check_and_the_install_command() {
    let err = run(false).await.unwrap_err().to_string();
    assert!(err.contains("efference update --check"), "{err}");
    assert!(err.contains("install.sh | sh"), "{err}");
}

/// `--version` prints the version and the build's revision and profile; `update` takes
/// `--check`.
#[test]
fn the_version_flag_and_the_update_verb_parse() {
    use clap::Parser;
    let Err(err) = Cli::try_parse_from(["efference", "--version"]) else {
        panic!("--version exits");
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    assert_eq!(
        err.to_string(),
        format!(
            "efference {}{}\n",
            env!("CARGO_PKG_VERSION"),
            env!("EFFERENCE_BUILD_INFO")
        )
    );
    let cli = Cli::try_parse_from(["efference", "update", "--check"]).unwrap();
    assert!(matches!(cli.command, Some(Command::Update { check: true })));
}
