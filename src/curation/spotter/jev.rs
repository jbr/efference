//! The decision-model spotter (docs/permissions.md).
//!
//! Same rung, different machine. [`ForkSpot`](super::ForkSpot) asks a generative
//! model for a stance and a sentence; this asks a **decision model** (an LLM with
//! generation replaced by a classifier head) fixed questions and gets a
//! calibrated probability for each. It cannot be talked to: there is no system
//! prompt to shape and no prose to read, only questions and numbers.
//!
//! ```text
//!   cold request:      state (config + move, as JSON) ──▶ sketch, P(in_scope), …
//!   relation request:  the move + earlier approved calls ──▶ same | wider | different
//!                                      folded in code ──▶ Stance + reason
//! ```
//!
//! The consequences are why this is a separate implementation rather than a model swap:
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
//!   reports none, so every call pays full input, and the persona/scope prefix
//!   economics that shape `ForkSpot` have no analogue here.
//!
//! Fail-closed holds for the cold request: any transport error or missing answer,
//! and the rung returns [`Origin::Fallback`] +
//! [`Stance::Gate`](crate::record::log::Stance::Gate). A failed relation request degrades
//! to no precedents instead, so it holds only what needed one.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::Config;
use crate::inference::api::{Endpoint, Question};
use crate::record::log::{Axes, Origin, Precedent, RequestOrigin, ToolCall};
use crate::tools::ToolBrief;

use super::{Circumstances, Read, Spotter, Spotting};

mod ask;
mod questions;
mod relation;
mod state;

pub use questions::plain_reason;

/// The decision-model spotter. Holds the [`Endpoint`] rather than a
/// [`Client`](crate::inference::api::Client): the decisions route is one JSON POST with no
/// system prompt, no tools, and no stream, so none of the client's machinery
/// applies (`src/inference/api/decisions.rs`).
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
    directories: Option<crate::config::policy::Directories>,
    /// Where this spotter's requests report their stages, under
    /// [`RequestOrigin::Spot`] (docs/log-and-projection.md). `None` records nothing.
    stages: Option<crate::inference::api::StageSink>,
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

    pub fn with_directories(mut self, directories: crate::config::policy::Directories) -> Self {
        self.directories = Some(directories);
        self
    }

    /// The bodies [`Spotter::assess`] would send for this call — the cold request, then
    /// the relation request when one is sent — built without sending them: what
    /// `efference dev spotter-request --json` prints.
    pub fn request_bodies(
        &self,
        role: &Config,
        call: &ToolCall,
        circumstances: &Circumstances<'_>,
    ) -> Result<Vec<String>, crate::inference::api::Error> {
        let dirs = self.directories.clone().or_else(super::process_directories);
        let asked = requests(
            role,
            call,
            circumstances,
            dirs.as_ref(),
            self.scratch.as_deref(),
        );
        let mut bodies =
            vec![
                self.endpoint
                    .decisions_body(&self.model, &asked.state, &asked.questions)?,
            ];
        if let Some(relation) = &asked.relation {
            bodies.push(self.endpoint.decisions_body(
                &self.model,
                &relation.state,
                &relation.questions,
            )?);
        }
        Ok(bodies)
    }
}

impl Spotter for JevSpot {
    fn set_stage_sink(&mut self, sink: crate::inference::api::StageSink) {
        self.stages = Some(sink);
    }

