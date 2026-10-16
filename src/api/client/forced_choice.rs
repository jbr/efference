//! `complete` and its forced-choice fallback (docs/transport.md): a model that refuses a
//! forced `tool_choice` is asked again as `auto` with an instruction, and the client
//! remembers. [`SentRequest`] is a request as it went out, which the triage keepalive
//! re-sends.

use super::*;
use crate::project::UserPart;

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
        self.unforced()
            .send(&turns, tools, ToolChoice::auto(), None, on_delta)
            .await
    }

    /// This client as the fallback sends: thinking off, which the forced request had,
    /// and off the cache chain, since the fallback's body is not the turn's prompt.
    pub(super) fn unforced(&self) -> Client {
        self.clone().without_thinking().without_cache_chain()
    }

    /// The request a [`Self::complete`] over these inputs went out as, read after it
    /// returned (docs/transport.md): as given, or — once this client has learned the
    /// model refuses a forced choice — `auto`, with [`forced_choice_instruction`]
    /// appended and thinking off. The lesson is only ever learned by a fallback that
    /// succeeded, so after an answered `complete` the flag says which form its last
    /// attempt took.
    pub(crate) fn sent_form(
        &self,
        turns: Vec<WireTurn>,
        tools: &[Value],
        choice: ToolChoice,
    ) -> SentRequest {
        use std::sync::atomic::Ordering;
        match choice.forced() {
            Some(forced) if self.forced_choice_refused.load(Ordering::Relaxed) => {
                let mut turns = turns;
                turns.push(WireTurn::text(forced_choice_instruction(forced)));
                SentRequest {
                    turns,
                    tools: tools.to_vec(),
                    choice: ToolChoice::auto(),
                    unforced: true,
                }
            }
            _ => SentRequest::new(turns, tools.to_vec(), choice),
        }
    }
}

/// One request as a client sends it (docs/transport.md): its turns, `tools[]` and
/// `tool_choice`, and whether it went out as the forced-choice fallback, which also
/// turns thinking off. The keepalive pings one: the main lineage's built from the
/// projection each tick, the triage lineage's stored when its request was sent.
#[derive(Clone, Debug, PartialEq)]
pub struct SentRequest {
    pub turns: Vec<WireTurn>,
    pub tools: Vec<Value>,
    pub choice: ToolChoice,
    /// Sent by the forced-choice fallback: `choice` is the `auto` it went out with, and
    /// the request carried no thinking.
    pub unforced: bool,
}

impl SentRequest {
    /// A request sent as given.
    pub fn new(turns: Vec<WireTurn>, tools: Vec<Value>, choice: ToolChoice) -> Self {
        Self {
            turns,
            tools,
            choice,
            unforced: false,
        }
    }

    /// This request ending at its last cache breakpoint (docs/transport.md): everything
    /// after it is dropped, so a re-send reads the cached prefix the request wrote and
    /// writes nothing past it. The breakpoint is a [`WireTurn::Breakpoint`] or a
    /// [`UserPart::Breakpoint`] inside a user turn, whose later parts go too. `None`
    /// when the request carries no breakpoint.
    pub fn through_last_breakpoint(mut self) -> Option<Self> {
        let end = self.turns.iter().rposition(|turn| match turn {
            WireTurn::Breakpoint => true,
            WireTurn::User(parts) => parts.iter().any(|p| matches!(p, UserPart::Breakpoint)),
            WireTurn::Assistant(_) => false,
        })?;
        self.turns.truncate(end + 1);
        if let Some(WireTurn::User(parts)) = self.turns.last_mut()
            && let Some(last) = parts
                .iter()
                .rposition(|p| matches!(p, UserPart::Breakpoint))
        {
            parts.truncate(last + 1);
        }
        Some(self)
    }
}
