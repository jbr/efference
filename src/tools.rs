//! The tool seam (DESIGN §5) — how the agent loop discovers and dispatches tools.
//!
//! A **backend** exposes one or more tools. `Bash` is one tool; an in-process MCP
//! server (DESIGN §5 "MCP integration") exposes many behind one shared state. So a
//! backend advertises a *set* of wire definitions and answers `handles` for
//! routing.
//!
//! Modeled as an enum ([`Provider`]) aggregated by a [`Toolbox`], rather than a
//! boxed `async` trait object: dispatch stays static (no `async-trait` crate, no
//! boxing), it matches the enum-centric style of the rest of the codebase, and a
//! new backend is a single variant. The §5 "in-process `Tool` trait" is realized
//! as this enum boundary — the loop knows only `Toolbox`, never a concrete tool.
//!
//! The mounted set is fixed for the session (chosen once in `main`), so the
//! request's `tools[]` is byte-stable and never perturbs the tools cache tier
//! (DESIGN §3).

use std::cell::RefCell;
use std::future::Future;
use std::marker::PhantomData;
use std::path::PathBuf;

use fs_mcp::FsTools;
use mcplease::traits::{AsToolsList, Tool as McpTool};
use mcplease::types::ToolSchema;
use semantic_edit_mcp::SemanticEditTools;
use serde_json::{Value, json};

use crate::bash::{self, Bash};
use crate::log::{RawResult, ToolCall};

/// A tool backend. Each variant owns whatever state its tools need; `Bash` is
/// stateless, an MCP server holds its session state (added as its own variant
/// when integrated).
pub enum Provider {
    Bash(Bash),
    Fs(McpServer<FsTools, fs_mcp::Tools>),
    SemanticEdit(McpServer<SemanticEditTools, semantic_edit_mcp::Tools>),
}

impl Provider {
    /// This backend's wire tool definitions (`tools[]` entries).
    fn definitions(&self) -> Vec<Value> {
        match self {
            Provider::Bash(_) => vec![Bash::definition()],
            Provider::Fs(s) => s.definitions.clone(),
            Provider::SemanticEdit(s) => s.definitions.clone(),
        }
    }

    /// Whether this backend owns the named tool.
    fn handles(&self, name: &str) -> bool {
        match self {
            Provider::Bash(_) => name == bash::NAME,
            Provider::Fs(s) => s.handles(name),
            Provider::SemanticEdit(s) => s.handles(name),
        }
    }

    /// Run a call this backend handles. `abort` is the loop's interrupt future
    /// (SIGINT → kill); backends that can't be interrupted ignore it.
    async fn run(&self, call: &ToolCall, abort: impl Future<Output = ()>) -> RawResult {
        match self {
            Provider::Bash(b) => b.run(&call.input, abort).await,
            // Synchronous and fast — no await, so these ignore `abort`.
            Provider::Fs(s) => s.run_call(&call.name, &call.input),
            Provider::SemanticEdit(s) => s.run_call(&call.name, &call.input),
        }
    }
}

/// An in-process MCP server (DESIGN §5) adapted to the tool seam, generic over the
/// server's session `State` and its generated `mcplease` `ToolEnum` dispatcher.
/// Holds the state behind a `RefCell` — the loop is single-threaded and
/// `mcplease`'s `execute` is synchronous (`&mut state`), so the borrow never spans
/// an `.await` — plus the precomputed Anthropic tool definitions and the names it
/// answers for. fs-mcp and semantic-edit-mcp are two instantiations.
pub struct McpServer<State, ToolEnum> {
    state: RefCell<State>,
    definitions: Vec<Value>,
    names: Vec<String>,
    _tools: PhantomData<fn() -> ToolEnum>,
}

