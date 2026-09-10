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

use std::borrow::Cow;
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
use read_image::ReadImage;

/// The property the harness adds to every mounted tool's input schema
/// (docs/tools.md "The `intent` property"): one line of prose saying why the call
/// is being made, generated before the call runs. Stripped from `input` before
/// dispatch, so no tool ever receives it; kept on the logged [`ToolCall`], so it
/// projects to the wire and is available to the harness's own readers.
pub const INTENT: &str = "intent";

/// The `intent` property's schema `description`. Deliberately a pointer, not the
/// guidance: the guidance is stated once in the session preamble
/// (`assets/prompts/session.md`), because repeating a paragraph across every
/// mounted tool costs prefix tokens per tool for no added signal. What must live
/// here is the *declaration* — a property the schema does not declare may not be
/// emittable at all under constrained decoding, and a preamble-only instruction is
/// a soft constraint that decays over a long run.
const INTENT_DESCRIPTION: &str =
    "Why this call, now — one line. See the guidance on `intent` in your instructions.";

/// Whether an `input_schema` already declares an `intent` property of its own.
///
/// A tool that does keeps its own field: the harness must not inject a second
/// meaning onto the same key, nor strip a value the tool is expecting. The check is
/// against the mounted table, which is fixed for the session, so a given name
/// answers the same way at definition time and at dispatch time — the injection and
/// the strip cannot disagree.
fn declares_own_intent(input_schema: &Value) -> bool {
    input_schema
        .get("properties")
        .and_then(|properties| properties.get(INTENT))
        .is_some()
}

/// Add [`INTENT`] to one wire tool definition, unless the tool declares its own.
///
/// Key order stays sorted (`serde_json::Map` is a `BTreeMap` — no `preserve_order`)
/// and `required` is appended to, so an identical mounted set yields byte-identical
/// definitions. The `tools[]` array sits in the cached prefix, so this must hold or
/// every request re-uploads the whole conversation (docs/log-and-projection.md).
fn with_intent(mut definition: Value) -> Value {
    let Some(schema) = definition.get_mut("input_schema") else {
        return definition;
    };
    if declares_own_intent(schema) {
        return definition;
    }
    let Some(schema) = schema.as_object_mut() else {
        return definition;
    };
    schema
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .map(|properties| {
            properties.insert(
                INTENT.to_string(),
                json!({ "type": "string", "description": INTENT_DESCRIPTION }),
            )
        });
    // Prepended, not appended. `required` is semantically a set — order means nothing
    // to the schema — so this is a free coin-flip rather than a known mechanism: it is
    // the only structural place left to say "this one first". `properties` cannot say
    // it, because sorted key order is not ours to choose (see the doc comment above),
    // and whether the model emits `intent` before the other arguments is the
    // difference between a reasoning slot and a label on a decided call
    // (docs/tools.md). The preamble instruction is the lever we are actually relying on.
    if let Some(required) = schema
        .entry("required")
        .or_insert_with(|| json!([]))
        .as_array_mut()
    {
        required.insert(0, Value::String(INTENT.to_string()));
    }
    definition
}

