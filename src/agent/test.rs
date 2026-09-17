use super::*;

pub(super) fn call(name: &str) -> ToolCall {
    ToolCall::new(name, "toolu_x", serde_json::json!({}))
}

pub(super) fn bash_call(command: &str) -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        "toolu_b",
        serde_json::json!({ "command": command }),
    )
}

mod cascade;
mod grants;
mod tools;
mod turn;
