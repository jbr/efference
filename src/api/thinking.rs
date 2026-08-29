//! Extended-thinking selection per model, and the tool-choice knob (docs/transport.md).

use super::*;

/// The `thinking` field (docs/transport.md). `Adaptive` lets the model decide
/// when and how much to think; `display: "summarized"` captures a readable
/// model-authored summary into the log at the same token cost as omitting it.
/// `Omit` is distinct from `Disabled`: it leaves the field off the request
/// entirely (via the `Request` field's `skip_serializing_if`), which is the only
/// safe spelling for foreign models, where an explicit `disabled` is rejected
/// outright by mandatory-reasoning endpoints.
#[derive(Serialize, Clone, Copy)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Thinking {
    Disabled,
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<&'static str>,
    },
    /// Never serialized — `is_omit` gates the field off the request.
    Omit,
}

impl Thinking {
    pub(super) fn is_omit(&self) -> bool {
        matches!(self, Self::Omit)
    }

    /// The thinking matrix over `tool_choice`, the model family, and the boot
    /// capability lookup (docs/transport.md). The one asymmetry to hold onto
    /// while reading the `match`: Claude falls through to explicit `disabled`,
    /// foreign models fall through to `Omit` — a mandatory-reasoning endpoint
    /// rejects an explicit disable outright, so the two non-adaptive answers are
    /// not interchangeable.
    ///
    /// An explicit `EFFERENCE_THINKING` overrides the `auto`-turn decision either way.
    pub(super) fn for_request(
        model: &str,
        tool_choice: &ToolChoice,
        model_reasoning: bool,
    ) -> Self {
        let adaptive = adaptive_on_auto(model, model_reasoning);
        match tool_choice.kind {
            "auto" if adaptive => THINKING_ADAPTIVE,
            _ if is_claude_family(model) => THINKING_DISABLED,
            _ => Thinking::Omit,
        }
    }

    /// The keepalive's thinking (docs/transport.md): explicit `disabled` on Claude,
    /// since `max_tokens: 0` rejects thinking-on, and the field omitted for foreign
    /// models — the same "never `disabled` off-family" rule as [`Self::for_request`].
    pub(super) fn for_keepalive(model: &str) -> Self {
        if is_claude_family(model) {
            THINKING_DISABLED
        } else {
            Thinking::Omit
        }
    }
}

/// The `output_config` object (docs/transport.md) — present on the request only
/// when the session set an effort, so a session that sets none sends the same
/// bytes it always did. One field today; the wrapper exists because the API
/// nests `effort` inside it.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    pub effort: Effort,
}

/// How much effort the model spends on a turn (docs/transport.md) — the *level*
/// axis, orthogonal to [`Thinking`]'s *type* axis. Set once at boot from
/// `--effort` or `[transport] effort`, never mutated for the life of the session,
/// so it cannot move a live cached prefix.
///
/// One coupling with the other axis: `Xhigh` and `Max` require thinking enabled,
/// and Anthropic answers the pairing with a 400 — [`check_effort_thinking`]
/// rejects it at boot instead.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// The five spellings, in ascending order — the vocabulary the flag, the
    /// config key, and the rejection message all read from.
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    /// The token the wire, the config, and the flag spell it with.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// Whether this level is legal only with thinking enabled. The top two are:
    /// they describe how far a think may run, so there is nothing for them to
    /// scale when thinking is off.
    pub fn requires_thinking(self) -> bool {
        matches!(self, Effort::Xhigh | Effort::Max)
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Effort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str() == s.trim())
            .ok_or_else(|| {
                format!(
                    "unknown effort {s:?} — expected one of {}",
                    Self::ALL.map(Effort::as_str).join(", ")
                )
            })
    }
}

