//! The decision-model spotter (docs/permissions.md).
//!
//! Same rung, different machine. [`ForkSpot`](super::ForkSpot) asks a generative
//! model for a stance and a sentence; this asks a **decision model** — an LLM with
//! generation replaced by a classifier head — a fixed set of boolean questions and
//! gets a calibrated probability for each. It answers in tens of milliseconds for
//! a fraction of a cent, and it cannot be talked to: there is no system prompt to
//! shape and no prose to read, only questions and numbers.
//!
//! ```text
//!   state (the config + the move, as JSON)  ──▶  P(in_scope), P(reversible), …
//!                                                        │
//!                                          thresholds ───┴──▶  Stance + reason
//! ```
//!
//! The consequences are the interesting part, and they are why this is a separate
//! implementation rather than a model swap:
//!
//! - **The judgement is decomposed.** No single question can be "what is your
//!   stance?", because the stance is a policy over several considerations. The
//!   axes are asked separately and folded in code (`questions.rs`).
//! - **The reason is synthesized.** The model emits no prose, so the one line the
//!   acting session and the human gate read is written here from the deciding axis
//!   and its figure — what the model actually reported, not a sentence it wrote.
//! - **Nothing caches.** The route has no cache-control and the model's endpoint
//!   reports none, so every call pays full input. At this price that is not a
//!   cost question, but it does mean the persona/scope prefix economics that shape
//!   `ForkSpot` have no analogue here.
//!
//! Fail-closed is unchanged: any transport error, any missing answer, and the
//! rung returns [`Origin::Fallback`] + [`Stance::Gate`](crate::log::Stance::Gate).

use crate::api::Endpoint;
use crate::config::Config;
use crate::log::{Origin, ToolCall};
use crate::tools::ToolBrief;

use super::{Read, Spotter, Spotting};

mod ask;
mod questions;
mod state;

pub use questions::{Axes, Band, Thresholds};

/// The decision-model spotter. Holds the [`Endpoint`] rather than a
/// [`Client`](crate::api::Client): the decisions route is one JSON POST with no
/// system prompt, no tools, and no stream, so none of the client's machinery
/// applies (`src/api/decisions.rs`).
pub struct JevSpot {
    endpoint: Endpoint,
    model: String,
    /// The session scratch dir (docs/tools.md), when one exists — the state's
    /// `scratch_directory`, which is what connects a `$TMPDIR/…` move to the
    /// granted literal path.
    scratch: Option<std::path::PathBuf>,
    /// The judged session's working directory, which is not always this
    /// process's: the offline eval replays cases recorded in other checkouts
    /// (docs/permissions.md). `None` falls back to the process cwd.
    cwd: Option<std::path::PathBuf>,
    thresholds: Thresholds,
}

impl JevSpot {
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Self {
        JevSpot {
            endpoint,
            model: model.into(),
            scratch: None,
            cwd: None,
            thresholds: Thresholds::default(),
        }
    }

