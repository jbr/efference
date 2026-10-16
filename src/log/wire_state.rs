//! A result's wire decision and the selection it can carry (docs/triage.md).

use super::*;

/// How a [`Payload::ToolResult`] should be projected. Flipping this (never
/// touching `raw`) is the one triage primitive — every projection experiment
/// is a change to how [`project`] reads these (docs/README.md, docs/log-and-projection.md).
///
/// Each variant is wired into [`project`] before it has a construction site, so a
/// new site lands as a behavior change rather than a type change that would move
/// the byte-stability pin. `Triaged` is built by the triage
/// (docs/triage.md); `Elided` by the interrupt (docs/control-channel.md).
///
/// [`project`]: crate::project::project
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WireState {
    /// Captured durably, but not yet accepted onto the main agent's wire.
    /// An appended result verdict resolves this state (docs/triage.md).
    Pending,
    /// Project the full `raw` content.
    Raw,
    /// Project the **verdict** — which lines survive (`keep`) plus an optional `note`
    /// — re-rendered from `raw` by [`project`] each turn, *not* the rendered bytes
    /// (docs/triage.md). The kept bytes are never stored twice, and the
    /// structured decision doubles as the record. `raw` stays in the log.
    ///
    /// [`project`]: crate::project::project
    Triaged(Triaged),
    /// Project nothing of substance; still recoverable from the log.
    Elided,
    /// A result an earlier harness withheld as a hard error because its triage had no
    /// verdict. **No longer written**: a triage without a verdict now holds its
    /// results until it has one (docs/triage.md). Kept so logs that carry it
    /// still load and project as they did — a hard error in place of the output
    /// (`is_error` forced true), `raw` still in the log.
    TriageFailed { why: String },
}

/// A triage verdict on one result (docs/triage.md): which lines survive,
/// the note read in their place, and triage's optional request for a better
/// `intent`. Carried by [`WireState::Triaged`]; the wire form is re-rendered from
/// `raw` plus this each turn, so a field added here is one setter and one render
/// clause, never a sweep.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Triaged {
    pub keep: Keep,
    pub note: Option<String>,
    /// Present only when the intent gave triage nothing to decide against — about
    /// the intent and never the task (docs/triage.md). Rendered after the
    /// note. Absent on logs written before the field existed, so it deserializes as
    /// `None` and serializes only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_feedback: Option<String>,
}

impl Triaged {
    /// A verdict carrying only its `keep`, with no note and no feedback.
    pub fn new(keep: Keep) -> Self {
        Self {
            keep,
            note: None,
            intent_feedback: None,
        }
    }

    /// Set the note. Builds; flipping a stored state is the projection's job.
    pub fn with_note(mut self, text: impl Into<String>) -> Self {
        self.note = Some(text.into());
        self
    }

    /// Set triage's `intent_feedback` (docs/triage.md).
    pub fn with_intent_feedback(mut self, text: impl Into<String>) -> Self {
        self.intent_feedback = Some(text.into());
        self
    }
}

impl From<Triaged> for WireState {
    fn from(verdict: Triaged) -> Self {
        Self::Triaged(verdict)
    }
}

/// What of a tool result survives on the canonical wire (docs/triage.md).
/// Ranges are 1-based inclusive line spans, interpreted against the first
/// `TRIAGE_LINE_CAP` lines — the set the triage showed the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keep {
    /// Keep every (shown) line.
    All,
    /// Keep nothing — only the `note` breadcrumb survives.
    None,
    /// Keep these line ranges (the model's `retain(lines=…)` choice).
    Ranges(Vec<(usize, usize)>),
}
