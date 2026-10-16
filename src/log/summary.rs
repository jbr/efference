//! Summaries: the text that replaces a region of the transcript, who made it, and who
//! it is for (docs/summary.md).

use super::*;

/// The content of a [`Payload::Summary`]: `text` replaces the inclusive range
/// `from..=upto` of the transcript for the readers its `audience` names. A head
/// summary starts at `from == 0`; a working-span summary starts later. The entry
/// itself projects to nothing; the text appears where the range ends
/// (docs/summary.md, docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub from: Seq,
    pub upto: Seq,
    pub text: String,
    /// Who made `text` (docs/summary.md). Absent from a summary the agent made.
    #[serde(default, skip_serializing_if = "MadeBy::is_consolidation")]
    pub made_by: MadeBy,
    /// Which projection the summary applies to (docs/summary.md).
    #[serde(default, skip_serializing_if = "Audience::is_all")]
    pub audience: Audience,
}

impl Summary {
    /// The agent's summary of the working span `from..=upto`, for every projection:
    /// the `consolidate` call's entry (docs/consolidate.md).
    pub fn consolidation(from: Seq, upto: Seq, text: impl Into<String>) -> Self {
        Summary {
            from,
            upto,
            text: text.into(),
            made_by: MadeBy::Consolidation,
            audience: Audience::All,
        }
    }

    /// The harness's summary of everything through `upto`, written from the digest by
    /// `model` (docs/summary.md).
    pub fn compaction(upto: Seq, text: impl Into<String>, model: LoggedInference) -> Self {
        Summary {
            from: 0,
            upto,
            text: text.into(),
            made_by: MadeBy::Compaction(model),
            audience: Audience::All,
        }
    }

    pub fn for_audience(mut self, audience: Audience) -> Self {
        self.audience = audience;
        self
    }

    /// The inclusive range of entries the summary replaces.
    pub fn range(&self) -> std::ops::RangeInclusive<Seq> {
        self.from..=self.upto
    }

    /// The one-line description clients show for the entry.
    pub fn label(&self) -> String {
        let verb = match self.made_by {
            MadeBy::Consolidation => "consolidated",
            MadeBy::Compaction(_) => "compacted",
        };
        if self.from == 0 {
            format!("{verb} through #{}", self.upto)
        } else {
            format!("{verb} #{}–#{}", self.from, self.upto)
        }
    }
}

/// Who made a [`Summary`]'s text (docs/summary.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MadeBy {
    /// The session's agent, in a `consolidate` call, with the region in view
    /// (docs/consolidate.md).
    #[default]
    Consolidation,
    /// The harness, through a model that read the digest (docs/summary.md).
    Compaction(LoggedInference),
}

impl MadeBy {
    fn is_consolidation(&self) -> bool {
        matches!(self, MadeBy::Consolidation)
    }
}

/// Which projection a [`Summary`] applies to (docs/summary.md). A `Triage` summary
/// replaces the range for a triage model on its own client; the main projection
/// ignores it, so the main model's wire bytes do not depend on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    #[default]
    All,
    Triage,
}

impl Audience {
    fn is_all(&self) -> bool {
        matches!(self, Audience::All)
    }
}