impl<State, ToolEnum> McpServer<State, ToolEnum>
where
    // `Tool<State>`'s supertrait gives `DeserializeOwned`, so `from_value` works.
    ToolEnum: AsToolsList + McpTool<State>,
{
    /// Adapt an already-constructed server state. The tool schemas are built once
    /// here (pure; `serde_json::to_value` sorts keys, so the defs are byte-stable
    /// for the session — §3) and reused as the wire `tools[]`.
    fn from_state(state: State) -> Self {
        // Tools efference manages itself and must not advertise. `set_working_
        // directory` in particular: efference seeds the cwd via `embedded`, AND
        // both MCP servers expose it — advertising both would put a duplicate tool
        // name on the wire, which the API rejects.
        const HIDDEN: &[&str] = &["set_working_directory"];
        let schemas: Vec<ToolSchema> = <ToolEnum as AsToolsList>::tools_list()
            .into_iter()
            .filter(|s| !HIDDEN.contains(&s.name.as_str()))
            .collect();
        let names = schemas.iter().map(|s| s.name.clone()).collect();
        let definitions = schemas.iter().map(anthropic_def).collect();
        Self {
            state: RefCell::new(state),
            definitions,
            names,
            _tools: PhantomData,
        }
    }

    fn handles(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// Dispatch one call through the generated `ToolEnum`: wrap as
    /// `{name, arguments}`, deserialize to the typed tool, and `execute` it
    /// against the shared state. Any failure (bad input, tool error) becomes an
    /// `is_error` result so the `tool_use` still gets its pair.
    fn run_call(&self, name: &str, input: &Value) -> RawResult {
        let envelope = json!({ "name": name, "arguments": input });
        let tool: ToolEnum = match serde_json::from_value(envelope) {
            Ok(tool) => tool,
            Err(e) => {
                return RawResult {
                    content: format!("invalid input for {name}: {e}"),
                    is_error: true,
                };
            }
        };
        match McpTool::execute(tool, &mut self.state.borrow_mut()) {
            Ok(content) => RawResult { content, is_error: false },
            Err(e) => RawResult { content: e.to_string(), is_error: true },
        }
    }
}

/// Translate an mcplease [`ToolSchema`] (MCP `inputSchema`, camelCase) into the
/// Anthropic `tools[]` shape (`input_schema`, snake_case). `serde_json::to_value`
/// normalizes object keys to sorted order, so the result is byte-deterministic —
/// safe for the tools cache tier (DESIGN §3).
fn anthropic_def(schema: &ToolSchema) -> Value {
    let mut def = serde_json::Map::new();
    def.insert("name".to_string(), Value::String(schema.name.clone()));
    if let Some(description) = &schema.description {
        def.insert("description".to_string(), Value::String(description.clone()));
    }
    def.insert(
        "input_schema".to_string(),
        serde_json::to_value(&schema.input_schema).unwrap_or(Value::Null),
    );
    Value::Object(def)
}

/// The mounted set of tool backends. The loop asks it for the wire `tools[]` and
/// routes each `tool_use` to the backend that handles it.
pub struct Toolbox {
    providers: Vec<Provider>,
}

impl Toolbox {
    pub fn new(providers: Vec<Provider>) -> Self {
        Self { providers }
    }

    /// Just bash — the Phase 0 default.
    pub fn bash_only() -> Self {
        Self::new(vec![Provider::Bash(Bash)])
    }

    /// Bash plus the in-process MCP servers — fs-mcp (read / write / list /
    /// search / move / delete) and semantic-edit-mcp (preview / retarget /
    /// persist tree-sitter edits) — each with its session cwd seeded to `cwd` and
    /// an in-memory session store (no shared `~/.ai-tools` writes). The agent gets
    /// real file + structured-edit tools that lean on the fork instead of piping
    /// to `head`/`grep` (DESIGN §4-realized, §5).
    pub fn bash_and_mcps(cwd: PathBuf) -> anyhow::Result<Self> {
        Ok(Self::new(vec![
            Provider::Bash(Bash),
            Provider::Fs(McpServer::from_state(FsTools::embedded(cwd.clone())?)),
            Provider::SemanticEdit(McpServer::from_state(SemanticEditTools::embedded(cwd)?)),
        ]))
    }

    /// All tool definitions, in mount order — the request's `tools[]`.
    pub fn definitions(&self) -> Vec<Value> {
        self.providers.iter().flat_map(Provider::definitions).collect()
    }

    /// Route a call to the backend that handles its name. An unknown name comes
    /// back as an error result (never a panic) so the assistant `tool_use` still
    /// gets exactly one paired `tool_result` — the API rejects an orphan.
    pub async fn run(&self, call: &ToolCall, abort: impl Future<Output = ()>) -> RawResult {
        match self.providers.iter().find(|p| p.handles(&call.name)) {
            Some(provider) => provider.run(call, abort).await,
            None => RawResult {
                content: format!("unknown tool: {}", call.name),
                is_error: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[tokio::test]
    async fn routes_a_bash_call_to_the_bash_backend() {
        let box_ = Toolbox::bash_only();
        let result = box_.run(&call(bash::NAME, json!({"command": "echo routed"})), pending()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("routed"), "got {:?}", result.content);
    }

    #[tokio::test]
    async fn unknown_tool_is_an_error_result_not_a_panic() {
        let box_ = Toolbox::bash_only();
        // An already-resolved abort proves the unknown path never touches it.
        let result = box_.run(&call("nope", json!({})), ready(())).await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown tool: nope"), "got {:?}", result.content);
    }

    fn fs_server(cwd: PathBuf) -> McpServer<FsTools, fs_mcp::Tools> {
        McpServer::from_state(FsTools::embedded(cwd).unwrap())
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
        assert!(read.content.contains("fs-mcp lives"), "got {:?}", read.content);

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

        let server: McpServer<SemanticEditTools, semantic_edit_mcp::Tools> =
            McpServer::from_state(SemanticEditTools::embedded(dir.clone()).unwrap());

        for expected in ["preview_edit", "retarget_edit", "persist_edit"] {
            assert!(server.handles(expected), "missing {expected} in {:?}", server.names);
        }
        let bad = server.run_call("preview_edit", &json!({ "nonsense": true }));
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

        let defs = Toolbox::bash_and_mcps(dir.clone()).unwrap().definitions();
        let mut names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(names, unique, "duplicate tool name on the wire");

        assert!(!names.contains(&"set_working_directory"), "should be filtered: {names:?}");
        for tool in ["bash", "read", "search", "preview_edit", "persist_edit"] {
            assert!(names.contains(&tool), "missing {tool} in {names:?}");
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
