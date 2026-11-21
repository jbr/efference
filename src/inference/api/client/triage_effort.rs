//! Result triage reasoning (docs/triage.md): the separate model's thinking off →
//! `low` → unset sequence, and the shared triage's `low` effort with the cache
//! evidence that withdraws it.

use super::*;
use crate::inference::api::error_status;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, PoisonError};
use tokio::time::{Duration, Instant};

/// How long after a turn a shared triage's cache read still counts as evidence about
/// effort: under the five-minute TTL, so a prefix that expired while the tools ran is not
/// read as a prefix the effort moved (docs/triage.md).
const EVIDENCE_WINDOW: Duration = Duration::from_secs(240);

/// The reasoning setting a separate-model result triage request was sent with
/// (docs/triage.md). The discriminants order the fallback sequence: the floor stored
/// for a route is the furthest setting a completed fallback has used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TriageReasoning {
    /// The model's explicit Messages spelling of thinking off.
    Off = 0,
    /// Effort `low`.
    Low = 1,
    /// No effort field: the provider's default.
    Unset = 2,
}

impl TriageReasoning {
    /// The next setting to try when `error` is an HTTP 400 whose message names this
    /// setting. `None` for any other error.
    pub(super) fn rejection(self, error: &Error) -> Option<Self> {
        if error_status(error) != Some(400) {
            return None;
        }
        let message = error.to_string().to_ascii_lowercase();
        let has = |name: &str| {
            message
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word == name)
        };
        match self {
            Self::Off if has("thinking") && (has("disabled") || has("between_tools")) => {
                Some(Self::Low)
            }
            Self::Low if has("effort") || has("reasoning_effort") => Some(Self::Unset),
            _ => None,
        }
    }

    fn breadcrumb(self) -> &'static str {
        match self {
            Self::Off => "upstream rejected thinking off for triage",
            Self::Low => "upstream rejected triage effort `low`",
            Self::Unset => unreachable!("no automatic setting left to reject"),
        }
    }
}

/// The effort a shared result triage was sent with (docs/triage.md). The thinking
/// type is always the agent's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SharedEffort {
    Low,
    /// The agent's own effort: the control, or the route's settled fallback.
    Inherited,
}

/// What shared result triage has established about effort `low` on this client's route
/// (docs/triage.md). [`Self::Inherit`] requires a contrast: a `low` read short of the
/// turn's prefix, a control at the inherited effort that reads it, and a second short
/// `low` read. A short control means the misses have another cause, so the count restarts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum LowEffortEvidence {
    /// Nothing established: the next shared triage sends `low`.
    #[default]
    Untested,
    /// The last `low` read short: the next shared triage is the control.
    ControlNext,
    /// A short `low` read, then a control that read the prefix: one more short `low`
    /// read completes the contrast.
    Contrasted,
    /// A `low` read covered the turn's prefix: `low` does not move this route's cache.
    Neutral,
    /// The contrast held, or the route refused `low`: shared triage inherits the effort.
    Inherit,
}

impl LowEffortEvidence {
    fn sends(self) -> SharedEffort {
        match self {
            Self::ControlNext | Self::Inherit => SharedEffort::Inherited,
            Self::Untested | Self::Contrasted | Self::Neutral => SharedEffort::Low,
        }
    }

    /// The state after a triage sent at `sent` read the turn's prefix (`full`) or fell
    /// short of it.
    pub(crate) fn observe(self, sent: SharedEffort, full: bool) -> Self {
        match (self, sent, full) {
            (Self::Untested | Self::Contrasted, SharedEffort::Low, true) => Self::Neutral,
            (Self::Untested, SharedEffort::Low, false) => Self::ControlNext,
            (Self::Contrasted, SharedEffort::Low, false) => Self::Inherit,
            (Self::ControlNext, SharedEffort::Inherited, true) => Self::Contrasted,
            (Self::ControlNext, SharedEffort::Inherited, false) => Self::Untested,
            (state, ..) => state,
        }
    }
}