    async fn assess(
        &self,
        role: &Config,
        call: &ToolCall,
        circumstances: &Circumstances<'_>,
    ) -> Spotting {
        let probe = circumstances.probe;
        let dirs = self.directories.clone().or_else(super::process_directories);
        let Requests {
            state,
            questions,
            relation,
            candidates,
        } = requests(
            role,
            call,
            circumstances,
            dirs.as_ref(),
            self.scratch.as_deref(),
        );
        let spotter_notes = role.spotter_notes.iter().any(|w| !w.trim().is_empty());
        let cold = self.endpoint.decide_traced(
            self.stages.as_ref(),
            RequestOrigin::Spot,
            &self.model,
            &state,
            &questions,
        );
        let relation = async {
            match &relation {
                None => None,
                Some(relation) => Some(
                    self.endpoint
                        .decide_traced(
                            self.stages.as_ref(),
                            RequestOrigin::Spot,
                            &self.model,
                            &relation.state,
                            &relation.questions,
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

        // Every unconditional axis must have come back. A partial answer set is
        // not a partial verdict — the fold reads all of them — so it fails closed
        // exactly as an unparseable `spot` call does.
        let Some(mut axes) = read_axes(&decisions, spotter_notes, probe.is_some()) else {
            log::warn!(
                "spotter decisions response answered {:?}, which is not the asked set",
                decisions.answers.keys().collect::<Vec<_>>()
            );
            return Spotting::fail_closed(Some(usage));
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
            usage: Some(usage),
            axes: Some(axes),
        }
    }
}

/// What one assessment asks the decision model: the cold request, and the relation
/// request when the session has consented calls of the move's family to compare it
/// against.
pub struct Requests<'a> {
    /// The cold request's state: the config and the move, as JSON.
    pub state: Value,
    /// The cold request's questions, by key.
    pub questions: BTreeMap<String, Question>,
    /// The relation request; `None` when no consented call of the move's family exists.
    pub relation: Option<Relation>,
    /// The consented calls the relation request compares the move against, oldest
    /// first.
    candidates: Vec<&'a ToolCall>,
}

/// The relation request's state and questions.
pub struct Relation {
    pub state: Value,
    pub questions: BTreeMap<String, Question>,
}

/// The requests [`JevSpot::assess`] sends for this call, from the judged session's
/// directories and scratch dir. `efference dev spotter-request` prints them.
pub fn requests<'a>(
    role: &Config,
    call: &ToolCall,
    circumstances: &Circumstances<'a>,
    dirs: Option<&crate::config::policy::Directories>,
    scratch: Option<&std::path::Path>,
) -> Requests<'a> {
    let &Circumstances {
        ref brief,
        chain,
        probe,
        precedents,
        // The prose rung's alone: no question here reads them.
        supervision: _,
        utterances: _,
    } = circumstances;
    let state = state::state(role, call, brief.as_ref(), chain, probe, dirs, scratch);
    let spotter_notes = role.spotter_notes.iter().any(|w| !w.trim().is_empty());
    let questions = ask::questions(spotter_notes, probe.is_some());
    // The relation request rides beside the cold one, only when the session has
    // consented calls of the move's family to compare against.
    let candidates = relation::candidates(call, precedents);
    let relation = (!candidates.is_empty()).then(|| Relation {
        state: relation::state(
            call,
            dirs.map(crate::config::policy::Directories::judged_working)
                .as_deref(),
        ),
        questions: relation::questions(&candidates),
    });
    Requests {
        state,
        questions,
        relation,
        candidates,
    }
}

/// The consented calls in `precedents` the relation request would compare `call`
/// against: the most recent of the move's family, oldest first. Passing only these as
/// the precedents asks the same requests, which is what lets the offline eval keep a
/// case's history small.
pub fn compared_precedents(call: &ToolCall, precedents: &[ToolCall]) -> Vec<ToolCall> {
    relation::candidates(call, precedents)
        .into_iter()
        .cloned()
        .collect()
}

/// Fold the relation request's billing into the cold request's, so the spot is
/// one entry in the ledger with the whole cost. The id and model stay the cold
/// request's.
fn add_usage(
    total: &mut crate::inference::api::RequestUsage,
    extra: &crate::inference::api::RequestUsage,
) {
    total.usage.input_tokens += extra.usage.input_tokens;
    total.usage.output_tokens += extra.usage.output_tokens;
}

/// Pull the asked axes out of the response. `None` when an unconditional axis is
/// missing or came back as something other than a boolean answer.
fn read_axes(
    decisions: &crate::inference::api::Decisions,
    spotter_notes: bool,
    probe: bool,
) -> Option<Axes> {
    Some(Axes {
        in_scope: decisions.probability(questions::IN_SCOPE)?,
        path_outside: decisions.probability(questions::PATH_OUTSIDE)?,
        path_denied: decisions.probability(questions::PATH_DENIED)?,
        exposes_secrets: decisions.probability(questions::EXPOSES_SECRETS)?,
        // A conditional axis is `None` when it wasn't asked, and a hard failure
        // when it was asked and went unanswered — the fold reads "not asked" as
        // "nothing to weigh", which is wrong if the question was in fact posed.
        note_warns: if spotter_notes {
            Some(decisions.probability(questions::NOTE_WARNS)?)
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
fn sketch(decisions: &crate::inference::api::Decisions) -> Option<Vec<f64>> {
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
    dirs: Option<&crate::config::policy::Directories>,
    scratch: Option<&std::path::Path>,
) -> String {
    let state = state::state(role, call, brief, &[], probe, dirs, scratch);
    serde_json::to_string_pretty(&state).unwrap_or_else(|_| state.to_string())
}

#[cfg(test)]
mod test;
