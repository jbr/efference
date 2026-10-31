//! Tool names and the namespace spelling they are called by (docs/tools.md): the key a
//! call's intent travels in, the wire names of the harness's own tools that the loop, the
//! permission config and the clients match on, and the `<namespace>__<tool>` form.

/// The property the harness adds to every mounted tool's input schema
/// (docs/tools.md "The `intent` property"): one line of prose saying why the call
/// is being made, generated before the call runs. Stripped from `input` before
/// dispatch, so no tool ever receives it; kept on the logged
/// [`ToolCall`](crate::log::ToolCall), so it projects to the wire and is available to the
/// harness's own readers.
pub const INTENT: &str = "intent";

/// `ask` — put a question to the operator and block until a submitted turn answers it
/// (docs/process-monitor.md). Loop-owned like `wait`, but it bypasses the permission cascade:
/// there is nothing in the world to authorize.
pub const ASK: &str = "ask";

/// `bash` — run a command in its own process group (docs/tools.md). The tool name the
/// model calls; `tool_use` / `tool_result` ids pair on it.
pub const BASH: &str = "bash";

/// `consolidate` — replace a finished span of the transcript with a summary the agent
/// writes (docs/consolidate.md). The loop branches on this name to take the structural path.
pub const CONSOLIDATE: &str = "consolidate";

/// `list_processes` — the directed read of running processes (docs/process-monitor.md). Named to
/// avoid colliding with fs-mcp's `list`.
pub const LIST_PROCESSES: &str = "list_processes";
/// `read_process` — cursor-based tailing of one process's output (the process monitor's
/// `read_since`, docs/process-monitor.md). Named to avoid colliding with fs-mcp's `read`.
pub const READ_PROCESS: &str = "read_process";
/// `kill_process` — terminate a background process's group (docs/process-monitor.md).
pub const KILL_PROCESS: &str = "kill_process";
/// `watch_process` — register a one-shot output pattern that notifies on a hit
/// (docs/process-monitor.md). Named beside `read_process`/`kill_process`; a bare `watch`
/// would read as a filesystem watcher.
pub const WATCH_PROCESS: &str = "watch_process";
/// `check_inbox` — surface pending notifications on demand (the directed inbox read,
/// docs/inbox.md) so the model needn't wait for them to ride its next action.
pub const CHECK_INBOX: &str = "check_inbox";
/// `wait` — block until a new inbox notification arrives, then surface pending
/// notifications (docs/process-monitor.md). Refuses when no known source can wake it.
pub const WAIT: &str = "wait";

/// `retain` — the tool a per-result triage calls to choose what the canonical wire keeps
/// of a result and which notifications to pin (docs/triage.md).
pub const RETAIN: &str = "retain";

/// `triage` — the tool of the **standalone** idle triage (docs/process-monitor.md): no
/// `keep` (there's no tool output to retain), only inbox/background dispositions.
pub const TRIAGE: &str = "triage";

/// Whether `name` is a triage tool. Both stand in every turn's `tools[]` (the shared
/// envelope, docs/triage.md) but are answered only when a triage's tail
/// instruction asks — the agent loop pairs an un-asked call with a nudge result
/// instead of running the cascade.
pub fn is_triage_tool(name: &str) -> bool {
    name == RETAIN || name == TRIAGE
}

/// The separator between server name and tool name on the wire
/// (`github__create_issue`). Two underscores: legal in the API's tool-name
/// alphabet, and absent from every embedded tool name, so a namespaced name
/// can never collide with an embedded one.
pub const NAMESPACE_SEPARATOR: &str = "__";

/// The namespace the harness's own embedded tools sit in for grant purposes
/// (docs/permissions.md). Their wire names carry no prefix — `read` is `read` —
/// so this is the name that stands in for the missing `<server>` half when a
/// `[tools]` entry is resolved: `efference` is the whole-namespace entry over the
/// built-ins, and `"efference.read"` is how `read` is keyed. A declared server
/// may not take the name (`config::BUILTIN_NAMESPACES`).
pub const BUILTIN_NAMESPACE: &str = "efference";

/// The namespace the in-session set is mounted under: `agents__<verb>` (docs/tools.md).
pub const AGENTS_NAMESPACE: &str = "agents";

/// The wire namespace of the language-server tools (`lsp__definition`). One of efference's
/// own, so it is off unless a `[tools]` entry enables it (`config::BUILTIN_NAMESPACES`).
pub const LSP_NAMESPACE: &str = "lsp";

/// A namespaced wire name rendered for a human: `ferritin__get` → `ferritin.get`.
///
/// The wire alphabet (`[a-zA-Z0-9_-]`) has no separator that reads as one, so
/// `__` is what the name must *be*; a dot is what it wants to look like. Only
/// the first occurrence is replaced — the server name is validated against that
/// same alphabet and embedded tool names contain no `__`, so the first `__` is
/// always the namespace boundary and any later one belongs to the server's own
/// tool name.
///
/// Display only. Nothing that reaches the wire, the log, or a lookup may go
/// through here — the dotted form is not a name anything can be found by.
pub fn display_name(wire_name: &str) -> String {
    wire_name.replacen(NAMESPACE_SEPARATOR, ".", 1)
}

/// The namespace a wire tool name belongs to: the `<server>` half of
/// `<server>__<tool>`, or [`BUILTIN_NAMESPACE`] for an unprefixed embedded name.
/// The boundary is the **first** `__`, the same rule [`display_name`] applies —
/// server names may not contain one.
pub fn namespace_of(wire_name: &str) -> &str {
    match wire_name.split_once(NAMESPACE_SEPARATOR) {
        Some((server, _)) => server,
        None => BUILTIN_NAMESPACE,
    }
}

/// How a logged grant names a whole namespace (docs/permissions.md): a
/// [`crate::log::ConfigChange::Tool`] carries `ferritin__*` for a granted
/// `ferritin` entry, beside the wire names exact grants carry. `*` is absent from
/// the API's tool-name alphabet, so it can never collide with a real advertised
/// name.
pub fn namespace_glob(namespace: &str) -> String {
    format!("{namespace}{NAMESPACE_SEPARATOR}*")
}

/// Whether a logged grant name is a whole-namespace glob — `<namespace>__*`
/// with a non-empty namespace that is itself star-free ([`namespace_glob`]).
pub fn is_namespace_glob(entry: &str) -> bool {
    entry
        .strip_suffix(&namespace_glob(""))
        .is_some_and(|namespace| {
            !namespace.is_empty()
                && !namespace.contains('*')
                && !namespace.contains(NAMESPACE_SEPARATOR)
        })
}
