//! `complete` and its forced-choice fallback (docs/transport.md): a model that refuses a
//! forced `tool_choice` is asked again as `auto` with an instruction, and the client
//! remembers.

use super::*;

/// The user line a forced call sent as `auto` ends with (docs/transport.md).
pub(crate) fn forced_choice_instruction(tool: &str) -> String {
    format!("Answer by calling the `{tool}` tool, and only that tool.")
}

impl Client {
    /// One streaming round-trip: serialize `messages`, POST with `stream: true`,
    /// and fold the SSE event stream back into a [`Completion`]. Each live signal
    /// — prose, thinking, a tool call opening, its arguments arriving — is handed
    /// to `on_delta` as a [`Streamed`] the moment it arrives: the transport-only
    /// live sink the daemon publishes to the control stream's `output` class.
    /// Pass a no-op `|_| {}` when nothing is listening (the triage, whose
    /// triage tokens must never surface).
    ///
    /// `into_sse` *is* the execution — the conn must not be awaited first (that
    /// is `SseErrorKind::AlreadyExecuted`). On a non-2xx it returns
    /// `SseErrorKind::Status` carrying the code; the failed `SseError` still owns the
    /// `Conn` (`SseError: Into<Conn>`), so [`Self::sse_open_failure`] reads the response
    /// body off it — Anthropic's 4xx carry a JSON `error.message` naming exactly what
    /// was malformed (e.g. an oversized request that exceeds the input limit), which is
    /// what makes such failures diagnosable.
    ///
    /// A forced `tool_choice` the model rejects ([`is_request_rejection`]) is
    /// retried once as
    /// `auto`, with a user line naming the tool and thinking kept off as the forced
    /// call had it (docs/transport.md). Some models refuse forced choice outright
    /// and their metadata does not say so; the retry is how the client learns it.
    /// When the retry succeeds, the client remembers, and later forced requests go
    /// straight to the fallback. When it fails too, the rejection had another cause: the
    /// first error is returned and nothing is learned.
    pub async fn complete(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        mut on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        use std::sync::atomic::Ordering;
        let Some(forced) = tool_choice.forced() else {
            return self.send(turns, tools, tool_choice, None, on_delta).await;
        };
        if self.forced_choice_refused.load(Ordering::Relaxed) {
            return self
                .send_unforced(turns, tools, forced, &mut on_delta)
                .await;
        }
        let refusal = match self
            .send(turns, tools, tool_choice, None, &mut on_delta)
            .await
        {
            Err(e) if is_request_rejection(&e) => e,
            answered => return answered,
        };
        match self
            .send_unforced(turns, tools, forced, &mut on_delta)
            .await
        {
            Ok(completion) => {
                log::warn!(
                    "{} refused a forced tool_choice ({refusal}); this client sends `auto` \
                     with an instruction from here on",
                    self.model
                );
                self.forced_choice_refused.store(true, Ordering::Relaxed);
                Ok(completion)
            }
            Err(_) => Err(refusal),
        }
    }

    /// A forced call sent as `auto`: the turns plus [`forced_choice_instruction`],
    /// with thinking off, which is what the forced request would have sent.
    async fn send_unforced(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        forced: &str,
        on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        let mut turns = turns.to_vec();
        turns.push(WireTurn::text(forced_choice_instruction(forced)));
        self.clone()
            .without_thinking()
            .without_cache_chain()
            .send(&turns, tools, ToolChoice::auto(), None, on_delta)
            .await
    }
}
