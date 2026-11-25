//! Thinking off learned from the endpoint's refusal (docs/transport.md). A model
//! whose metadata spells thinking off with `between_tools` can refuse that spelling
//! too, so a rejection naming the thinking type is retried once with adaptive
//! thinking at effort `low`; when that retry succeeds, the client sends that form
//! from then on.

use std::pin::Pin;
use std::sync::atomic::Ordering;

use super::forced_choice::forced_choice_instruction;
use super::*;
use tokio::sync::futures::Notified;

/// The effort the recovery sends beside adaptive thinking (docs/transport.md). The
/// request had spelled thinking off, so the retry's level is the lowest one rather
/// than the model's default.
const LOW_EFFORT: &str = "low";

/// Whether `error` is the endpoint's refusal of a thinking-off spelling: an HTTP 400
/// whose message names the thinking type, the answer the separate-model result
/// triage's ladder matches for its first step (`TriageReasoning::Off`). Any other
/// rejection is not this recovery's to answer, so an unrelated 400 costs nothing
/// (docs/transport.md).
fn thinking_off_refusal(error: &Error) -> bool {
    TriageReasoning::Off.rejection(error).is_some()
}

impl Client {
    /// Whether a request that spells thinking off is the recovery's to answer: the
    /// dialect has a spelling the endpoint can refuse — only Messages spells thinking
    /// off, as `disabled` or `between_tools`, while Responses and Completions send no
    /// such field — the model takes adaptive thinking, and no separate-model result
    /// triage is present. That triage's off → `low` → unset sequence has the
    /// refusal's first step, recorded in `retries`, and a retry on top of the ladder
    /// would take that step (docs/transport.md).
    fn recovers_thinking_off(&self, tool_choice: &ToolChoice) -> bool {
        self.dialect() == WireDialect::Messages
            && self.thinking.adaptive
            && self.foreground_triage.is_none()
            && matches!(
                Thinking::for_request(tool_choice, self.thinking, self.thinking_off),
                Thinking::Disabled | Thinking::BetweenTools
            )
    }

    /// Whether this request goes out in the recovered form: the endpoint refused this
    /// client's thinking-off spelling and a retry at adaptive `low` cured it
    /// (docs/transport.md). An `auto` request only — a forced choice cannot carry
    /// thinking on, so it keeps its own spelling until it is sent as `auto` with
    /// [`forced_choice_instruction`], which [`Self::sends_forced_as_auto`] arranges.
    fn recovered(&self, tool_choice: &ToolChoice) -> bool {
        self.thinking_off_refused.load(Ordering::Relaxed)
            && tool_choice.forced().is_none()
            && self.recovers_thinking_off(tool_choice)
    }

    /// The `thinking` this request sends: the recovered form where this client has
    /// recorded a refusal of its thinking-off spelling (docs/transport.md).
    pub(super) fn request_thinking(&self, tool_choice: &ToolChoice) -> Thinking {
        if self.recovered(tool_choice) {
            THINKING_ADAPTIVE
        } else {
            Thinking::for_request(tool_choice, self.thinking, self.thinking_off)
        }
    }

    /// The effort this request carries: `low` where the recovery applies, the
    /// session's own otherwise (docs/transport.md).
    pub(super) fn request_effort(&self, tool_choice: &ToolChoice) -> Option<String> {
        if self.recovered(tool_choice) {
            Some(LOW_EFFORT.to_string())
        } else {
            self.effort.clone()
        }
    }

    /// This client as the recovery sends it: thinking on, at effort `low` — the form
    /// that stands in for the refused spelling. The flag is not set until the retry
    /// succeeds, so the retry is this client's ordinary path with those two settings
    /// changed.
    fn recovered_client(&self) -> Client {
        let mut client = self.clone();
        client.thinking_off = false;
        client.effort = Some(LOW_EFFORT.to_string());
        client
    }

    /// [`Self::send_request`], with the thinking-off refusal learned on the way
    /// (docs/transport.md). A rejection naming the thinking type this request sent
    /// ([`thinking_off_refusal`]) is retried once with adaptive thinking at effort
    /// `low`; a forced choice goes as `auto` with [`forced_choice_instruction`],
    /// since a forced choice and thinking on are a 400 together. When the retry
    /// succeeds the client sends that form from then on, so the refusal is paid once;
    /// when it fails too, the rejection had another cause, the first error is
    /// returned, and nothing is learned. Every attempt reads against `abort`, the
    /// interrupt [`Self::send`] armed.
    pub(super) async fn send_with_thinking_fallback(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        cap: Option<u32>,
        mut abort: Pin<&mut Notified<'_>>,
        mut on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        if self.thinking_off_refused.load(Ordering::Relaxed)
            || !self.recovers_thinking_off(&tool_choice)
        {
            return self
                .send_request(turns, tools, tool_choice, cap, abort, on_delta)
                .await;
        }
        let refusal = match self
            .send_request(
                turns,
                tools,
                tool_choice,
                cap,
                abort.as_mut(),
                &mut on_delta,
            )
            .await
        {
            Err(e) if thinking_off_refusal(&e) => e,
            answered => return answered,
        };
        let forced = tool_choice.forced();
        let mut turns = turns.to_vec();
        let tool_choice = match forced {
            Some(forced) => {
                turns.push(WireTurn::text(forced_choice_instruction(forced)));
                ToolChoice::auto()
            }
            None => tool_choice,
        };
        // The instruction changes the turns, so a forced retry is off the cache chain
        // as the forced-choice fallback is; an `auto` retry sends the turns the
        // request did and stays on it.
        let client = match forced {
            Some(_) => self.clone().without_cache_chain(),
            None => self.clone(),
        };
        match client
            .recovered_client()
            .send_request(&turns, tools, tool_choice, cap, abort, on_delta)
            .await
        {
            Ok(completion) => {
                log::warn!(
                    "{} refused thinking off ({refusal}); this client sends adaptive \
                     thinking at effort `{LOW_EFFORT}` from here on",
                    self.model
                );
                self.thinking_off_refused.store(true, Ordering::Relaxed);
                Ok(completion)
            }
            Err(_) => Err(refusal),
        }
    }
}