    pub fn with_scratch(mut self, scratch: Option<std::path::PathBuf>) -> Self {
        self.scratch = scratch;
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<std::path::PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Replace the decision band — the offline eval's tuning knob, the analogue of
    /// `ForkSpot::with_persona` (docs/permissions.md). Production always boots the
    /// default; nothing routes a substitute band into a live session.
    pub fn with_thresholds(mut self, thresholds: Thresholds) -> Self {
        self.thresholds = thresholds;
        self
    }
}

impl Spotter for JevSpot {
    async fn assess(
        &self,
        role: &Config,
        call: &ToolCall,
        brief: Option<&ToolBrief<'_>>,
        chain: &[String],
        probe: Option<&str>,
    ) -> Spotting {
        let cwd = self.cwd.clone().or_else(|| std::env::current_dir().ok());
        let state = state::state(
            role,
            call,
            brief,
            chain,
            probe,
            cwd.as_deref(),
            self.scratch.as_deref(),
        );
        let watch_for = role.watch_for.iter().any(|w| !w.trim().is_empty());
        let questions = ask::questions(watch_for, probe.is_some());

        let decisions = match self.endpoint.decide(&self.model, &state, &questions).await {
            Ok(decisions) => decisions,
            Err(error) => {
                log::warn!("spotter decisions request failed: {error}");
                return Spotting::fail_closed(None);
            }
        };
        let usage = Some(decisions.request_usage());

        // Every unconditional axis must have come back. A partial answer set is
        // not a partial verdict — the fold reads all of them — so it fails closed
        // exactly as an unparseable `spot` call does.
        let Some(axes) = read_axes(&decisions, watch_for, probe.is_some()) else {
            log::warn!(
                "spotter decisions response answered {:?}, which is not the asked set",
                decisions.answers.keys().collect::<Vec<_>>()
            );
            return Spotting::fail_closed(usage);
        };

        let (stance, deciding) = questions::combine(&axes, &self.thresholds, probe.is_some());
        Spotting {
            read: Read {
                stance,
                reason: questions::reason(stance, deciding, &axes),
            },
            origin: Origin::Model,
            usage,
            axes: Some(axes),
        }
    }
}

/// Pull the asked axes out of the response. `None` when an unconditional axis is
/// missing or came back as something other than a boolean answer.
fn read_axes(decisions: &crate::api::Decisions, watch_for: bool, probe: bool) -> Option<Axes> {
    Some(Axes {
        in_scope: decisions.probability(questions::IN_SCOPE)?,
        reversible: decisions.probability(questions::REVERSIBLE)?,
        sanctioned: decisions.probability(questions::SANCTIONED)?,
        path_outside: decisions.probability(questions::PATH_OUTSIDE)?,
        path_denied: decisions.probability(questions::PATH_DENIED)?,
        exposes_secrets: decisions.probability(questions::EXPOSES_SECRETS)?,
        // A conditional axis is `None` when it wasn't asked, and a hard failure
        // when it was asked and went unanswered — the fold reads "not asked" as
        // "nothing to weigh", which is wrong if the question was in fact posed.
        watch_for_hit: conditional(decisions, questions::WATCH_FOR_HIT, watch_for)?,
        prefix_safe: conditional(decisions, questions::PREFIX_SAFE, probe)?,
    })
}

/// `Ok(None)` for an axis that wasn't asked, `Ok(Some(p))` for one that was
/// answered, `None` for one that was asked and wasn't — expressed as nested
/// `Option` so `?` propagates the failure and keeps the "not asked" case.
fn conditional(decisions: &crate::api::Decisions, key: &str, asked: bool) -> Option<Option<f64>> {
    match asked {
        true => decisions.probability(key).map(Some),
        false => Some(None),
    }
}

/// Replay a recorded [`Axes`] through a band, without a request
/// (docs/permissions.md).
///
/// This is the whole point of folding in code rather than in the model: the
/// stance is a pure function of numbers the eval already wrote down, so moving a
/// threshold is re-scored against a whole corpus for free. One paid pass records
/// the vectors; every band question after it is arithmetic.
///
/// `probe` says whether a promotion probe rode the original call — the recorded
/// `prefix_safe` already answers that, but the fold needs to know whether the
/// question was asked at all.
pub fn replay(axes: &Axes, thresholds: &Thresholds, probe: bool) -> crate::log::Stance {
    questions::combine(axes, thresholds, probe).0
}

/// The exact state a request would carry for this call, pretty-printed — what the
/// decision model reads. The `efference dev spotter` tester prints it, the way it
/// prints the prose spotter's move block.
pub fn preview_state(
    role: &Config,
    call: &ToolCall,
    brief: Option<&ToolBrief<'_>>,
    probe: Option<&str>,
    cwd: Option<&std::path::Path>,
    scratch: Option<&std::path::Path>,
) -> String {
    let state = state::state(role, call, brief, &[], probe, cwd, scratch);
    serde_json::to_string_pretty(&state).unwrap_or_else(|_| state.to_string())
}

#[cfg(test)]
mod test;