/// A tool backend. Each variant owns whatever state its tools need; `Bash` is
/// stateless, an MCP server holds its session state (added as its own variant
/// when integrated).
pub enum Provider {
    Bash(Bash),
    /// `read_image` (docs/tools.md): pulls an on-disk image into context as a
    /// `tool_result` image block, at a model-chosen resolution.
    ReadImage(ReadImage),
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
            Provider::ReadImage(_) => vec![ReadImage::definition()],
            Provider::Fs(s) => s.definitions.clone(),
            Provider::SemanticEdit(s) => s.definitions.clone(),
            Provider::Connected(c) => c.definitions(),
        }
    }

    /// Whether this backend owns the named tool.
    fn handles(&self, name: &str) -> bool {
        match self {
            Provider::Bash(_) => name == bash::NAME,
            Provider::ReadImage(_) => name == read_image::NAME,
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
            // No brief yet either: nearly every call clears on a path grant, and
            // the spotter's structured-move render shows the input verbatim.
            Provider::ReadImage(_) => None,
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

    /// The call as the *tool* should see it: [`INTENT`] removed, since the harness
    /// put it there and the tool's schema never asked for it (docs/tools.md).
    /// Borrows unless there is something to remove — the common shape is one clone
    /// of a small `Value` per call.
    ///
    /// A tool that declares its own `intent` keeps the field: it is the tool's, and
    /// [`with_intent`] left that schema alone, so nothing was injected to remove.
    fn without_intent<'a>(&self, call: &'a ToolCall) -> Cow<'a, ToolCall> {
        let declares_own = match self {
            Provider::Bash(_) => declares_own_intent(&Bash::definition()["input_schema"]),
            Provider::ReadImage(_) => declares_own_intent(&ReadImage::definition()["input_schema"]),
            Provider::Fs(s) => s
                .schema(&call.name)
                .is_some_and(|d| declares_own_intent(&d.input_schema)),
            Provider::SemanticEdit(s) => s
                .schema(&call.name)
                .is_some_and(|d| declares_own_intent(&d.input_schema)),
            Provider::Connected(c) => c
                .brief(&call.name)
                .is_some_and(|b| declares_own_intent(&b.definition.input_schema)),
        };
        if declares_own || call.input.get(INTENT).is_none() {
            return Cow::Borrowed(call);
        }
        let mut stripped = call.clone();
        if let Some(object) = stripped.input.as_object_mut() {
            object.remove(INTENT);
        }
        Cow::Owned(stripped)
    }

    /// This backend's full definition for `name`, for `GET /v1/tools/:name`
    /// (docs/control-api.md) — the entry [`Toolbox::tool_entry`] serves.
    /// Unlike [`Self::brief`], `Bash` and `ReadImage` answer: their wire
    /// `Value` carries a `description` and `input_schema`, just not the
    /// `annotations`/`title` the Anthropic tool shape has no slot for.
    fn tool_entry(&self, name: &str) -> Option<ToolEntry> {
        // Every provider here is mounted through `Toolbox`, so its wire entry
        // went through `with_intent` in `Toolbox::definitions` — the served
        // schema does too (`inject_intent: true`).
        match self {
            Provider::Bash(_) if name == bash::NAME => {
                Some(ToolEntry::from_raw(name, &Bash::definition(), None, true))
            }
            Provider::Bash(_) => None,
            Provider::ReadImage(_) if name == read_image::NAME => Some(ToolEntry::from_raw(
                name,
                &ReadImage::definition(),
                None,
                true,
            )),
            Provider::ReadImage(_) => None,
            Provider::Fs(s) => Some(ToolEntry::from_definition(s.schema(name)?, None, true)),
            Provider::SemanticEdit(s) => {
                Some(ToolEntry::from_definition(s.schema(name)?, None, true))
            }
            Provider::Connected(c) => {
                let brief = c.brief(name)?;
                Some(ToolEntry::from_definition(
                    brief.definition,
                    brief.server.map(str::to_string),
                    true,
                ))
            }
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
            Provider::ReadImage(r) => r.run(&call.input).into(),
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
                return RawResult::error(format!("invalid input for {name}: {e}"));
            }
        };
        match tool.call(&mut self.state.borrow_mut(), &request_context()) {
            Ok(result) => crate::mcp::render_result(result),
            Err(e) => RawResult::error(e.to_string()),
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

/// One tool's full definition, addressed by wire name — the payload for
/// `GET /v1/tools/:name` (docs/control-api.md). A consumer fetches these
/// lazily, one at a time, rather than holding a table pushed at session
/// start: an MCP server may mount dozens of tools with long descriptions.
///
/// `description` and `annotations`/`title` are third-party prose for a
/// mounted server (`server.is_some()`): a consumer renders them as plain
/// text, never markdown, and attributes them to `server`
/// (`src/supervisor.rs` draws the same untrusted-server line for the
/// built-ins' own annotations).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolEntry {
    /// The wire name this entry was looked up by (`<server>__<tool>` or an
    /// unprefixed embedded name) — echoed back so a caller that resolved a
    /// dotted operator spelling can confirm what it landed on.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Sent exactly as the model saw it this session, [`INTENT`] included
    /// (docs/tools.md) — so a consumer comparing a call's arguments against
    /// `required` sees the schema the call was actually made against, not a
    /// pre-injection one that would flag `intent` as undeclared on every call.
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<mcplease::types::ToolAnnotations>,
    /// The server that mounted this tool, or `None` for one of the harness's
    /// own embedded tools (`bash`, `read_image`, the fs/semantic-edit MCP
    /// servers) — the line [`ToolBrief::server`] already draws.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

impl ToolEntry {
    /// From a mounted [`ToolDefinition`] (an embedded or connected server's
    /// snapshotted schema). `inject_intent` mirrors whatever the wire
    /// actually did with this schema: `true` for a `Toolbox`-mounted tool
    /// (`with_intent` runs over every entry in [`Toolbox::definitions`]),
    /// `false` for the harness's own loop-owned tools (`agent::tool_catalog`
    /// — `consolidate`, the supervisor/inbox family — which
    /// [`agent::tool_definitions`](crate::agent::tool_definitions) advertises
    /// *without* running them through [`with_intent`]).
    pub(crate) fn from_definition(
        definition: &ToolDefinition,
        server: Option<String>,
        inject_intent: bool,
    ) -> Self {
        let input_schema = definition.input_schema.clone();
        Self {
            name: definition.name.clone(),
            title: definition.title.clone(),
            description: definition.description.clone(),
            input_schema: if inject_intent {
                with_intent_schema(input_schema)
            } else {
                input_schema
            },
            annotations: definition.annotations.clone(),
            server,
        }
    }

    /// From a raw wire `Value` (`Bash::definition()`, `ReadImage::definition()`,
    /// or one of the peek tools' static schemas) — no [`ToolDefinition`] backs
    /// any of these, so `title`/`annotations` are unavailable rather than
    /// absent-by-choice. `inject_intent` — see [`Self::from_definition`].
    pub(crate) fn from_raw(
        name: &str,
        raw: &Value,
        server: Option<String>,
        inject_intent: bool,
    ) -> Self {
        let input_schema = raw
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| json!({}));
        Self {
            name: name.to_string(),
            title: None,
            description: raw
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            input_schema: if inject_intent {
                with_intent_schema(input_schema)
            } else {
                input_schema
            },
            annotations: None,
            server,
        }
    }
}

