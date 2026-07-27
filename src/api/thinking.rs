//! Extended-thinking selection per model, and the tool-choice knob (docs/transport.md).

use super::*;

/// The `thinking` field (docs/transport.md, docs/transport.md). `Adaptive` lets the
/// model decide when/how much to think and interleaves it across turns; `display: "summarized"`
/// captures a readable model-authored summary into the log (same token cost as omitted). The
/// serde tag renders `{"type":"adaptive","display":"summarized"}` / `{"type":"disabled"}`
/// exactly.
#[derive(Serialize, Clone, Copy)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Thinking {
    Disabled,
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<&'static str>,
    },
}

impl Thinking {
    /// Thinking rides on `tool_choice` and the model: forced choice ⟺ no thinking (the API
    /// rejects the combination), and a model that can't run adaptive thinking (e.g. Haiku
    /// 4.5) never gets it even on `auto`. Encoding both here makes the illegal pairings
    /// unrepresentable (docs/transport.md).
    pub(super) fn for_request(model: &str, tool_choice: &ToolChoice) -> Self {
        match tool_choice.kind {
            "auto" if supports_adaptive_thinking(model) => THINKING_ADAPTIVE,
            _ => THINKING_DISABLED,
        }
    }
}

/// Whether to send `thinking: {type:"adaptive"}` for `model` (docs/transport.md,
/// docs/transport.md). An explicit `EFFERENCE_THINKING` wins; otherwise this
/// is an **allowlist**, not "everything but haiku": most non-Anthropic models (reached
/// via OpenRouter) don't accept the Anthropic `thinking` field at all, so the safe
/// default is off unless the model is a recognized adaptive-capable Claude tier
/// (Sonnet/Opus/Fable — **not** Haiku, which takes `enabled`+`budget_tokens` only).
/// Matched by substring so both bare ids (`claude-opus-4-8`) and provider-scoped slugs
/// (`anthropic/claude-opus-4.1`) match. There is no portable capability query for this
/// — OpenRouter's `supported_parameters` describes its normalized `reasoning` interface,
/// not the Anthropic `thinking` block — so recognition lives here; override per session
/// with `EFFERENCE_THINKING=adaptive|off`.
pub(super) fn supports_adaptive_thinking(model: &str) -> bool {
    THINKING_OVERRIDE.unwrap_or_else(|| is_recognized_adaptive_claude(model))
}

/// The allowlist behind [`supports_adaptive_thinking`]'s `auto` default: a Claude
/// family model (direct id or `anthropic/…` slug) that isn't Haiku. Everything
/// foreign (`openai/…`, `google/…`, …) falls outside it and thus gets no `thinking`.
///
/// Adaptive thinking (`{type:"adaptive"}`) is a Claude **4.6+** feature; pre-4.6
/// tiers (Opus 4.5/4.1, Sonnet 4.5/4, …) speak the older `{type:"enabled",
/// budget_tokens}` API and **400 on adaptive**. This substring rule doesn't encode
/// that version floor — parsing a version out of both `claude-opus-4-8` and
/// `anthropic/claude-opus-4.1` is its own fragile problem — so it assumes the
/// current Claude generation, which is what efference actually serves (default
/// `claude-opus-4-8`). efference can't express budgeted thinking anyway (the
/// [`Thinking`] enum is Adaptive/Disabled only), so a pre-4.6 Claude wants
/// thinking off: set `EFFERENCE_THINKING=off` for that non-target case.
pub(super) fn is_recognized_adaptive_claude(model: &str) -> bool {
    (model.contains("claude") || model.contains("anthropic")) && !model.contains("haiku")
}

/// The `tool_choice` field. Phase 1+ always sets `disable_parallel_tool_use` so
/// the model emits **at most one** `tool_use` per turn (docs/README.md) — set once and
/// never varied, so it never invalidates the cache. `auto()` lets the model
/// decide whether to call a tool; `force(name)` makes it call exactly that one
/// (the fork's `retain`, docs/redaction-fork.md).
#[derive(Serialize, Clone, Copy)]
pub struct ToolChoice {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'static str>,
    disable_parallel_tool_use: bool,
}

impl ToolChoice {
    pub fn auto() -> Self {
        Self {
            kind: "auto",
            name: None,
            disable_parallel_tool_use: true,
        }
    }

    pub fn force(name: &'static str) -> Self {
        Self {
            kind: "tool",
            name: Some(name),
            disable_parallel_tool_use: true,
        }
    }
}
