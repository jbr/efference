//! Names a recorded tool call is read by (docs/tools.md, docs/process-monitor.md): the key
//! that carries a call's intent inside its `input`, and the tool name under which a
//! question to the operator is recorded.
//!
//! The tool backends add and answer them; the projection, the stop classifier and the
//! clients read them back out of the log.

/// The property the harness adds to every mounted tool's input schema
/// (docs/tools.md "The `intent` property"): one line of prose saying why the call
/// is being made, generated before the call runs. Stripped from `input` before
/// dispatch, so no tool ever receives it; kept on the logged
/// [`ToolCall`](super::ToolCall), so it projects to the wire and is available to the
/// harness's own readers.
pub const INTENT: &str = "intent";

/// `ask` — put a question to the operator and block until a submitted turn answers it
/// (docs/process-monitor.md). Loop-owned like `wait`, but it bypasses the permission cascade:
/// there is nothing in the world to authorize.
pub const ASK: &str = "ask";