/// [`with_intent`] applied to a bare `input_schema`, unwrapped back out —
/// reused so [`ToolEntry`] carries exactly the schema the model was sent,
/// without duplicating the injection rule.
fn with_intent_schema(schema: Value) -> Value {
    with_intent(json!({ "input_schema": schema }))
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| json!({}))
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

/// What a call naming no tool gets back, given the names that *do* exist.
///
/// `unknown tool: ferritin` is true and useless: it says what failed, not what
/// would work. In the incident this is drawn from, `ferritin` was an allowed
/// **bash prefix** listed in the session's boot context as a skill, and the model
/// built a structured call out of the name rather than running it as a command.
/// Nothing on the path corrected it — the result text didn't, and the distillate
/// then wrote down a false lesson the raw never claimed ("not available in this
/// environment"). The correction belongs in the result text because a text that
/// carries it is one a summary can only compress, not invent past (docs/tools.md).
///
/// Naming what *does* exist is the general form of the fix. `bash` is always
/// mounted, so pointing at it covers the whole family of "this name is a program,
/// not a tool" — the case that actually occurred.
pub fn unknown_tool_message(name: &str, available: &[String]) -> String {
    let mut message = format!("unknown tool: {name}\n\n");
    if available.is_empty() {
        message.push_str("No tools are mounted in this session.");
        return message;
    }
    message.push_str(&format!("Mounted tools: {}.", available.join(", ")));
    if available.iter().any(|n| n == bash::NAME) {
        let bash = bash::NAME;
        message.push_str(&format!(
            "\n\nIf `{name}` is a program rather than a tool, run it through `{bash}`."
        ));
        message.push_str(&format!(
            " `{bash}` is how any command-line program is invoked, including one the session's config allows by name."
        ));
    }
    message
}

