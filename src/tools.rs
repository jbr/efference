//! The tool layer (docs/tools.md) — how the agent loop discovers and dispatches tools.
//!
//! A **backend** exposes one or more tools. `Bash` is one tool; an in-process MCP
//! server (docs/tools.md "MCP integration") exposes many behind one shared state. So a
//! backend advertises a *set* of wire definitions and answers `handles` for
//! routing.
//!
//! Modeled as an enum ([`Provider`]) aggregated by a [`Toolbox`], rather than a
//! boxed `async` trait object: dispatch stays static (no `async-trait` crate, no
//! boxing), it matches the enum-centric style of the rest of the codebase, and a
//! new backend is a single variant. The docs/tools.md "in-process `Tool` trait" is realized
//! as this enum boundary — the loop knows only `Toolbox`, never a concrete tool.
//!
//! The mounted set is fixed for the session (chosen once in `main`), so the
//! request's `tools[]` is byte-stable and never perturbs the tools cache tier
//! (docs/log-and-projection.md).

use std::cell::RefCell;
use std::future::Future;
use std::marker::PhantomData;
use std::path::PathBuf;

use fs_mcp::FsTools;
use mcplease::traits::{AsToolsList, Dispatch};
use mcplease::types::{RequestContext, Tool as ToolDefinition};
use semantic_edit_mcp::SemanticEditTools;
use serde_json::{Value, json};

use crate::bash::{self, Bash, Outcome};
use crate::frontend::Interrupt;
use crate::log::{RawResult, ToolCall};

/// A tool backend. Each variant owns whatever state its tools need; `Bash` is
/// stateless, an MCP server holds its session state (added as its own variant
/// when integrated).
pub enum Provider {
    Bash(Bash),
    // The MCP servers are boxed: their session state makes them far larger than
    // `Bash`, and an unboxed enum would size every `Provider` to the largest
    // variant (clippy::large_enum_variant). Method calls auto-deref through the box.
    Fs(Box<McpServer<FsTools, fs_mcp::Tools>>),
    SemanticEdit(Box<McpServer<SemanticEditTools, semantic_edit_mcp::Tools>>),
    /// A connected provider (docs/tools.md): a user-declared server reached
    /// over stdio or HTTP, its tool table discovered at mount.
    Connected(Box<crate::mcp::ConnectedProvider>),
}

impl Provider {
    /// This backend's wire tool definitions (`tools[]` entries).
    fn definitions(&self) -> Vec<Value> {
        match self {
            Provider::Bash(_) => vec![Bash::definition()],
            Provider::Fs(s) => s.definitions.clone(),
            Provider::SemanticEdit(s) => s.definitions.clone(),
            Provider::Connected(c) => c.definitions(),
        }
    }

    /// Whether this backend owns the named tool.
    fn handles(&self, name: &str) -> bool {
        match self {
            Provider::Bash(_) => name == bash::NAME,
            Provider::Fs(s) => s.handles(name),
            Provider::SemanticEdit(s) => s.handles(name),
            Provider::Connected(c) => c.handles(name),
        }
    }

    /// The spotter's brief for a tool this backend owns. `Bash` has none: a bash
    /// move is judged as its command text, not as a tool schema
    /// (`spotter::render_move`).
    fn brief(&self, name: &str) -> Option<ToolBrief<'_>> {
        match self {
            Provider::Bash(_) => None,
            Provider::Fs(s) => s.schema(name).map(|definition| ToolBrief {
                server: None,
                definition,
            }),
            Provider::SemanticEdit(s) => s.schema(name).map(|definition| ToolBrief {
                server: None,
                definition,
            }),
            Provider::Connected(c) => c.brief(name),
        }
    }

    /// Run a call this backend handles. `abort` is the loop's interrupt future,
    /// resolving with the operator's intent (kill or detach); backends that
    /// can't be interrupted ignore it. `on_spawn` fires with the child's pid when
    /// the backend spawns an OS process (only bash does — the daemon registers it
    /// in the supervisor's process namespace, docs/supervisor.md). `on_output` is
    /// the live-stream sink (docs/README.md) — only the long-running, interruptible
    /// bash tool streams; the MCP tools are synchronous and resolve whole as
    /// [`Outcome::Done`].
    async fn run(
        &self,
        call: &ToolCall,
        abort: impl Future<Output = Interrupt>,
        on_spawn: impl FnOnce(Option<u32>),
        on_output: impl FnMut(&[u8]),
    ) -> Outcome {
        match self {
            Provider::Bash(b) => b.run(&call.input, abort, on_spawn, on_output).await,
            // Synchronous and fast — no await, so these ignore `abort`, spawn no
            // process, and stream nothing; their result lands whole as one
            // committed entry.
            Provider::Fs(s) => s.run_call(&call.name, &call.input).into(),
            Provider::SemanticEdit(s) => s.run_call(&call.name, &call.input).into(),
            // Async over a wire, but no OS process of the *call's* own and no
            // live stream: the result lands whole. Races `abort` internally
            // (Kill abandons the call; Detach is bash-only and keeps waiting).
            Provider::Connected(c) => c.run(call, abort).await,
        }
    }
}

