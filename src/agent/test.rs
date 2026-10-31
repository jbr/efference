use super::*;

use crate::names::tools::BASH;

pub(super) fn call(name: &str) -> ToolCall {
    ToolCall::new(name, "toolu_x", serde_json::json!({}))
}

pub(super) fn bash_call(command: &str) -> ToolCall {
    ToolCall::new(BASH, "toolu_b", serde_json::json!({ "command": command }))
}

mod ask;
mod cascade;
mod expiry;
mod gate_grant;
mod grants;
mod images;
mod title;
mod tools;
mod turn;