/// The mounted set of tool backends. The loop asks it for the wire `tools[]` and
/// routes each `tool_use` to the backend that handles it.
pub struct Toolbox {
    providers: Vec<Provider>,
    /// The `[env]` spawn policy — strip, then inject — applied to every
    /// subprocess this toolbox spawns: the `bash` child and each stdio MCP
    /// server (docs/env.md). Held here as well as on the bash provider so a
    /// server mounted *after* the toolbox was built still gets it.
    child_env: crate::env_table::ChildEnv,
    /// Whether the session's model accepts image blocks
    /// (`ModelCapabilities::supports_image_input`, docs/transport.md). `true`
    /// unless a boot lookup said otherwise, which is what the dev views (`wire`,
    /// `spotter`, `peek`) get — they never run a tool.
    ///
    /// Held on the toolbox rather than on the image-producing tools because two
    /// unrelated backends produce images (`read_image`, and any MCP server whose
    /// result carries an image block), and the gate has to answer the same way
    /// for both. Applied at [`Toolbox::run`], the one seam every call passes
    /// through (docs/tools.md).
    image_input: bool,
}

impl Toolbox {
    pub fn new(providers: Vec<Provider>) -> Self {
        Self {
            providers,
            child_env: crate::env_table::ChildEnv::default(),
            image_input: true,
        }
    }

    /// Record whether this session's model accepts images
    /// (`ModelCapabilities::supports_image_input`). Separate from construction
    /// for the same reason as [`Self::with_bash_limits`]: only a served session
    /// has run the boot capability lookup.
    pub fn with_image_input(mut self, image_input: bool) -> Self {
        self.image_input = image_input;
        self
    }

    /// Just bash — no MCP mounts, for tests and the dev views.
    pub fn bash_only() -> Self {
        Self::new(vec![Provider::Bash(Bash::default())])
    }

    /// Apply the `[env]` table's spawn policy to the mounted `bash` provider
    /// (docs/env.md). Separate from construction for the same reason as
    /// [`with_bash_limits`](Self::with_bash_limits): only a served session has a
    /// config, and the dev views never spawn anything.
    pub fn with_child_env(mut self, child_env: crate::env_table::ChildEnv) -> Self {
        for provider in &mut self.providers {
            if let Provider::Bash(bash) = provider {
                bash.child_env = child_env.clone();
            }
        }
        self.child_env = child_env;
        self
    }

    /// Apply the config's foreground-lifetime limits to the mounted `bash` provider
    /// (docs/supervisor.md). Separate from construction because only a served session
    /// has a config — the dev views (`wire`, `spotter`, `peek`) build a toolbox to read
    /// tool *definitions* and never run a command, so they keep the defaults.
    pub fn with_bash_limits(mut self, limits: crate::config::BashLimits) -> Self {
        for provider in &mut self.providers {
            if let Provider::Bash(bash) = provider {
                bash.background_after = limits.background_after;
                bash.background_after_max = limits.background_after_max;
            }
        }
        self
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
            Provider::Bash(Bash {
                cwd: cwd.clone(),
                tmpdir: scratch,
                ..Bash::default()
            }),
            Provider::ReadImage(ReadImage { cwd: cwd.clone() }),
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
        authorization: Option<&str>,
    ) -> anyhow::Result<()> {
        let provider =
            crate::mcp::ConnectedProvider::mount(name, config, cwd, authorization, &self.child_env)
                .await?;
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

    /// All tool definitions, in mount order — the request's `tools[]`, each carrying
    /// the injected [`INTENT`] property (docs/tools.md). One injection site for every
    /// backend: a tool the harness mounts and a tool a connected server advertises
    /// get the same field on the same terms.
    pub fn definitions(&self) -> Vec<Value> {
        self.providers
            .iter()
            .flat_map(Provider::definitions)
            .map(with_intent)
            .collect()
    }

    /// Every advertised name the given dialect's request would reject, each with why
    /// (docs/tools.md). Names are checked as they reach the wire — the namespaced
    /// `<server>__<tool>` form — because the MCP spec lets a server's own tool name
    /// carry characters and lengths the request alphabet does not, and the namespace
    /// prefix counts against the ceiling. Empty means every name is legal.
    pub fn illegal_names(&self, dialect: crate::api::WireDialect) -> Vec<String> {
        self.names()
            .iter()
            .filter_map(|name| {
                dialect
                    .tool_name_problem(name)
                    .map(|why| format!("{} {why}", crate::mcp::display_name(name)))
            })
            .collect()
    }

    /// The names of every mounted tool, in mount order.
    pub fn names(&self) -> Vec<String> {
        self.definitions()
            .iter()
            .filter_map(|d| d.get("name")?.as_str().map(str::to_string))
            .collect()
    }

    /// What a call routed here under no mounted name gets back — the toolbox's own
    /// names. The loop short-circuits a name no *turn* tool owns before dispatch
    /// (docs/permissions.md), so this path answers the narrower question: the name
    /// reached a toolbox that does not own it.
    fn unknown_tool_message(&self, name: &str) -> String {
        unknown_tool_message(name, &self.names())
    }

    /// The spotter's brief for a structured tool (docs/permissions.md): what the
    /// mount snapshotted about it, so the rung that judges an unfamiliar tool sees
    /// more than a name and a JSON blob. `None` for bash and for a name no backend
    /// owns.
    pub fn brief(&self, name: &str) -> Option<ToolBrief<'_>> {
        self.providers.iter().find_map(|p| p.brief(name))
    }