/// An in-process MCP server (docs/tools.md) adapted to the toolbox, generic over the
/// server's session `State` and its generated `mcplease` `ToolEnum` dispatcher.
/// Holds the state behind a `RefCell` — the loop is single-threaded and
/// `mcplease`'s `Dispatch::call` is synchronous (`&mut state`), so the borrow never spans
/// an `.await` — plus the precomputed Anthropic tool definitions and the names it
/// answers for. fs-mcp and semantic-edit-mcp are two instantiations.
pub struct McpServer<State, ToolEnum> {
    state: RefCell<State>,
    definitions: Vec<Value>,
    /// The mounted tool table, aligned with `definitions`. Kept whole (not just
    /// the names) because `anthropic_def` drops what the wire has no slot for —
    /// the annotations and the title — and the spotter reads those
    /// ([`ToolBrief`], docs/permissions.md).
    schemas: Vec<ToolDefinition>,
    _tools: PhantomData<fn() -> ToolEnum>,
}

impl<State, ToolEnum> McpServer<State, ToolEnum>
where
    // `Dispatch<State>`'s supertrait gives `DeserializeOwned`, so `from_value` works.
    ToolEnum: AsToolsList + Dispatch<State>,
{
    /// Adapt an already-constructed server state, advertising every tool the
    /// generated enum exposes **except** those named in `hidden` — the curation
    /// point for which of a server's tools reach the wire (docs/tools.md taxonomy).
    /// The tool schemas are built once here (pure; `serde_json::to_value` sorts
    /// keys, so the defs are byte-stable for the session — docs/log-and-projection.md) and reused as the
    /// wire `tools[]`.
    fn from_state(state: State, hidden: &[&str]) -> Self {
        let schemas: Vec<ToolDefinition> = <ToolEnum as AsToolsList>::tools_list()
            .into_iter()
            .filter(|s| !hidden.contains(&s.name.as_str()))
            .collect();
        let definitions = schemas.iter().map(anthropic_def).collect();
        Self {
            state: RefCell::new(state),
            definitions,
            schemas,
            _tools: PhantomData,
        }
    }

    fn handles(&self, name: &str) -> bool {
        self.schemas.iter().any(|s| s.name == name)
    }

    /// This server's entry for `name`, for the spotter's brief.
    fn schema(&self, name: &str) -> Option<&ToolDefinition> {
        self.schemas.iter().find(|s| s.name == name)
    }

    /// Dispatch one call through the generated `ToolEnum`: wrap as
    /// `{name, arguments}`, deserialize to the typed tool, and `call` it
    /// against the shared state. The `CallToolResult` is folded to text by the
    /// same renderer the connected providers use (docs/tools.md), so an
    /// embedded tool that grows a structured or multimodal result reads the
    /// same way as a remote one. Any failure (bad input, tool error) becomes an
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
        match tool.call(&mut self.state.borrow_mut(), &request_context()) {
            Ok(result) => crate::mcp::render_result(result),
            Err(e) => RawResult {
                content: e.to_string(),
                is_error: true,
            },
        }
    }
}

/// What an embedded server learns about its caller. Over a wire this rides in
/// each request's `_meta` (protocol revision 2026-07-28); in-process there is
/// no wire, so the same context is handed to `call` directly. efference
/// declares no optional client capabilities — notably not elicitation, so a
/// tool must not ask for more input mid-call.
fn request_context() -> RequestContext {
    RequestContext {
        protocol_version: Some(mcplease::types::LATEST_PROTOCOL_VERSION.to_string()),
        client_info: Some(mcplease::types::Implementation::new(
            "efference",
            env!("CARGO_PKG_VERSION"),
        )),
        client_capabilities: Default::default(),
    }
}

/// What the spotter is handed about a structured tool before it judges a call
/// (docs/permissions.md). Borrowed out of the mounted tool table, which is fixed
/// for the session.
///
/// The wire `tools[]` entry is not enough on its own: [`anthropic_def`] keeps
/// only `name` / `description` / `input_schema`, because those are the only
/// fields the Anthropic tool shape has. A server's own behavior hints
/// (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`) and its
/// `title` are dropped there, and they are exactly what a permission rung wants —
/// so the brief reads the `ToolDefinition` the mount snapshotted instead.
pub struct ToolBrief<'a> {
    /// The connected server that advertised this tool; `None` for an embedded
    /// one. The spotter is told which, because "a tool this harness ships" and
    /// "a tool a declared remote server offers" carry different priors.
    pub server: Option<&'a str>,
    /// The mounted entry, namespaced — `name` is the wire name.
    pub definition: &'a ToolDefinition,
}

