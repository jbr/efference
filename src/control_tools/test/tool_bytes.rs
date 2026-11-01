//! The bytes each agent surface advertises its control tools in (docs/tools.md), pinned
//! whole: the `agents__*` definitions a hosted session's `tools[]` carries, the in-session
//! set's tool table (titles and annotations included, which the wire form drops), and the
//! `tools` of `efference mcp`'s `tools/list` answer. Each reaches a model in its cached
//! prompt prefix, so a byte moved here moves the cache of every session that mounts the
//! verbs (docs/log-and-projection.md). It fails here first; a deliberate change updates the
//! pin with `EFFERENCE_WRITE_PINS=1 cargo test tool_bytes`, and says so in its commit.
//!
//! A pin file holds one tool per line, each line exactly the bytes that tool takes in the
//! array, in advertised order: the array is the lines joined with commas inside brackets.

use super::super::*;
use mcplease::traits::AsToolsList;
use serde_json::Value;
use std::path::Path;

/// The pin files, relative to the crate root.
const DIR: &str = "src/control_tools/test/tool_bytes";

/// `array`, a serialized tool array, is the pin `name` byte for byte.
fn pinned(name: &str, array: &Value) {
    let tools = array.as_array().expect("a tool array");
    let lines: Vec<String> = tools.iter().map(Value::to_string).collect();
    // The lines are the array's own bytes, element by element.
    assert_eq!(format!("[{}]", lines.join(",")), array.to_string());
    let generated: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR).join(name);
    if std::env::var_os("EFFERENCE_WRITE_PINS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    if committed == generated {
        return;
    }
    let differing = committed
        .lines()
        .zip(generated.lines())
        .position(|(was, now)| was != now)
        .unwrap_or_else(|| committed.lines().count().min(generated.lines().count()));
    panic!(
        "{} moved at tool {}: these bytes are in the cached prefix of every session that \
         mounts the verbs.\npinned:    {}\ngenerated: {}",
        path.display(),
        differing + 1,
        committed.lines().nth(differing).unwrap_or("(nothing)"),
        generated.lines().nth(differing).unwrap_or("(nothing)"),
    );
}

/// The `agents__*` definitions as a hosted session's request carries them: namespaced,
/// `intent` injected, in mount order (docs/tools.md).
#[test]
fn the_in_session_tools_array_is_pinned() {
    let host = crate::daemon::Host::new(
        crate::inference::InferenceRegistry::empty(),
        crate::config::env_table::EnvSnapshot::new(Default::default())
            .expect("an empty environment is valid"),
    );
    let mut toolbox = crate::tools::Toolbox::new(Vec::new());
    toolbox.mount_sessions(Control::in_session(&host, "caller".into()));
    pinned(
        "in_session_wire.jsonl",
        &Value::Array(toolbox.definitions()),
    );
}

/// The in-session set's own tool table: what the spotter's brief and the tool route read,
/// title and annotations included.
#[test]
fn the_in_session_tool_table_is_pinned() {
    let tools = serde_json::to_value(in_session::Tools::tools_list()).unwrap();
    pinned("in_session.jsonl", &tools);
}

/// `efference mcp`'s `tools/list`, through mcplease's dispatch: the tools a mounting
/// harness caches and hands its model.
#[tokio::test]
async fn the_mcp_tools_list_is_pinned() {
    let response = super::mcp_call("tools/list", serde_json::json!({})).await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.expect("a result");
    pinned("mcp.jsonl", &result["tools"]);
}
