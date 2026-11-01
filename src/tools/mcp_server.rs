//! An embedded MCP server mounted in-process (docs/tools.md).

use super::*;

impl<State, ToolEnum> McpServer<State, ToolEnum>
where
    // `Dispatch<State>`'s supertrait gives `DeserializeOwned`, so `from_value` works.
    ToolEnum: AsToolsList + Dispatch<State>,
    State: Clone + Send + Sync + 'static,
{
    /// Adapt an already-constructed server state, advertising every tool the
    /// generated enum exposes **except** those named in `hidden` — the curation
    /// point for which of a server's tools reach the wire (docs/tools.md taxonomy).
    /// The tool schemas are built once here (pure; `serde_json::to_value` sorts
    /// keys, so the defs are byte-stable for the session — docs/log-and-projection.md) and reused as the
    /// wire `tools[]`.
    pub(super) fn from_state(state: State, hidden: &[&str]) -> Self {
        Self::from_state_named(state, hidden, None)
    }

    /// [`from_state`](Self::from_state) mounted under a wire-name namespace
    /// (`<server>__<tool>`, docs/tools.md): the definitions are rewritten to the
    /// namespaced form and dispatch strips the prefix back off. The namespace is
    /// fixed for the session, so the definitions stay byte-stable
    /// (docs/log-and-projection.md).
    pub(super) fn from_state_namespaced(state: State, hidden: &[&str], namespace: &str) -> Self {
        Self::from_state_named(state, hidden, Some(namespace))
    }

    pub(super) fn from_state_named(state: State, hidden: &[&str], namespace: Option<&str>) -> Self {
        let schemas: Vec<ToolDefinition> = <ToolEnum as AsToolsList>::tools_list()
            .into_iter()
            .filter(|s| !hidden.contains(&s.name.as_str()))
            .collect();
        let definitions = schemas
            .iter()
            .map(|schema| {
                let mut definition = anthropic_def(schema);
                if let Some(namespace) = namespace {
                    definition["name"] = Value::String(format!("{namespace}__{}", schema.name));
                }
                definition
            })
            .collect();
        Self {
            state,
            definitions,
            schemas,
            namespace: namespace.map(str::to_string),
            _tools: PhantomData,
        }
    }

    /// Drop the tools `enabled` refuses by wire name, keeping `schemas` and
    /// `definitions` aligned; whether any remain.
    pub(super) fn retain_enabled(&mut self, enabled: &impl Fn(&str) -> bool) -> bool {
        let keep: Vec<bool> = self
            .definitions
            .iter()
            .map(|d| d.get("name").and_then(Value::as_str).is_some_and(enabled))
            .collect();
        let mut flags = keep.iter();
        self.schemas.retain(|_| *flags.next().unwrap_or(&false));
        let mut flags = keep.iter();
        self.definitions.retain(|_| *flags.next().unwrap_or(&false));
        !self.definitions.is_empty()
    }

    /// The bare tool name behind a wire name: strips this server's namespace
    /// when mounted namespaced, identity otherwise. `None` when the name does
    /// not carry this server's prefix.
    pub(super) fn bare_name<'a>(&self, name: &'a str) -> Option<&'a str> {
        match &self.namespace {
            Some(namespace) => name
                .strip_prefix(namespace.as_str())
                .and_then(|rest| rest.strip_prefix("__")),
            None => Some(name),
        }
    }

    pub(super) fn handles(&self, name: &str) -> bool {
        self.bare_name(name)
            .is_some_and(|bare| self.schemas.iter().any(|s| s.name == bare))
    }

    /// This server's entry for `name`, for the spotter's brief.
    pub(super) fn schema(&self, name: &str) -> Option<&ToolDefinition> {
        let bare = self.bare_name(name)?;
        self.schemas.iter().find(|s| s.name == bare)
    }

    /// Dispatch one call through the generated `ToolEnum`: wrap as
    /// `{name, arguments}`, deserialize to the typed tool, and `call` it
    /// against the shared state. The `CallToolResult` is folded to text by the
    /// same renderer the connected providers use (docs/tools.md), so an
    /// embedded tool that grows a structured or multimodal result reads the
    /// same way as a remote one. Any failure (bad input, tool error) becomes an
    /// `is_error` result so the `tool_use` still gets its pair.
    pub(super) async fn run_call(&self, name: &str, input: &Value) -> RawResult {
        let Some(bare) = self.bare_name(name) else {
            return RawResult::error(format!("unknown tool: {name}"));
        };
        let envelope = json!({ "name": bare, "arguments": input });
        let tool: ToolEnum = match serde_json::from_value(envelope) {
            Ok(tool) => tool,
            Err(e) => {
                return RawResult::error(format!(
                    "invalid input for {name}: {e} (received: {})",
                    received(input)
                ));
            }
        };
        match tool.call(self.state.clone(), request_context()).await {
            Ok(result) => mcp::render_result(&result),
            Err(e) => RawResult::error(e.to_string()),
        }
    }
}

/// What a rejected input carried, for the rejection text (docs/tools.md): an
/// object's top-level keys, sorted, or what the value was when it is not an
/// object. The deserializer's message says what was expected; this says what was
/// sent, so a call that misnamed a field reads against its own keys.
fn received(input: &Value) -> String {
    match input {
        Value::Object(map) if map.is_empty() => "no keys".to_string(),
        Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            keys.join(", ")
        }
        Value::String(_) => "a string".to_string(),
        Value::Number(_) => "a number".to_string(),
        Value::Bool(_) => "a boolean".to_string(),
        Value::Array(_) => "an array".to_string(),
        Value::Null => "null".to_string(),
    }
}

/// What an embedded server learns about its caller. Over a wire this rides in
/// each request's `_meta` (protocol revision 2026-07-28); in-process there is
/// no wire, so the same `_meta` is read here and handed to `call` directly.
/// efference declares no optional client capabilities — notably not
/// elicitation, so a tool must not ask for more input mid-call.
fn request_context() -> RequestContext {
    use mcplease::types::{Implementation, LATEST_PROTOCOL_VERSION, meta_keys};
    RequestContext::from_params(Some(&json!({
        "_meta": {
            meta_keys::PROTOCOL_VERSION: LATEST_PROTOCOL_VERSION,
            meta_keys::CLIENT_INFO: Implementation::new("efference", env!("CARGO_PKG_VERSION")),
            meta_keys::CLIENT_CAPABILITIES: {},
        }
    })))
}