/// The boot check for the one coupling between the two axes (docs/transport.md):
/// `xhigh`/`max` with thinking off is a 400 from Anthropic, so a session
/// configured that way is refused at startup with a message naming both halves
/// rather than failing on its first turn. Pure in `thinking_off` so the caller
/// supplies the session's effective answer — which is
/// [`auto_turns_disable_thinking`], covering both routes to disabled: the
/// `EFFERENCE_THINKING=off` override, and a Claude-family model whose turns fall
/// through to explicit `disabled` (Haiku, or any model the capability lookup
/// denies reasoning).
pub fn check_effort_thinking(effort: Option<Effort>, thinking_off: bool) -> Result<(), String> {
    match effort {
        Some(effort) if thinking_off && effort.requires_thinking() => Err(format!(
            "effort `{effort}` requires thinking, but this session's turns send thinking \
             disabled (EFFERENCE_THINKING=off, or a model that doesn't get adaptive thinking) \
             — the provider answers that pairing with a 400. Use `high` or lower, or enable \
             thinking."
        )),
        _ => Ok(()),
    }
}

/// Whether the session's turns will carry thinking **disabled** — the effective
/// answer [`check_effort_thinking`] needs, mirroring [`Thinking::for_request`]'s
/// `auto` arm through the shared [`adaptive_on_auto`]: not adaptive, on a
/// Claude-family model (the fall-through to explicit `disabled`). A foreign model
/// without adaptive gets the field *omitted*, not disabled — the endpoint decides,
/// so effort there is the unverified-translation watch-item (docs/transport.md),
/// not this check's business.
pub fn auto_turns_disable_thinking(model: &str, model_reasoning: bool) -> bool {
    !adaptive_on_auto(model, model_reasoning) && is_claude_family(model)
}

/// Whether an `auto`-choice turn thinks: the `EFFERENCE_THINKING` override when
/// set, else the recognized-adaptive-Claude rule or the capability lookup's
/// reasoning grant. The one place this is decided — [`Thinking::for_request`] and
/// [`auto_turns_disable_thinking`] both read it, so the boot check cannot drift
/// from what the turns actually send.
fn adaptive_on_auto(model: &str, model_reasoning: bool) -> bool {
    THINKING_OVERRIDE.unwrap_or_else(|| is_recognized_adaptive_claude(model) || model_reasoning)
}

/// A Claude-family model id — direct (`claude-…`) or provider-scoped
/// (`anthropic/…`). These speak the Anthropic `thinking` block natively, so an
/// explicit `disabled` is safe and, with forced tool choice, required. Everything
/// else gets the field omitted rather than disabled.
pub(super) fn is_claude_family(model: &str) -> bool {
    model.contains("claude") || model.contains("anthropic")
}

/// The Claude half of [`Thinking::for_request`]'s `auto` default: a Claude family
/// model (direct id or `anthropic/…` slug) that isn't Haiku. Foreign models
/// (`openai/…`, `google/…`, …) fall outside it and are governed by the capability
/// lookup instead.
///
/// Adaptive thinking (`{type:"adaptive"}`) is a Claude **4.6+** feature; earlier
/// tiers speak the older `{type:"enabled", budget_tokens}` API and **400 on
/// adaptive**. This substring rule deliberately does not encode that version floor
/// — parsing a version out of both `claude-opus-4-8` and `anthropic/claude-opus-4.1`
/// is its own fragile problem — so it assumes the Claude generation efference
/// serves. [`Thinking`] cannot express budgeted thinking at all, so an older Claude
/// wants thinking off: `EFFERENCE_THINKING=off` covers that case.
pub(super) fn is_recognized_adaptive_claude(model: &str) -> bool {
    (model.contains("claude") || model.contains("anthropic")) && !model.contains("haiku")
}

/// The `tool_choice` field. `disable_parallel_tool_use` is always set, so the
/// model emits **at most one** `tool_use` per turn (docs/README.md) — never
/// varied, so it never invalidates the cache. `auto()` lets the model decide
/// whether to call a tool; `force(name)` makes it call exactly that one, which
/// only the spotter's `spot` does (docs/permissions.md) — forcing moves the
/// prefix relative to the shared envelope every other call type sends.
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
