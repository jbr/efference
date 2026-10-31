//! The fixture builder reads back as the file a test would have written by hand
//! (docs/permissions.md).

use super::*;
use crate::config::testing::ConfigFixture;

/// Every entry the builder writes parses to the config the same entries written as TOML
/// do, so a test moved from its TOML onto the builder keeps its input.
#[test]
fn a_fixture_is_the_config_its_toml_would_be() {
    // Root keys follow the tables here on purpose: the order of the calls is not the
    // order of the file.
    let built = ConfigFixture::new()
        .with_tool("read")
        .with_tool("write")
        .with_tool("agents__message")
        .with_namespace("lsp")
        .with_path(".", true, true)
        .with_path("/refs", true, false)
        .with_path("out", false, true)
        .with_path(".efference", false, false)
        .with_command("cargo", CommandStance::Clear)
        .with_command("git push", CommandStance::Gate)
        .with_command("npm", CommandStance::Deny)
        .with_command("make", CommandStance::Watch)
        .with_watch_for("deploy keys")
        .with_scope("Fix the build.")
        .with_watch_for("network egress")
        .build();
    let written = parse_str(
        r#"
        scope = "Fix the build."
        watch-for = ["deploy keys", "network egress"]

        [tools]
        efference = { enabled = true }
        agents = { enabled = true }
        lsp = { enabled = true }
        "efference.read" = { granted = true }
        "efference.write" = { granted = true }
        "agents.message" = { granted = true }

        [paths]
        "." = true
        "/refs" = { read = true }
        "out" = { write = true }
        ".efference" = false

        [commands]
        cargo = true
        "git push" = "gate"
        npm = false
        make = "watch"
        "#,
    );
    assert_eq!(format!("{built:?}"), format!("{written:?}"));
}

/// With nothing written, the fixture is the config of an empty file.
#[test]
fn an_empty_fixture_is_the_default_config() {
    assert_eq!(
        format!("{:?}", ConfigFixture::new().build()),
        format!("{:?}", Config::default())
    );
}
