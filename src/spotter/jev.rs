//! The decision-model spotter (docs/permissions.md).
//!
//! Same rung, different machine. [`ForkSpot`](super::ForkSpot) asks a generative
//! model for a stance and a sentence; this asks a **decision model** (an LLM with
//! generation replaced by a classifier head) fixed questions and gets a
//! calibrated probability for each. It answers in tens of milliseconds for
//! a fraction of a cent, and it cannot be talked to: there is no system prompt to
//! shape and no prose to read, only questions and numbers.
//!
//! ```text
//!   cold request:      state (config + move, as JSON) ──▶ sketch, P(in_scope), …
//!   relation request:  the move + earlier approved calls ──▶ same | wider | different
//!                                      folded in code ──▶ Stance + reason
//! ```
//!
//! The consequences are the interesting part, and they are why this is a separate
//! implementation rather than a model swap:
//!
//! - **The judgement is decomposed.** No single question can be "what is your
//!   stance?", because the stance is a policy over several considerations. The
//!   cold axes and the operator's approved history are read separately and
//!   folded in code (`questions.rs`).
//! - **The reason is synthesized.** The model emits no prose, so the one line the
//!   acting session and the human gate read is written here from the level, what
//!   raised it, and the precedents found — what the model actually reported, not
//!   a sentence it wrote.
//! - **Nothing caches.** The route has no cache-control and the model's endpoint
//!   reports none, so every call pays full input. At this price that is not a
//!   cost question, but it does mean the persona/scope prefix economics that shape
//!   `ForkSpot` have no analogue here.
//!
//! Fail-closed holds for the cold request: any transport error or missing answer,
//! and the rung returns [`Origin::Fallback`] +
//! [`Stance::Gate`](crate::log::Stance::Gate). A failed relation request degrades
//! to no precedents instead, so it holds only what needed one.

use crate::api::Endpoint;
use crate::config::Config;
use crate::log::{Origin, RequestOrigin, ToolCall};
use crate::tools::ToolBrief;

use super::{Read, Spotter, Spotting};

mod ask;
mod questions;
mod relation;
mod state;

pub use questions::{Axes, Precedent, plain_reason};

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
    /// The judged session's project and working directories, which are not always
    /// this process's: the offline eval replays cases recorded in other checkouts
    /// (docs/permissions.md). `None` falls back to the process cwd as both.
    directories: Option<crate::policy::Directories>,
    /// Where this spotter's requests report their stages, under
    /// [`RequestOrigin::Spot`] (docs/log-and-projection.md). `None` records nothing.
    stages: Option<crate::api::StageSink>,
}

impl JevSpot {
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Self {
        JevSpot {
            endpoint,
            model: model.into(),
            scratch: None,
            directories: None,
            stages: None,
        }
    }

    pub fn with_scratch(mut self, scratch: Option<std::path::PathBuf>) -> Self {
        self.scratch = scratch;
        self
    }

    pub fn with_directories(mut self, directories: crate::policy::Directories) -> Self {
        self.directories = Some(directories);
        self
    }
}

impl Spotter for JevSpot {
    fn set_stage_sink(&mut self, sink: crate::api::StageSink) {
        self.stages = Some(sink);
    }

