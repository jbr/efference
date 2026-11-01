//! Which API family fronts a session's endpoint, and the wire dialect it speaks
//! (docs/transport.md).
//!
//! A boot preamble and a log header record the provider by name ([`LoggedInference`]), and
//! an assistant turn is recorded in the arm for its dialect ([`AssistantOutput`]), so the
//! two enums are part of what the log is written in. What a provider *does* — its URLs,
//! auth, cache lifetime, placements and defaults — is the transport's: the `impl` blocks,
//! including the serde impls that write and read a provider's name, are in
//! `src/inference/api/provider.rs` and `src/inference/api/provider/dialect.rs`.
//!
//! [`LoggedInference`]: super::LoggedInference
//! [`AssistantOutput`]: super::AssistantOutput

/// The request/response shape a provider speaks (docs/transport.md). Derived
/// from the declared provider — there is no independent knob — so one session
/// projects and assembles in exactly one dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireDialect {
    /// Anthropic Messages: `system` + `messages[]`, content blocks, SSE
    /// `message_start` … `message_stop`.
    Messages,
    /// OpenAI Responses: `input[]` items, ordered output items, SSE
    /// `response.created` … `response.completed`.
    Responses,
    /// Chat Completions: `messages[]` with `role: "tool"` results, assistant
    /// messages replayed as received, SSE `choices[0].delta` chunks ending in
    /// `[DONE]` (docs/transport.md).
    Completions,
    /// The System One contract (docs/transport.md): one POST of
    /// `{model, state, questions}` answered with a probability per question.
    /// Not a turn dialect — it has no assistant output to encode or assemble —
    /// so [`Provider::inference_dialect`] never returns it, and a connection
    /// speaking it can back the spotter role only.
    SystemOne,
}

/// Which API family fronts the messages endpoint. Each variant knows its
/// messages URL, its auth header shape, and its model-metadata endpoint — the
/// operator declares one word and everything else is derived.
///
/// Serializes as its lowercase name ([`Provider::name`]), for state and boot
/// reports; a request body never carries it (the dialect encoder is what a
/// provider selects).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Anthropic's own API: `x-api-key` auth, metadata at `GET /v1/models/{model}`.
    Anthropic,
    /// OpenRouter's Anthropic-Messages skin (`/api/v1/messages`): `authorization:
    /// Bearer` auth, metadata at `GET /api/v1/models/{author}/{slug}/endpoints`
    /// (ceilings) plus `GET /api/v1/model/{author}/{slug}` (effort levels).
    OpenRouter,
    /// OpenAI's Responses API (`/v1/responses`): `authorization: Bearer` auth,
    /// capabilities from the compiled catalog in `provider/openai.rs` because
    /// the Models endpoint reports none of the limits the boot path consumes.
    OpenAI,
    /// The Codex backend (`chatgpt.com/backend-api/codex/responses`): the same
    /// Responses wire, billed to a ChatGPT/Codex subscription instead of the
    /// platform balance. Auth is the OAuth access token Codex CLI caches in
    /// `~/.codex/auth.json` plus its `ChatGPT-Account-ID`; the request shape
    /// mirrors what Codex CLI sends
    /// ([`ResponsesProfile::Codex`](crate::inference::api::ResponsesProfile::Codex)).
    OpenAICodex,
    /// TypeSafe's System One API (`/v1/systemone`): `authorization: Bearer`
    /// auth, and **no inference dialect at all**. It serves decision models —
    /// state plus typed questions in, a calibrated probability per question out
    /// — so it can back the spotter rung (docs/permissions.md) and nothing else.
    /// [`Self::inference_dialect`] returns `None` for it, and resolution refuses
    /// it for the main and triage roles before anything downstream could ask.
    TypeSafe,
    /// An endpoint the operator declared rather than one this enum knows: a wire
    /// dialect plus a base URL from `[connections.<name>]`
    /// (docs/transport.md). The dialect is the payload because it is the one fact
    /// no default can supply — everything else a custom connection needs is either
    /// owned data on [`Endpoint`](crate::inference::api::Endpoint) (the URL, the auth header,
    /// extra headers) or derived from the dialect here.
    ///
    /// Every per-provider quirk takes its conservative arm for this variant, and
    /// derives from the dialect rather than being inherited from a named provider
    /// that happens to speak the same one. Those values — a sticky-routing header,
    /// an attribution header, a cache-key field, a placement workaround — are
    /// facts about one vendor's deployment, so copying OpenRouter's set onto an
    /// arbitrary endpoint would send it another vendor's workarounds.
    Custom(WireDialect),
}