/// The prefix the client's last finished turn left cached, and when it finished: what the
/// next shared triage's read is measured against (docs/triage.md).
#[derive(Clone, Copy, Debug)]
pub(super) struct TurnCache {
    tokens: u64,
    at: Instant,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Client {
    pub(super) fn for_triage_reasoning(&self, mode: TriageReasoning) -> Self {
        let mut client = self.clone().with_effort(match mode {
            TriageReasoning::Low => Some("low".into()),
            _ => None,
        });
        client.thinking_off = mode == TriageReasoning::Off;
        if mode != TriageReasoning::Off {
            // After off was refused, do not send the same field under a forced choice.
            client.thinking.off = ThinkingOff::Omit;
        }
        // Even a forced-choice fallback request must not include the gateway's switch
        // that disables reasoning on a model whose thinking control is omitted.
        client.foreground_triage = Some(mode);
        client
    }

    fn triage_reasoning(&self) -> TriageReasoning {
        match self.triage_reasoning_floor.load(Ordering::Relaxed) {
            2 => TriageReasoning::Unset,
            0 if self.dialect() == WireDialect::Messages
                && self.thinking.off != ThinkingOff::Omit =>
            {
                TriageReasoning::Off
            }
            _ => TriageReasoning::Low,
        }
    }

    /// Send a separate-model result triage at the lowest setting this dialect exposes,
    /// without using the main effort even when the provider rejects an override. Only
    /// learn from a completed fallback; an unrelated rejection must not trigger another
    /// send.
    pub(crate) async fn complete_separate_triage(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        choice: ToolChoice,
        mut on_delta: impl FnMut(Streamed),
        retries: &mut Vec<String>,
    ) -> (Result<Completion, Error>, TriageReasoning) {
        let mut mode = self.triage_reasoning();
        let mut first_refusal = None;
        loop {
            let answer = self
                .for_triage_reasoning(mode)
                .complete(turns, tools, choice, &mut on_delta)
                .await;
            match answer {
                Err(error) => {
                    let Some(next) = mode.rejection(&error) else {
                        return (Err(first_refusal.unwrap_or(error)), mode);
                    };
                    retries.push(mode.breadcrumb().into());
                    first_refusal.get_or_insert(error);
                    mode = next;
                }
                Ok(completion) => {
                    if first_refusal.is_some() && completion.upstream_failure().is_none() {
                        self.triage_reasoning_floor
                            .fetch_max(mode as u8, Ordering::Relaxed);
                    }
                    return (Ok(completion), mode);
                }
            }
        }
    }

    /// Record the prefix a finished turn left cached (docs/triage.md). Called where a
    /// stream finishes, beside the cache chain, so every dialect's turn records it; a
    /// request of any other origin, or a stream that died, records nothing.
    pub(super) fn remember_turn_cache(&self, origin: RequestOrigin, completion: &Completion) {
        if origin != RequestOrigin::Turn || completion.upstream_failure().is_some() {
            return;
        }
        let usage = &completion.usage;
        *lock(&self.turn_cache) = Some(TurnCache {
            tokens: usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
            at: Instant::now(),
        });
    }

    /// Whether a triage that read `read` tokens from the cache read the last turn's
    /// whole prefix: at least what that turn left cached. Strict, because a triage on a
    /// route that `low` splits can still read most of the prefix, from an earlier `low`
    /// triage's write a turn or two back. `None` when there is nothing to measure
    /// against — no turn yet, a turn that cached nothing (a route without a cache), or
    /// one older than [`EVIDENCE_WINDOW`].
    fn reads_turn_prefix(&self, read: u64) -> Option<bool> {
        let turn = (*lock(&self.turn_cache))?;
        (turn.tokens > 0 && turn.at.elapsed() < EVIDENCE_WINDOW).then_some(read >= turn.tokens)
    }

    /// Move the recorded turn `by` into the past, as if the tools had run that long.
    #[cfg(test)]
    pub(crate) fn age_turn_cache(&self, by: Duration) {
        if let Some(turn) = lock(&self.turn_cache).as_mut() {
            turn.at = turn
                .at
                .checked_sub(by)
                .expect("a turn recorded after the clock's epoch");
        }
    }

    /// The route's evidence about shared-triage effort `low`.
    pub(crate) fn low_effort_evidence(&self) -> LowEffortEvidence {
        *lock(&self.low_effort_evidence)
    }

    /// Send a shared result triage (docs/triage.md): the agent's own request with only
    /// the effort changed to `low`, unless this route's evidence shows that `low` moves its
    /// cache. `effort` is the triage's choice, made on its first request and kept for
    /// the rest of that triage's requests; only the first request's read is evidence,
    /// since a later attempt reads the fork's own tail.
    pub(crate) async fn complete_shared_triage(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        choice: ToolChoice,
        mut on_delta: impl FnMut(Streamed),
        effort: &mut Option<SharedEffort>,
        retries: &mut Vec<String>,
    ) -> Result<Completion, Error> {
        if self.effort.as_deref() == Some("low") {
            // `low` is already the agent's effort: nothing to change or learn.
            return self.complete(turns, tools, choice, on_delta).await;
        }
        let first = effort.is_none();
        let sent = *effort.get_or_insert_with(|| self.low_effort_evidence().sends());
        let completion = match sent {
            SharedEffort::Inherited => self.complete(turns, tools, choice, &mut on_delta).await?,
            SharedEffort::Low => {
                let low = self.clone().with_effort(Some("low".into()));
                match low.complete(turns, tools, choice, &mut on_delta).await {
                    Err(error) if TriageReasoning::Low.rejection(&error).is_some() => {
                        retries.push("upstream rejected shared triage effort `low`".into());
                        *effort = Some(SharedEffort::Inherited);
                        let answer = self.complete(turns, tools, choice, on_delta).await;
                        if answer
                            .as_ref()
                            .is_ok_and(|completion| completion.upstream_failure().is_none())
                        {
                            *lock(&self.low_effort_evidence) = LowEffortEvidence::Inherit;
                        }
                        return answer;
                    }
                    answer => answer?,
                }
            }
        };
        if first
            && completion.upstream_failure().is_none()
            && let Some(full) = self.reads_turn_prefix(completion.usage.cache_read_input_tokens)
        {
            let mut evidence = lock(&self.low_effort_evidence);
            let next = evidence.observe(sent, full);
            if next == LowEffortEvidence::Inherit && *evidence != next {
                retries.push(
                    "shared triage effort `low` read short of the turn's cached prefix twice, \
                     around a control at the agent's effort that read it; inheriting the \
                     agent's effort"
                        .into(),
                );
            }
            *evidence = next;
        }
        Ok(completion)
    }
}
