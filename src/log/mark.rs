//! Salience annotations: marks, who wrote them, and who they are for (docs/log-and-projection.md).

use super::*;

/// The content of a [`Payload::Mark`] — a breadcrumb left in the record. `note` is
/// the one-line decision / dead-end / relevance / consolidation-summary text.
/// Additive by design: a mark projects to nothing except through
/// its range variants, so growing this struct cannot perturb the
/// byte-stability pin (docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mark {
    pub kind: MarkKind,
    pub note: String,
    /// Who wrote `note` (docs/summary-head.md). Absent from a mark written before the
    /// field existed, which reads as [`Writer::Agent`].
    #[serde(default, skip_serializing_if = "Writer::is_agent")]
    pub writer: Writer,
    /// Which projection the mark applies to (docs/summary-head.md).
    #[serde(default, skip_serializing_if = "Audience::is_all")]
    pub audience: Audience,
}

impl Mark {
    /// A consolidation boundary written by the agent, for every projection: the
    /// `consolidate` call's mark (docs/consolidate.md).
    pub fn boundary(upto: Seq, note: impl Into<String>) -> Self {
        Mark {
            kind: MarkKind::Boundary { upto },
            note: note.into(),
            writer: Writer::default(),
            audience: Audience::default(),
        }
    }

    /// Replace only this inclusive working span (docs/consolidate.md).
    pub fn span(from: Seq, upto: Seq, note: impl Into<String>) -> Self {
        Self {
            kind: MarkKind::Span { from, upto },
            note: note.into(),
            writer: Writer::Agent,
            audience: Audience::All,
        }
    }

    pub fn written_by(mut self, writer: Writer) -> Self {
        self.writer = writer;
        self
    }

    pub fn for_audience(mut self, audience: Audience) -> Self {
        self.audience = audience;
        self
    }
}

/// Who wrote a [`Mark`]'s note (docs/summary-head.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Writer {
    /// The session's agent, in a `consolidate` call (docs/consolidate.md).
    #[default]
    Agent,
    /// A model that read the digest and wrote an account of it.
    Narrator(LoggedInference),
}

impl Writer {
    fn is_agent(&self) -> bool {
        matches!(self, Writer::Agent)
    }
}

/// Which projection a [`Mark`] applies to (docs/summary-head.md). A `Triage` mark folds
/// the span for a triage model on its own client; the main projection ignores it, so
/// the main model's wire bytes do not depend on it.
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

/// Head and working-span replacements interpreted by projection
/// (docs/consolidate.md, docs/summary-head.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MarkKind {
    Boundary {
        upto: Seq,
    },
    /// An inclusive replacement range; entries before `from` remain unchanged.
    Span {
        from: Seq,
        upto: Seq,
    },
}

impl MarkKind {
    pub fn range(&self) -> std::ops::RangeInclusive<Seq> {
        match *self {
            Self::Boundary { upto } => 0..=upto,
            Self::Span { from, upto } => from..=upto,
        }
    }
    pub fn label(&self) -> String {
        match *self {
            Self::Boundary { upto } => format!("consolidated through #{upto}"),
            Self::Span { from, upto } => format!("consolidated #{from}–#{upto}"),
        }
    }
}
