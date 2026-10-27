//! Tests for the per-server perimeter report: the mark beside each advertised
//! tool is the file's `[tools]` decision — left out, granted by its own entry or
//! its namespace's, or not granted — and nothing else (docs/tools.md).

use super::*;

#[test]
fn the_mark_follows_the_table_tool_namespace_absent_or_off() {
    let role = efference::config::Config::from_toml(
        r#"
        [tools]
        "ferritin.get" = { granted = true }
        "ferritin.hidden" = { enabled = false }
        other = { granted = true }

        [mcp.servers.ferritin]
        command = "ferritin"

        [mcp.servers.other]
        command = "other"
        "#,
    )
    .expect("valid config");
    assert_eq!(grant_mark(&role, "ferritin__get"), "+");
    assert_eq!(grant_mark(&role, "other__anything"), "+");
    assert_eq!(grant_mark(&role, "ferritin__unlisted"), "-");
    assert_eq!(grant_mark(&role, "ferritin__hidden"), "x");
}

fn write_config(text: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("efference-check-mount-{}.toml", std::process::id()));
    std::fs::write(&path, text).unwrap();
    path
}

/// Failure policy mirrors boot (docs/tools.md): a required server that does not
/// mount fails the check; an `optional = true` one is reported and tolerated.
/// A nonexistent command exercises both branches with no network and no fixture
/// binary — the spawn itself fails.
#[tokio::test]
async fn an_unmountable_server_fails_unless_optional() {
    let required =
        write_config("[mcp.servers.broken]\ncommand = \"efference-definitely-not-a-server\"\n");
    let err = check(Some(required.clone()))
        .await
        .expect_err("a required server that cannot spawn must fail the check");
    assert!(
        err.to_string().contains("broken"),
        "the error names the server: {err}"
    );
    let _ = std::fs::remove_file(&required);

    let optional = write_config(
        "[mcp.servers.broken]\ncommand = \"efference-definitely-not-a-server\"\noptional = true\n",
    );
    check(Some(optional.clone()))
        .await
        .expect("an optional server that cannot spawn is tolerated");
    let _ = std::fs::remove_file(&optional);
}
