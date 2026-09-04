//! Tests for the per-server perimeter report: the mark beside each advertised
//! tool is the file's `tools` decision — exact grant, dotted-alias-resolved
//! grant, or the terminal-`.*` namespace glob — and nothing else (docs/tools.md).

use super::*;

fn role_with_tools(entries: &[&str]) -> efference::config::Config {
    let mut text = String::from("tools = [");
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            text.push_str(", ");
        }
        text.push_str(&format!("\"{entry}\""));
    }
    text.push_str("]\n");
    let path =
        std::env::temp_dir().join(format!("efference-check-mark-{}.toml", std::process::id()));
    std::fs::write(&path, &text).unwrap();
    let role = efference::config::load(Some(&path)).unwrap().role;
    let _ = std::fs::remove_file(&path);
    role
}

#[test]
fn the_mark_follows_the_grant_exact_glob_or_absent() {
    let role = role_with_tools(&["ferritin.get", "other.*"]);
    assert_eq!(grant_mark(&role, "ferritin__get"), "+");
    assert_eq!(grant_mark(&role, "other__anything"), "+");
    assert_eq!(grant_mark(&role, "ferritin__unlisted"), "-");
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
    let required = write_config(
        "tools = []\n\n[mcp.servers.broken]\ncommand = \"efference-definitely-not-a-server\"\n",
    );
    let err = check(Some(required.clone()))
        .await
        .expect_err("a required server that cannot spawn must fail the check");
    assert!(
        err.to_string().contains("broken"),
        "the error names the server: {err}"
    );
    let _ = std::fs::remove_file(&required);

    let optional = write_config(
        "tools = []\n\n[mcp.servers.broken]\ncommand = \"efference-definitely-not-a-server\"\noptional = true\n",
    );
    check(Some(optional.clone()))
        .await
        .expect("an optional server that cannot spawn is tolerated");
    let _ = std::fs::remove_file(&optional);
}