    /// One mounted tool's full definition, addressed by its wire name — the
    /// answer `GET /v1/tools/:name` serves (docs/control-api.md). Unlike
    /// [`Toolbox::brief`], this has no holes: `Bash` and `ReadImage` answer
    /// here even though the spotter's brief skips them for reasons that are
    /// about the spotter, not about the definition existing. `None` when no
    /// mounted provider owns `name`.
    pub fn tool_entry(&self, name: &str) -> Option<ToolEntry> {
        self.providers.iter().find_map(|p| p.tool_entry(name))
    }

    /// Every mounted tool's full definition, keyed by wire name — what the
    /// daemon snapshots into `Session` before the toolbox moves into the loop
    /// (docs/control-api.md), since the mounted set is fixed for the session
    /// and there is nothing to reconcile later (docs/log-and-projection.md).
    pub fn tool_catalog(&self) -> std::collections::HashMap<String, ToolEntry> {
        self.names()
            .into_iter()
            .filter_map(|name| {
                let entry = self.tool_entry(&name)?;
                Some((name, entry))
            })
            .collect()
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
        // A model that reports no image input is told so *before* the read
        // (docs/tools.md): `read_image` exists only to produce an image, so it
        // has no useful degraded form, and decoding a file to throw the pixels
        // away spends the work to arrive at the same answer.
        if !self.image_input && call.name == read_image::NAME {
            return RawResult::error(NO_IMAGE_INPUT_REFUSAL).into();
        }
        let outcome = match self.providers.iter().find(|p| p.handles(&call.name)) {
            Some(provider) => {
                let call = provider.without_intent(call);
                provider.run(&call, abort, on_spawn, on_output).await
            }
            None => RawResult::error(self.unknown_tool_message(&call.name)).into(),
        };
        strip_unreadable_images(outcome, self.image_input)
    }
}

/// What a call that would have produced an image gets back when the session's
/// model reports no image input (docs/tools.md). Addressed to the model, and
/// says what *would* work — the sentence a model reads after a refusal decides
/// whether it retries the same call.
const NO_IMAGE_INPUT_REFUSAL: &str = "this session's model reports no image input, so an image \
     cannot be added to the transcript. read the file's text some other way, or ask the operator \
     to serve a model that takes images.";

/// Drop realized images from a result the model cannot read, replacing the bytes
/// with a line saying why (docs/tools.md). An MCP tool call is not *about* its
/// image the way `read_image` is — a screenshot tool's text half is often the
/// useful part — so the call still succeeds and `is_error` stays false; what
/// changes is that the log and the wire carry no bytes nothing will look at, and
/// the model is told rather than left to infer from silence.
fn strip_unreadable_images(outcome: Outcome, image_input: bool) -> Outcome {
    if image_input {
        return outcome;
    }
    let Outcome::Done(mut result) = outcome else {
        return outcome;
    };
    if result.images.is_empty() {
        return Outcome::Done(result);
    }
    let dropped = result.images.len();
    result.images.clear();
    result.content.push_str(&format!(
        "\n\n[{dropped} image block(s) withheld: this session's model reports no image input]"
    ));
    Outcome::Done(result)
}

#[cfg(test)]
mod test;

pub mod read_image;
