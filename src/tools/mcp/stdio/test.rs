use super::*;

#[tokio::test]
async fn stdio_env_is_snapshot_then_stanza_with_root_removed_last() {
    let base = crate::config::env_table::EnvSnapshot::new(
        [
            ("IDENTITY".into(), "base".into()),
            ("KEPT".into(), "from-snapshot".into()),
        ]
        .into(),
    )
    .unwrap();
    let child_env = crate::config::env_table::ChildEnv::from_snapshot(base, vec![])
        .with_inject([("IDENTITY".into(), "table".into())].into());
    let stanza = [("IDENTITY".into(), "stanza".into())].into();
    let mut transport = StdioTransport::spawn(
        "/bin/sh",
        &[
            "-c".into(),
            "printf '%s|%s|%s\\n' \"$IDENTITY\" \"$KEPT\" \"${EFFERENCE_PROVIDER-unset}\"".into(),
        ],
        &stanza,
        &std::env::current_dir().unwrap(),
        &child_env,
    )
    .unwrap();
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        transport.pipe.reader.read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(line, "stanza|from-snapshot|unset\n");
    assert!(transport._child.wait().await.unwrap().success());
}
