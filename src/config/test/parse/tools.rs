//! Tests for the `[tools]` table: which tools a session mounts and which it may call without
//! review (docs/tools.md, docs/permissions.md).

use super::*;

fn parse_err(text: &str) -> LoadError {
    parse(Path::new("bad.toml"), text).unwrap_err()
}

/// Opt-in: with no `[tools]` entry none of efference's own tools is mounted, and nothing is
/// granted — except the tools the turn loop drives, which every session has.
#[test]
fn an_empty_table_mounts_none_of_efferences_own_tools() {
    let role = parse_str("");
    for name in ["read", "bash", "edit", "agents__message", "lsp__definition"] {
        assert!(!role.tool_enabled(name), "{name}");
        assert!(!role.tools.granted(name), "{name}");
    }
    for name in ["consolidate", "kill_process", "ask", "wait", "check_inbox"] {
        assert!(role.tool_enabled(name), "{name}");
    }
}

/// The example the table was designed around: mount every `agents` verb, grant one.
#[test]
fn a_namespace_can_be_enabled_while_one_of_its_tools_is_granted() {
    let role = parse_str(
        r#"
            [tools]
            efference = { enabled = true, granted = true }
            agents = { enabled = true }
            "agents.message" = { granted = true }
        "#,
    );
    assert!(role.tool_enabled("read") && role.tools.granted("read"));
    assert!(role.tool_enabled("agents__message") && role.tools.granted("agents__message"));
    assert!(role.tool_enabled("agents__create"));
    assert!(!role.tools.granted("agents__create"));
}

/// A tool's entry beats its namespace's, field by field, so a namespace can carry exceptions.
#[test]
fn a_tool_entry_carves_out_of_its_namespace() {
    let role = parse_str(
        r#"
            [tools]
            efference = { enabled = true, granted = true }
            "efference.bash" = { enabled = false }
            "efference.write" = { granted = false }
        "#,
    );
    assert!(!role.tool_enabled("bash"));
    assert!(role.tool_enabled("write"));
    assert!(!role.tools.granted("write"));
    assert!(role.tools.granted("read"));
}

/// A declared server is mounted by its declaration; `[tools]` grants it, or turns tools off.
/// Only the first dot is the boundary: a server's own tool name may contain dots (the MCP spec
/// permits them), and those stay in the wire name.
#[test]
fn a_declared_servers_tools_are_enabled_unless_turned_off() {
    let role = parse_str(
        r#"
            [tools]
            ferritin = { granted = true }
            "ferritin.admin.tools.list" = { enabled = false }

            [mcp.servers.ferritin]
            command = "ferritin"
        "#,
    );
    assert!(role.tool_enabled("ferritin__get"));
    assert!(role.tools.granted("ferritin__get"));
    assert!(!role.tool_enabled("ferritin__admin.tools.list"));
}

/// A loop tool takes a grant like any other tool.
#[test]
fn a_loop_tool_takes_a_grant() {
    let role = parse_str("[tools]\n\"efference.kill_process\" = { granted = true }\n");
    assert!(role.tools.granted("kill_process"));
}

/// A logged grant names a tool by wire name and a namespace by its glob; applying either
/// lands on the entry the file would carry.
#[test]
fn a_logged_grant_applies_to_a_tool_or_a_namespace() {
    let mut role = parse_str("[tools]\nagents = { enabled = true }\n");
    role.tools.grant("agents__status");
    assert!(role.tools.granted("agents__status"));
    assert!(!role.tools.granted("agents__create"));
    role.tools.grant("agents__*");
    assert!(role.tools.granted("agents__create"));
    assert_eq!(
        role.tools.grant_names().into_iter().collect::<Vec<_>>(),
        ["agents__*", "agents__status"]
    );
}

#[test]
fn a_grant_on_a_tool_that_is_not_enabled_fails_the_file() {
    for text in [
        "[tools]\nefference = { granted = true }\n",
        "[tools]\n\"agents.message\" = { granted = true }\n",
        "[tools]\nefference = { enabled = true }\n\"efference.bash\" = { enabled = false, granted = true }\n",
    ] {
        let err = parse_err(text);
        assert!(
            matches!(
                err,
                LoadError::Validate(_, ValidationError::ToolGrantedNotEnabled(_))
            ),
            "{text}: {err}"
        );
    }
}

/// A bare key is a namespace, so one of efference's own tools written bare names nothing —
/// and the message says how to write it.
#[test]
fn a_bare_tool_name_fails_and_names_the_dotted_spelling() {
    let err = parse_err("[tools]\nbash = { enabled = false }\n");
    assert!(
        matches!(
            err,
            LoadError::Validate(_, ValidationError::ToolNamespace(_))
        ),
        "{err}"
    );
    assert!(err.to_string().contains("\"efference.bash\""), "{err}");
}

#[test]
fn an_undeclared_namespace_fails_the_file() {
    for key in ["ghost", "\"ghost.lookup\"", "\"efference.\""] {
        let err = parse_err(&format!("[tools]\n{key} = {{ enabled = true }}\n"));
        assert!(
            matches!(
                err,
                LoadError::Validate(_, ValidationError::ToolNamespace(_))
            ),
            "{key}: {err}"
        );
    }
}

/// A whole namespace is its bare key; a key is not a pattern.
#[test]
fn a_star_in_a_key_fails_the_file() {
    for key in ["\"efference.*\"", "\"ferritin.ge*\"", "\"*\""] {
        let err = parse_err(&format!("[tools]\n{key} = {{ enabled = true }}\n"));
        assert!(
            matches!(err, LoadError::Validate(_, ValidationError::ToolGlob(_))),
            "{key}: {err}"
        );
    }
    let err = parse_err("[tools]\n\"efference.*\" = { enabled = true }\n");
    assert!(err.to_string().contains("(`efference`)"), "{err}");
}

/// Turning off a tool the loop relies on would leave the loop asking for a tool the session
/// does not have.
#[test]
fn enabled_on_a_loop_tool_fails_the_file() {
    let err = parse_err("[tools]\n\"efference.consolidate\" = { enabled = false }\n");
    assert!(
        matches!(
            err,
            LoadError::Validate(_, ValidationError::ToolAlwaysPresent(_))
        ),
        "{err}"
    );
}

/// Unquoted, `agents.message` is a TOML dotted key: it nests as `agents = { message = … }`,
/// and the entry names the stray key with the fix.
#[test]
fn an_unquoted_dotted_key_names_the_quoting_fix() {
    let err = parse_err("[tools]\nagents.message = { granted = true }\n").to_string();
    assert!(err.contains("unknown key `message`"), "{err}");
    assert!(err.contains("\"<namespace>.message\""), "{err}");
}

/// The old list form is gone; it fails the file rather than granting nothing.
#[test]
fn the_list_form_fails_the_file() {
    assert!(matches!(
        parse_err("tools = [\"read\"]\n"),
        LoadError::Parse(..)
    ));
}

/// A declared server may not take one of efference's own namespaces, whose wire names it would
/// share.
#[test]
fn a_server_named_for_a_builtin_namespace_fails_the_file() {
    for name in ["efference", "agents", "lsp"] {
        let err = parse_err(&format!("[mcp.servers.{name}]\ncommand = \"x\"\n"));
        assert!(
            matches!(
                err,
                LoadError::Validate(_, ValidationError::McpServerReserved(_))
            ),
            "{name}: {err}"
        );
    }
}
