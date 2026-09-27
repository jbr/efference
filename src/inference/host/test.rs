use super::*;

fn home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("eff-host-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".efference")).unwrap();
    home
}

#[test]
fn the_table_parses_beside_the_connections_and_expands_home() {
    let home = home("parse");
    std::fs::write(
        home.join(".efference/config.toml"),
        r#"
[host]
bind = "100.64.0.1:7878"
public-name = "box.example.ts.net"
tls-cert = "~/.tls/box.crt"
tls-key = "/etc/box.key"

[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "K" }
"#,
    )
    .unwrap();
    let host = HostConfig::load(&home).unwrap();
    assert_eq!(host.bind.as_deref(), Some("100.64.0.1:7878"));
    assert_eq!(host.public_name.as_deref(), Some("box.example.ts.net"));
    assert_eq!(host.tls_cert, Some(home.join(".tls/box.crt")));
    assert_eq!(host.tls_key, Some(PathBuf::from("/etc/box.key")));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn a_missing_file_is_an_empty_table_and_an_unknown_key_is_refused() {
    let home = home("edges");
    assert_eq!(
        HostConfig::load(&home.join("nowhere")).unwrap(),
        HostConfig::default()
    );
    std::fs::write(home.join(".efference/config.toml"), "[host]\nport = 1\n").unwrap();
    let err = HostConfig::load(&home).unwrap_err().to_string();
    assert!(err.contains("port"), "{err}");
    std::fs::remove_dir_all(&home).unwrap();
}
