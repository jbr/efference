//! Tests for `[mcp.servers.*]`.

use super::*;

#[test]
fn mcp_servers_parse_into_transports() {
    let config = parse_str(
        r#"
[mcp.servers.github]
command = "gh-mcp-server"
args = ["--stdio"]
env = { GITHUB_TOKEN = "t" }

[mcp.servers.sentry]
url = "https://mcp.example.com/mcp"
"#,
    );
    assert_eq!(config.mcp_servers.len(), 2);
    // BTreeMap: mount order is name order, deterministic.
    let names: Vec<&str> = config.mcp_servers.keys().map(String::as_str).collect();
    assert_eq!(names, ["github", "sentry"]);
    assert!(matches!(
        &config.mcp_servers["github"].transport,
        McpTransport::Stdio { command, args, env }
            if command == "gh-mcp-server" && args == &["--stdio"] && env["GITHUB_TOKEN"] == "t"
    ));
    assert!(matches!(
        &config.mcp_servers["sentry"].transport,
        McpTransport::Http { url } if url == "https://mcp.example.com/mcp"
    ));
    // Required is the default: a declared server that will not mount stops the boot.
    assert!(config.mcp_servers.values().all(|entry| !entry.optional));
}

/// `optional = true` is the opt-out from the fatal default. The default is the
/// strict one deliberately: an opt-*in* to strictness is one nobody remembers to
/// write, so the safe behavior has to be what you get for free.
#[test]
fn a_server_can_opt_out_of_being_required() {
    let config = parse_str(
        r#"
[mcp.servers.required_by_default]
command = "a"

[mcp.servers.explicitly_optional]
command = "b"
optional = true

[mcp.servers.explicitly_required]
url = "https://example.com/mcp"
optional = false
"#,
    );
    assert!(!config.mcp_servers["required_by_default"].optional);
    assert!(config.mcp_servers["explicitly_optional"].optional);
    assert!(!config.mcp_servers["explicitly_required"].optional);
}

/// The stanza's shape picks the transport, so it must commit to exactly one.
#[test]
fn mcp_server_with_both_or_neither_transport_fails_the_file() {
    for text in [
        "[mcp.servers.x]\ncommand = \"a\"\nurl = \"https://b\"\n",
        "[mcp.servers.x]\nargs = [\"--stdio\"]\n",
    ] {
        let err = parse(Path::new("bad.toml"), text).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::McpServerTransport(_))
        ));
    }
}

/// `args`/`env` parameterize a spawned subprocess; an HTTP server is not spawned.
#[test]
fn mcp_http_server_with_spawn_keys_fails_the_file() {
    let err = parse(
        Path::new("bad.toml"),
        "[mcp.servers.x]\nurl = \"https://b\"\nenv = { A = \"1\" }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::McpServerHttpExtras(_))
    ));
}

/// The server name prefixes every advertised tool on the wire (`<server>__<tool>`),
/// so it must fit the API's tool-name alphabet.
#[test]
fn mcp_server_name_outside_the_tool_alphabet_fails_the_file() {
    let err = parse(
        Path::new("bad.toml"),
        "[mcp.servers.\"my server!\"]\ncommand = \"a\"\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::McpServerName(_))
    ));
}

/// `__` is the namespace separator itself. A server name containing one would move the
/// boundary every direction of the mapping resolves as the *first* `__`, so it is
/// rejected even though `_` is otherwise a legal character.
#[test]
fn mcp_server_name_containing_the_separator_fails_the_file() {
    let err = parse(
        Path::new("bad.toml"),
        "[mcp.servers.my__server]\ncommand = \"a\"\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::McpServerName(_))
    ));
    // A single underscore is still fine — only the doubled form is the separator.
    let ok = parse(
        Path::new("ok.toml"),
        "[mcp.servers.my_server]\ncommand = \"a\"\n",
    )
    .unwrap();
    assert!(ok.mcp_servers.contains_key("my_server"));
}
