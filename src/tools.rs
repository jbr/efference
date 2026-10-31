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
//! as this enum boundary — the loop knows only `Toolbox`, never a concrete tool. One
//! variant holds a trait object: the session-control verbs are defined above this module,
//! so `Provider::Sessions` holds them as a [`ControlServer`] (`session_control.rs`).
//!
//! The mounted set is fixed for the session (chosen once in `main`), so the
//! request's `tools[]` is byte-stable and never perturbs the tools cache tier
//! (docs/log-and-projection.md).

use std::borrow::Cow;
use std::future::Future;
use std::marker::PhantomData;
use std::path::PathBuf;

use fs_mcp::FsTools;
use mcplease::traits::{AsToolsList, Dispatch};
use mcplease::types::{RequestContext, Tool as ToolDefinition};
use semantic_edit_mcp::SemanticEditTools;
use serde_json::{Value, json};

use crate::bash::{Bash, Outcome};
use crate::log::{Notification, RawResult, ToolCall};
use crate::names::tools::{BASH, INTENT};
use read_image::ReadImage;

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

/// The operator's intent when interrupting a *running* tool (docs/process-monitor.md).
/// Both reach the one intervention point pace can't — mid-tool — but differ in
/// what happens to the child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    /// Kill the process group now (`POST /loop/interrupt`, the Ctrl-C guardrail).
    Kill,
    /// Keep the child running but stop blocking on it: hand it to the background
    /// process monitor as a `p#<n>` process (`PATCH /processes/:id {background: true}`, the
    /// foreground→background handoff, docs/process-monitor.md).
    Detach,
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
    /// The session-control verbs (docs/tools.md): `agents__*`, driving the
    /// host in-process with the calling session's identity. Mounted only for a
    /// session created on a host.
    Sessions(Box<dyn ControlServer>),
    /// The `lsp` built-ins (docs/tools.md): `lsp__*`, structural-lsp's tools over
    /// the host's shared language servers. Mounted only for a session created on a
    /// host whose config enables them.
    Lsp(Box<lsp::Lsp>),
    /// A connected provider (docs/tools.md): a user-declared server reached
    /// over stdio or HTTP, its tool table discovered at mount.
    Connected(Box<crate::mcp::ConnectedProvider>),
}

/// An in-process MCP server (docs/tools.md) adapted to the toolbox, generic over the
/// server's `State` and its generated `mcplease` `ToolEnum` dispatcher. Holds the
/// state — a bundle of shared handles, cloned into each call, which is what
/// `mcplease`'s `Dispatch::call` takes — plus the precomputed Anthropic tool
/// definitions and the names it answers for. fs-mcp and semantic-edit-mcp are two
/// instantiations.
pub struct McpServer<State, ToolEnum> {
    state: State,
    definitions: Vec<Value>,
    /// The mounted tool table, aligned with `definitions`. Kept whole (not just
    /// the names) because `anthropic_def` drops what the wire has no slot for —
    /// the annotations and the title — and the spotter reads those
    /// ([`ToolBrief`], docs/permissions.md).
    schemas: Vec<ToolDefinition>,
    /// The wire-name namespace (`<server>__<tool>`) this server was mounted
    /// under, or `None` for a bare-named embedded server (docs/tools.md).
    namespace: Option<String>,
    _tools: PhantomData<fn() -> ToolEnum>,
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
/// (`src/process_monitor.rs` draws the same untrusted-server line for the
/// built-ins' own annotations).
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    // The MCP annotations are mcplease's type, which derives no schema; any object
    // describes them.
    #[cfg_attr(
        test,
        schemars(with = "Option<std::collections::BTreeMap<String, Value>>")
    )]
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
    /// — `consolidate`, the process-monitor/inbox family — which
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
                with_intent_schema(&input_schema)
            } else {
                input_schema
            },
            annotations: definition.annotations.clone(),
            server,
        }
    }

    /// From a raw wire `Value` (`Bash::definition()`, `ReadImage::definition()`,
    /// or one of the triage tools' static schemas) — no [`ToolDefinition`] backs
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
                with_intent_schema(&input_schema)
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
fn with_intent_schema(schema: &Value) -> Value {
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
/// Nothing on the path corrected it — the result text didn't, and the selection
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
    if available.iter().any(|n| n == BASH) {
        let bash = BASH;
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
    /// `spotter`, `triage`) get — they never run a tool.
    ///
    /// Held on the toolbox rather than on the image-producing tools because two
    /// unrelated backends produce images (`read_image`, and any MCP server whose
    /// result carries an image block), and the gate has to answer the same way
    /// for both. Applied at [`Toolbox::run`], the one function every call passes
    /// through (docs/tools.md).
    image_input: bool,
}

#[cfg(test)]
mod test;

pub mod lsp;
mod mcp_server;
mod provider;
pub mod read_image;
mod session_control;
pub use session_control::{ControlServer, SessionControl};
mod toolbox;
#[cfg(test)]
use toolbox::strip_unreadable_images;