/// Translate an MCP tool definition (`inputSchema`, camelCase) into the
/// Anthropic `tools[]` shape (`input_schema`, snake_case). The schema is
/// carried as a raw `serde_json::Value`, whose object maps keep keys in
/// sorted order, so re-serializing an identical definition is
/// byte-deterministic — safe for the tools cache tier
/// (docs/log-and-projection.md).
pub(crate) fn anthropic_def(schema: &ToolDefinition) -> Value {
    let mut def = serde_json::Map::new();
    def.insert("name".to_string(), Value::String(schema.name.clone()));
    if let Some(description) = &schema.description {
        def.insert(
            "description".to_string(),
            Value::String(description.clone()),
        );
    }
    def.insert("input_schema".to_string(), schema.input_schema.clone());
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
        Self::new(vec![Provider::Bash(Bash::default())])
    }

    /// Bash plus the in-process MCP servers — fs-mcp (read / search / write /
    /// list) and semantic-edit-mcp (preview / retarget / persist tree-sitter
    /// edits) — each with its session cwd seeded to `cwd` and an in-memory session
    /// store (no shared `~/.ai-tools` writes). The agent gets real file +
    /// structured-edit tools that lean on the fork instead of piping to
    /// `head`/`grep` (docs/redaction-fork.md, docs/tools.md). `scratch` is the
    /// session scratch dir bash exports as `$TMPDIR` (docs/tools.md); `None` for
    /// contexts that never execute (the `wire` dev view).
    pub fn bash_and_mcps(cwd: PathBuf, scratch: Option<PathBuf>) -> anyhow::Result<Self> {
        Ok(Self::new(vec![
            Provider::Bash(Bash { tmpdir: scratch }),
            // fs-mcp curated down to the tools whose structured, fork-friendly
            // output genuinely beats the shell. `set_working_directory` is managed
            // by efference (the cwd is seeded). `move`/`delete` are dropped: they
            // earn nothing over bash's `mv`/`rm` — even Haiku reaches for those —
            // so they aren't worth the tool-doc tokens (docs/tools.md taxonomy).
            // `list` survives because it is gitignore-aware (skips `target/`,
            // `.git/`), which `ls`/`find` are not.
            Provider::Fs(Box::new(McpServer::from_state(
                FsTools::embedded(cwd.clone())?,
                &["set_working_directory", "move", "delete"],
            ))),
            // semantic-edit-mcp: only `set_working_directory` is hidden (both
            // servers expose it, and a duplicate name on the wire is a 400).
            Provider::SemanticEdit(Box::new(McpServer::from_state(
                SemanticEditTools::embedded(cwd)?,
                &["set_working_directory"],
            ))),
        ]))
    }

    /// Mount one user-declared connected provider (docs/tools.md): connect,
    /// handshake, snapshot its tool table, and add it to the mounted set.
    /// Namespacing (`<server>__<tool>`) keeps its names disjoint from every
    /// other provider's, so the unique-names invariant holds by construction.
    /// Call before the first projection — the mounted set must be fixed for
    /// the session (docs/log-and-projection.md).
    pub async fn mount_connected(
        &mut self,
        name: &str,
        config: &crate::config::McpTransport,
        cwd: &std::path::Path,
    ) -> anyhow::Result<()> {
        let provider = crate::mcp::ConnectedProvider::mount(name, config, cwd).await?;
        self.providers.push(Provider::Connected(Box::new(provider)));
        Ok(())
    }

    /// The session scratch dir the bash provider exports as `$TMPDIR`, if any.
    /// The daemon reads it here to seed the path expander and the background
    /// spawn env — one source of truth: the value the toolbox was built with
    /// (docs/tools.md).
    pub fn scratch_dir(&self) -> Option<&std::path::Path> {
        self.providers.iter().find_map(|p| match p {
            Provider::Bash(b) => b.tmpdir.as_deref(),
            _ => None,
        })
    }

    /// All tool definitions, in mount order — the request's `tools[]`.
    pub fn definitions(&self) -> Vec<Value> {
        self.providers
            .iter()
            .flat_map(Provider::definitions)
            .collect()
    }

    /// The spotter's brief for a structured tool (docs/permissions.md): what the
    /// mount snapshotted about it, so the rung that judges an unfamiliar tool sees
    /// more than a name and a JSON blob. `None` for bash and for a name no backend
    /// owns.
    pub fn brief(&self, name: &str) -> Option<ToolBrief<'_>> {
        self.providers.iter().find_map(|p| p.brief(name))
    }

    /// Route a call to the backend that handles its name. An unknown name comes
    /// back as an error result (never a panic) so the assistant `tool_use` still
    /// gets exactly one paired `tool_result` — the API rejects an orphan.
    pub async fn run(
        &self,
        call: &ToolCall,
        abort: impl Future<Output = Interrupt>,
        on_spawn: impl FnOnce(Option<u32>),
        on_output: impl FnMut(&[u8]),
    ) -> Outcome {
        match self.providers.iter().find(|p| p.handles(&call.name)) {
            Some(provider) => provider.run(call, abort, on_spawn, on_output).await,
            None => RawResult {
                content: format!("unknown tool: {}", call.name),
                is_error: true,
            }
            .into(),
        }
    }
}

#[cfg(test)]
mod test;
