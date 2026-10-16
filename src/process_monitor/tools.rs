//! The process-monitor/inbox tool family's mounted-table entries (docs/process-monitor.md).
//!
//! The loop dispatches this family structurally, outside the `Toolbox`, so its
//! definitions live here rather than on a provider — read by the wire array
//! (`agent::tool_definitions`), the spotter's brief, and the tool catalog.

use std::sync::LazyLock;

use serde_json::{Value, json};

use super::{ASK, CHECK_INBOX, KILL, LIST, READ, WAIT, WATCH};
use crate::tools::{ToolBrief, anthropic_def};
use mcplease::types::{Tool as ToolDefinition, ToolAnnotations};

/// The mounted-table entries for the process-monitor/inbox tools — the same
/// [`ToolDefinition`] shape a provider's mount snapshots, so the spotter's brief
/// (docs/permissions.md) can read them like any other tool's. The loop dispatches
/// this family structurally, outside the `Toolbox`, so a brief lookup that
/// consulted only the mounted providers would judge these as a bare name plus an
/// opaque JSON input — the descriptions exist, and this is what carries them to
/// that rung.
///
/// The annotations are the harness's claims about its own tools, so they carry
/// none of the untrusted-server caveat: everything here is session-local
/// (closed-world), the reads are reads, and `kill_process` is the one
/// destructive member (SIGKILL, idempotent — a second kill of a dead process
/// changes nothing further).
static SCHEMAS: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| {
    let read_only = ToolAnnotations {
        read_only_hint: Some(true),
        open_world_hint: Some(false),
        ..Default::default()
    };
    let mut list = ToolDefinition::new(LIST, json!({ "type": "object", "properties": {} }));
    list.description = Some(
        "List the background processes currently running — handle, command, how long each \
         has run, and how long since its last output. Their recent output also surfaces \
         automatically after each of your actions; use this to check on them deliberately."
            .into(),
    );
    list.annotations = Some(read_only.clone());

    let mut read = ToolDefinition::new(
        READ,
        json!({
            "type": "object",
            "properties": {
                "handle": { "type": "string", "description": "The process handle, e.g. \"p#3\"." },
                "cursor": { "type": "integer", "description": "Byte offset from a previous read_process result; omit to start from the earliest retained output." }
            },
            "required": ["handle"]
        }),
    );
    read.description = Some(
        "Read a background process's output. Returns everything from `cursor` (a byte \
         offset returned by a previous read; omit to start from the earliest retained \
         output) to the end, plus a new cursor to pass next time so you don't re-read \
         what you've seen."
            .into(),
    );
    read.annotations = Some(read_only.clone());

    let mut kill = ToolDefinition::new(
        KILL,
        json!({
            "type": "object",
            "properties": {
                "handle": { "type": "string", "description": "The process handle, e.g. \"p#3\"." }
            },
            "required": ["handle"]
        }),
    );
    kill.description = Some(
        "Kill a running background process (SIGKILL to its whole process group). Its exit \
         is then confirmed in your inbox."
            .into(),
    );
    kill.annotations = Some(ToolAnnotations {
        read_only_hint: Some(false),
        destructive_hint: Some(true),
        idempotent_hint: Some(true),
        open_world_hint: Some(false),
        ..Default::default()
    });

    let mut watch = ToolDefinition::new(
        WATCH,
        json!({
            "type": "object",
            "properties": {
                "handle": { "type": "string", "description": "The process handle, e.g. \"p#3\"." },
                "pattern": { "type": "string", "description": "Plain text to look for in a line of the process's output. Matched as a substring, case-sensitively — not a regular expression." }
            },
            "required": ["handle", "pattern"]
        }),
    );
    watch.description = Some(
        "Ask to be notified the next time a background process prints a line containing \
         `pattern`. Returns immediately — it does not wait. Only output printed after this \
         call is matched, and the watch fires once and is then done; register it again if \
         you want the next occurrence too. Use this instead of polling read_process in a \
         loop: end your turn, and the match arrives in your inbox."
            .into(),
    );
    watch.annotations = Some(ToolAnnotations {
        // Registers a subscription in the ephemeral registry — nothing in the world,
        // nothing in the log, and no process state the other tools can observe. Not
        // `read_only`, because it does leave something behind that later fires.
        read_only_hint: Some(false),
        destructive_hint: Some(false),
        idempotent_hint: Some(false),
        open_world_hint: Some(false),
        ..Default::default()
    });

    let mut inbox = ToolDefinition::new(CHECK_INBOX, json!({ "type": "object", "properties": {} }));
    inbox.description = Some(
        "Show your pending notifications now — background-process exits, operator nudges, \
         messages from other agents — so you can triage them on demand instead of waiting \
         for them to surface after your next action."
            .into(),
    );
    inbox.annotations = Some(read_only.clone());

    let mut wait = ToolDefinition::new(WAIT, json!({ "type": "object", "properties": {} }));
    wait.description = Some(
        "Wait for the next inbox notification, then return the pending notifications. Refuses if no \
         running background process or pending notification can wake you."
            .into(),
    );
    wait.annotations = Some(read_only);

    let mut ask = ToolDefinition::new(
        ASK,
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question, in full. The operator reads it as your message, so do not repeat it in prose."
                }
            },
            "required": ["question"]
        }),
    );
    ask.description = Some(
        "Put a question to the operator and wait for the reply, which comes back as this call's \
         result. Use it when you need an answer, a decision, or a go-ahead before continuing; \
         ending your turn means the work is done. Notifications that arrive while you wait do not \
         answer it; they are shown to you beside the reply. Asks made in parallel are presented \
         to the operator one at a time, in an unspecified order, so make them independent of each \
         other."
            .into(),
    );
    ask.annotations = Some(ToolAnnotations {
        // Waits on a person and changes nothing in the world or the session's own state.
        read_only_hint: Some(true),
        open_world_hint: Some(false),
        ..Default::default()
    });

    vec![list, read, kill, watch, inbox, wait, ask]
});

/// The harness-level process-monitor/inbox tools (docs/process-monitor.md). Like `consolidate`, these
/// act on daemon state (the registry, the inbox), not the world, so the agent loop
/// dispatches them structurally — but their results triage through the universal
/// fork like any tool (no bypass). Derived from the same [`SCHEMAS`] the brief
/// reads, through the same `anthropic_def` a provider's wire entries go through
/// (sorted keys, annotations dropped — the wire has no slot for them), so
/// `tools[]` stays byte-stable for the session (docs/log-and-projection.md).
pub fn tool_definitions() -> Vec<Value> {
    SCHEMAS.iter().map(anthropic_def).collect()
}

/// The spotter's brief for a process-monitor/inbox tool (docs/permissions.md); `None`
/// outside the family. `server: None` — these are the harness's own.
pub fn brief(name: &str) -> Option<ToolBrief<'static>> {
    let schemas: &'static Vec<ToolDefinition> = &SCHEMAS;
    schemas
        .iter()
        .find(|s| s.name == name)
        .map(|definition| ToolBrief {
            server: None,
            definition,
        })
}
