use super::*;

fn home(tag: &str) -> std::path::PathBuf {
    let home =
        std::env::temp_dir().join(format!("eff-updates-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".efference")).unwrap();
    home
}

/// The answer is read beside the connections; an absent table, and an absent file,
/// check nothing.
#[test]
fn the_answer_parses_and_no_answer_checks_nothing() {
    let home = home("parse");
    let config = home.join(".efference/config.toml");
    std::fs::write(
        &config,
        r#"
[updates]
check = true

[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "K" }
"#,
    )
    .unwrap();
    assert!(UpdatesConfig::load(&home).unwrap().check);

    std::fs::write(&config, "[updates]\ncheck = false\n").unwrap();
    assert!(!UpdatesConfig::load(&home).unwrap().check);

    std::fs::write(&config, "[updates]\n").unwrap();
    assert!(!UpdatesConfig::load(&home).unwrap().check);

    std::fs::write(&config, "").unwrap();
    assert!(!UpdatesConfig::load(&home).unwrap().check);
    assert!(!UpdatesConfig::load(&home.join("nowhere")).unwrap().check);
    std::fs::remove_dir_all(&home).unwrap();
}

/// A misspelled key or a value that is not a boolean is refused, as everywhere in
/// the file.
#[test]
fn an_unknown_key_or_a_non_boolean_is_refused() {
    let home = home("edges");
    let config = home.join(".efference/config.toml");
    std::fs::write(&config, "[updates]\nchecks = true\n").unwrap();
    let err = UpdatesConfig::load(&home).unwrap_err().to_string();
    assert!(err.contains("checks"), "{err}");
    std::fs::write(&config, "[updates]\ncheck = \"yes\"\n").unwrap();
    assert!(UpdatesConfig::load(&home).is_err());
    std::fs::remove_dir_all(&home).unwrap();
}
