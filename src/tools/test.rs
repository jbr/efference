use super::*;
use crate::log::CallId;
use serde_json::json;
use std::future::{pending, ready};

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: CallId("toolu_1".into()),
        name: name.into(),
        input,
    }
}

#[test]
fn bash_only_advertises_exactly_the_bash_tool() {
    let defs = Toolbox::bash_only().definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0]["name"], json!(bash::NAME));
}

fn done(outcome: Outcome) -> RawResult {
    match outcome {
        Outcome::Done(result) => result,
        Outcome::Detached(_) => panic!("expected a completed result, got a detach"),
    }
}

#[tokio::test]
async fn routes_a_bash_call_to_the_bash_backend() {
    let box_ = Toolbox::bash_only();
    let result = done(
        box_.run(
            &call(bash::NAME, json!({"command": "echo routed"})),
            pending(),
            |_| {},
        )
        .await,
    );
    assert!(!result.is_error);
    assert!(
        result.content.contains("routed"),
        "got {:?}",
        result.content
    );
}

#[tokio::test]
async fn unknown_tool_is_an_error_result_not_a_panic() {
    let box_ = Toolbox::bash_only();
    // An already-resolved abort proves the unknown path never touches it.
    let result = done(
        box_.run(&call("nope", json!({})), ready(Interrupt::Kill), |_| {})
            .await,
    );
    assert!(result.is_error);
    assert!(
        result.content.contains("unknown tool: nope"),
        "got {:?}",
        result.content
    );
}

fn fs_server(cwd: PathBuf) -> McpServer<FsTools, fs_mcp::Tools> {
    McpServer::from_state(FsTools::embedded(cwd).unwrap(), &["set_working_directory"])
}

/// fs-mcp's schemas translate to the Anthropic `tools[]` shape. Uses
/// `tools_list()`, which is pure (schema generation) — hermetic, no store.
#[test]
fn fs_schemas_translate_to_anthropic_tool_defs() {
    let schemas = <fs_mcp::Tools as AsToolsList>::tools_list();
    let defs: Vec<Value> = schemas.iter().map(anthropic_def).collect();

    let names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    for expected in ["read", "search", "write", "list"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    for def in &defs {
        // Anthropic shape: snake_case `input_schema`, an object schema, and no
        // leftover MCP camelCase `inputSchema`.
        assert!(def.get("inputSchema").is_none(), "camelCase leaked: {def}");
        assert_eq!(def["input_schema"]["type"], json!("object"), "{def}");
    }
}

/// End-to-end execute path: read and list real files through the fs server.
/// Hermetic — `FsTools::embedded` uses an **in-memory** session store, so this
/// never touches the shared `~/.ai-tools` store; only the temp file is on disk.
#[test]
fn fs_read_and_list_roundtrip() {
    let dir = std::env::temp_dir().join(format!("eff-fsrt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hello.txt"), "fs-mcp lives").unwrap();

    let server = fs_server(dir.clone());

    // `read` resolves the relative path against the seeded session cwd.
    let read = server.run_call("read", &json!({ "paths": ["hello.txt"] }));
    assert!(!read.is_error, "read errored: {:?}", read.content);
    assert!(
        read.content.contains("fs-mcp lives"),
        "got {:?}",
        read.content
    );

    // `list` finds the file in the seeded directory.
    let list = server.run_call("list", &json!({ "path": "." }));
    assert!(!list.is_error, "list errored: {:?}", list.content);
    assert!(list.content.contains("hello.txt"), "got {:?}", list.content);

    // An unknown tool name routed here is a clean error, not a panic.
    let bad = server.run_call("read", &json!({ "wrong": true }));
    assert!(bad.is_error, "bad input should error: {:?}", bad.content);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// semantic-edit-mcp wires up through the same generic `McpServer`: its
/// schemas translate, an embedded instance (in-memory stores + tree-sitter
/// language registry) advertises the edit tools, and bad input is a clean
/// error. Hermetic — no shared store, no real edit performed.
#[test]
fn semantic_edit_server_advertises_its_edit_tools() {
    let dir = std::env::temp_dir().join(format!("eff-se-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let server: McpServer<SemanticEditTools, semantic_edit_mcp::Tools> = McpServer::from_state(
        SemanticEditTools::embedded(dir.clone()).unwrap(),
        &["set_working_directory"],
    );

    for expected in ["edit", "retarget_edit", "undo_edit"] {
        assert!(
            server.handles(expected),
            "missing {expected} in {:?}",
            server.names
        );
    }
    let bad = server.run_call("edit", &json!({ "nonsense": true }));
    assert!(bad.is_error, "bad input should error: {:?}", bad.content);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The fully-mounted toolbox must advertise **unique** tool names — both MCP
/// servers expose `set_working_directory`, which would collide on the wire (the
/// API rejects duplicate names). It's filtered; the real tools survive.
#[test]
fn mounted_toolbox_advertises_unique_tool_names() {
    let dir = std::env::temp_dir().join(format!("eff-uniq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let defs = Toolbox::bash_and_mcps(dir.clone(), None)
        .unwrap()
        .definitions();
    let mut names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(names, unique, "duplicate tool name on the wire");

    // `set_working_directory` collides; `move`/`delete` are curated out (bash
    // does them as well — docs/tools.md). The fork-friendly fs tools survive.
    for hidden in ["set_working_directory", "move", "delete"] {
        assert!(!names.contains(&hidden), "should be filtered: {names:?}");
    }
    for tool in [
        "bash",
        "read",
        "search",
        "write",
        "list",
        "edit",
        "undo_edit",
    ] {
        assert!(names.contains(&tool), "missing {tool} in {names:?}");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
