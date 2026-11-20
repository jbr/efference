//! The decision-model spotter (docs/permissions.md).
//!
//! Same rung, different machine. [`ForkSpot`](super::ForkSpot) asks a generative
//! model for a stance and a sentence; this asks a **decision model** (an LLM with
//! generation replaced by a classifier head) fixed questions and gets a
//! calibrated probability for each. It cannot be talked to: there is no system
//! prompt to shape and no prose to read, only questions and numbers.
//!
//! ```text
//!   positive request:  state ──▶ P(yes) per battery question, as written (+ prefix_safe)
//!   negative request:  state ──▶ P(yes) per battery question, negated
//!                      averaged, folded in code ──▶ Stance + reason
//! ```
//!
//! The consequences are why this is a separate implementation rather than a model swap:
//!
//! - **The judgement is decomposed.** No single question can be "what is your
//!   stance?", because the stance is a policy over several considerations. A battery
//!   of reasons to clear and hazards is read, each question in two wordings, and
//!   folded in code (`questions.rs`).
//! - **The reason is synthesized.** The model emits no prose, so the one line the
//!   acting session and the human gate read is written here from what decided the
//!   fold — what the model actually reported, not a sentence it wrote.
//! - **Nothing caches.** The route has no cache-control and the model's endpoint
//!   reports none, so every call pays full input, and the persona/scope prefix
//!   economics that shape `ForkSpot` have no analogue here.
//!
//! Fail-closed holds for both requests: any transport error or missing answer, and
//! the rung returns [`Origin::Fallback`] +
//! [`Stance::Gate`](crate::record::log::Stance::Gate).

use std::collections::BTreeMap;

use crate::config::Config;
use crate::inference::api::{Decisions, Endpoint, Question};
use crate::record::log::{Axes, Origin, QuestionReading, RequestOrigin, ToolCall};

use super::{Circumstances, Read, Spotter, Spotting};

mod ask;
mod questions;
mod state;

pub use questions::plain_reason;
pub use state::{State, conversation};

use ask::Polarity;

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

    /// The bodies [`Spotter::assess`] would send for this call — the positive request,
    /// then the negative one — built without sending them: what `efference dev
    /// spotter-request --json` prints.
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
        [&asked.positive, &asked.negative]
            .into_iter()
            .map(|questions| {
                self.endpoint
                    .decisions_body(&self.model, &asked.state, questions)
            })
            .collect()
    }

    /// Ask `questions` about `state`, reporting the request's stages.
    async fn ask(
        &self,
        state: &State,
        questions: &BTreeMap<String, Question>,
    ) -> Result<Decisions, crate::inference::api::Error> {
        self.endpoint
            .decide_traced(
                self.stages.as_ref(),
                RequestOrigin::Spot,
                &self.model,
                state,
                questions,
            )
            .await
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
        let probe = circumstances.probe.is_some();
        let dirs = self.directories.clone().or_else(super::process_directories);
        let Requests {
            state,
            positive,
            negative,
            outside_grants,
        } = requests(
            role,
            call,
            circumstances,
            dirs.as_ref(),
            self.scratch.as_deref(),
        );
        let (positive, negative) =
            futures_lite::future::zip(self.ask(&state, &positive), self.ask(&state, &negative))
                .await;

        // The spot is one ledger entry with both requests' cost; the id and model are
        // the positive request's.
        let mut usage = None;
        for answered in [&positive, &negative].into_iter().flatten() {
            let request = answered.request_usage();
            match &mut usage {
                None => usage = Some(request),
                Some(sum) => sum.usage.add(&request.usage),
            }
        }
        let (positive, negative) = match (positive, negative) {
            (Ok(positive), Ok(negative)) => (positive, negative),
            (Err(error), _) | (_, Err(error)) => {
                log::warn!("spotter decisions request failed: {error}");
                return Spotting::fail_closed(usage);
            }
        };

        // Every asked question must have come back in both wordings. A partial answer
        // set is not a partial verdict — the fold reads all of them — so it fails
        // closed exactly as an unparseable `spot` call does.
        let spotter_notes = has_spotter_notes(role);
        let Some(axes) = read_axes(&positive, &negative, spotter_notes, probe) else {
            log::warn!(
                "spotter decisions responses answered {:?} and {:?}, which is not the asked set",
                positive.answers.keys().collect::<Vec<_>>(),
                negative.answers.keys().collect::<Vec<_>>(),
            );
            return Spotting::fail_closed(usage);
        };
        let axes = axes.with_outside_grants(outside_grants);

        let decision = questions::decide(&axes, probe);
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

/// What one assessment asks the decision model: one state, asked the battery in each
/// wording, and the harness's own fact the fold reads beside the answers.
pub struct Requests {
    /// The state both requests carry.
    pub state: State,
    /// The positive request's questions, by key: the battery as written, and the
    /// promotion question when a probe is present.
    pub positive: BTreeMap<String, Question>,
    /// The negative request's questions, by key: the battery negated.
    pub negative: BTreeMap<String, Question>,
    /// Whether the move's resolved paths place it outside the grants.
    pub outside_grants: bool,
}

/// The requests [`JevSpot::assess`] sends for this call, from the judged session's
/// directories and scratch dir. `efference dev spotter-request` prints them.
pub fn requests(
    role: &Config,
    call: &ToolCall,
    circumstances: &Circumstances<'_>,
    dirs: Option<&crate::config::policy::Directories>,
    scratch: Option<&std::path::Path>,
) -> Requests {
    let spotter_notes = has_spotter_notes(role);
    let probe = circumstances.probe.is_some();
    Requests {
        state: state::state(role, call, circumstances, dirs, scratch),
        positive: ask::questions(Polarity::Positive, spotter_notes, probe),
        negative: ask::questions(Polarity::Negative, spotter_notes, probe),
        outside_grants: state::outside_grants(role, call, dirs, scratch),
    }
}

/// Whether the config has a spotter note, which is when the two note questions are
/// asked.
fn has_spotter_notes(role: &Config) -> bool {
    role.spotter_notes.iter().any(|w| !w.trim().is_empty())
}

/// Pull the asked answers out of the two responses. `None` when an asked question is
/// missing from either, or came back as something other than a boolean answer. A
/// conditional question that was not asked has no entry; one that was asked and went
/// unanswered is a failure, since the fold would read its absence as "nothing to weigh".
fn read_axes(
    positive: &Decisions,
    negative: &Decisions,
    spotter_notes: bool,
    probe: bool,
) -> Option<Axes> {
    let readings = ask::asked(spotter_notes)
        .map(|question| {
            let reading = QuestionReading::new(
                positive.probability(question.key)?,
                negative.probability(question.key)?,
            );
            Some((question.key.to_string(), reading))
        })
        .collect::<Option<Vec<_>>>()?;
    let prefix_safe = match probe {
        true => Some(positive.probability(questions::PREFIX_SAFE)?),
        false => None,
    };
    Some(Axes::new(readings).with_prefix_safe(prefix_safe))
}

/// The exact state a request would carry for this call, pretty-printed — what the
/// decision model reads. The `efference dev spotter` tester prints it, the way it
/// prints the prose spotter's move block.
pub fn preview_state(
    role: &Config,
    call: &ToolCall,
    circumstances: &Circumstances<'_>,
    dirs: Option<&crate::config::policy::Directories>,
    scratch: Option<&std::path::Path>,
) -> String {
    let state = state::state(role, call, circumstances, dirs, scratch);
    serde_json::to_string_pretty(&state).unwrap_or_else(|e| format!("unserializable state: {e}"))
}

#[cfg(test)]
mod test;