    async fn assess(
        &self,
        role: &Config,
        call: &ToolCall,
        brief: Option<&ToolBrief<'_>>,
        chain: &[String],
        probe: Option<&str>,
        precedents: &[ToolCall],
    ) -> Spotting {
        let dirs = self.directories.clone().or_else(super::process_directories);
        let state = state::state(
            role,
            call,
            brief,
            chain,
            probe,
            dirs.as_ref(),
            self.scratch.as_deref(),
        );
        let watch_for = role.watch_for.iter().any(|w| !w.trim().is_empty());
        let questions = ask::questions(watch_for, probe.is_some());

        // The relation request rides beside the cold one, only when the session
        // has consented calls of the move's family to compare against.
        let candidates = relation::candidates(call, precedents);
        let relation_state = relation::state(
            call,
            dirs.as_ref()
                .map(crate::policy::Directories::judged_working)
                .as_deref(),
        );
        let relation_questions = relation::questions(&candidates);
        let cold = self.endpoint.decide_traced(
            self.stages.as_ref(),
            RequestOrigin::Spot,
            &self.model,
            &state,
            &questions,
        );
        let relation = async {
            match candidates.is_empty() {
                true => None,
                false => Some(
                    self.endpoint
                        .decide_traced(
                            self.stages.as_ref(),
                            RequestOrigin::Spot,
                            &self.model,
                            &relation_state,
                            &relation_questions,
                        )
                        .await,
                ),
            }
        };
        let (cold, relation) = futures_lite::future::zip(cold, relation).await;

        let decisions = match cold {
            Ok(decisions) => decisions,
            Err(error) => {
                log::warn!("spotter decisions request failed: {error}");
                return Spotting::fail_closed(None);
            }
        };
        let mut usage = decisions.request_usage();

        // A relation failure means no precedents, not no verdict: the move holds
        // or clears on its level alone.
        let (reading, compared) = match relation {
            None => (Some(questions::Reading::default()), Vec::new()),
            Some(Ok(answered)) => {
                add_usage(&mut usage, &answered.request_usage());
                (
                    Some(relation::read(&answered, candidates.len())),
                    relation::compared(&answered, &candidates),
                )
            }
            Some(Err(error)) => {
                log::warn!("spotter relation request failed: {error}");
                (None, Vec::new())
            }
        };
        let usage = Some(usage);

        // Every unconditional axis must have come back. A partial answer set is
        // not a partial verdict — the fold reads all of them — so it fails closed
        // exactly as an unparseable `spot` call does.
        let Some(mut axes) = read_axes(&decisions, watch_for, probe.is_some()) else {
            log::warn!(
                "spotter decisions response answered {:?}, which is not the asked set",
                decisions.answers.keys().collect::<Vec<_>>()
            );
            return Spotting::fail_closed(usage);
        };

        let decision = questions::decide(&axes, reading, probe.is_some());
        axes.precedent = reading.map(|r| Precedent {
            candidates: r.candidates,
            same: r.same,
            wider: r.wider,
            required: decision.required,
            compared,
        });
        Spotting {
            read: Read {
                stance: decision.stance,
                reason: decision.reason,
            },
            origin: Origin::Model,
            usage,
            axes: Some(axes),
        }
    }
}

/// Fold the relation request's billing into the cold request's, so the spot is
/// one entry in the ledger with the whole cost. The id and model stay the cold
/// request's.
fn add_usage(total: &mut crate::api::RequestUsage, extra: &crate::api::RequestUsage) {
    total.usage.input_tokens += extra.usage.input_tokens;
    total.usage.output_tokens += extra.usage.output_tokens;
}

/// Pull the asked axes out of the response. `None` when an unconditional axis is
/// missing or came back as something other than a boolean answer.
fn read_axes(decisions: &crate::api::Decisions, watch_for: bool, probe: bool) -> Option<Axes> {
    Some(Axes {
        in_scope: decisions.probability(questions::IN_SCOPE)?,
        path_outside: decisions.probability(questions::PATH_OUTSIDE)?,
        path_denied: decisions.probability(questions::PATH_DENIED)?,
        exposes_secrets: decisions.probability(questions::EXPOSES_SECRETS)?,
        // A conditional axis is `None` when it wasn't asked, and a hard failure
        // when it was asked and went unanswered — the fold reads "not asked" as
        // "nothing to weigh", which is wrong if the question was in fact posed.
        watch_for_hit: if watch_for {
            Some(decisions.probability(questions::WATCH_FOR_HIT)?)
        } else {
            None
        },
        prefix_safe: if probe {
            Some(decisions.probability(questions::PREFIX_SAFE)?)
        } else {
            None
        },
        sketch: Some(sketch(decisions)?),
        precedent: None,
    })
}

/// The sketch distribution as five probabilities, level 0 first. `None` when the
/// answer is missing a level.
fn sketch(decisions: &crate::api::Decisions) -> Option<Vec<f64>> {
    let answered = decisions.probabilities(questions::SKETCH)?;
    (0..questions::LEVELS)
        .map(|level| answered.get(&level.to_string()).copied())
        .collect()
}

/// The exact state a request would carry for this call, pretty-printed — what the
/// decision model reads. The `efference dev spotter` tester prints it, the way it
/// prints the prose spotter's move block.
pub fn preview_state(
    role: &Config,
    call: &ToolCall,
    brief: Option<&ToolBrief<'_>>,
    probe: Option<&str>,
    dirs: Option<&crate::policy::Directories>,
    scratch: Option<&std::path::Path>,
) -> String {
    let state = state::state(role, call, brief, &[], probe, dirs, scratch);
    serde_json::to_string_pretty(&state).unwrap_or_else(|_| state.to_string())
}

#[cfg(test)]
mod test;
