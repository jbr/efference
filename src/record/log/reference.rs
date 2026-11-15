//! One chip a submitted turn carries — the shared reference contract
//! (docs/log-and-projection.md, docs/control-api.md): what a definition is, the
//! targets it can point at, and the two shapes a target takes. A stored
//! [`ReferenceTarget`] is what the submission gate writes after validating a
//! client's turn; a [`SubmittedTarget`] is what a client sends: it names an
//! upload or a session for the server to resolve, and supplies the absolute
//! path it means for a filesystem target, which the gate currently refuses.

use serde::{Deserialize, Serialize};

use super::attachment::{self, Arrival};

/// Requested access on a filesystem reference (docs/control-api.md): the
/// default is read with no write, write implies read, and an explicit
/// `read: false` without write is refused when the access is normalized — a
/// reference the submitter cannot read is not a reference. The booleans record
/// what the submission asked for; `write: false` means "no write requested",
/// never "deny write".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ReferenceAccess {
    pub read: bool,
    pub write: bool,
}

impl Default for ReferenceAccess {
    /// Read, no write.
    fn default() -> Self {
        Self {
            read: true,
            write: false,
        }
    }
}

impl ReferenceAccess {
    /// The access the gate stores: `None` when the submitter asked for neither
    /// read nor write, which it refuses; write alone normalizes to read+write.
    pub fn normalized(self) -> Option<Self> {
        let read = self.read || self.write;
        read.then_some(Self {
            read,
            write: self.write,
        })
    }

    /// The request as the rendered document states it: historical metadata
    /// about what the submission asked for, never a claim of current policy.
    fn requested(self) -> &'static str {
        match (self.read, self.write) {
            // Write alone normalizes to read+write; neither is the refusal.
            (true, true) | (false, true) => "read+write",
            (true, false) => "read",
            (false, false) => "none",
        }
    }
}

/// What a filesystem reference points at, as the server resolved it at
/// submission — never re-resolved against a changed working directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReferenceTargetKind {
    File,
    Directory,
}

/// Where a stored reference points. A client's submission names an upload or
/// a session; the server resolves the first against its store and validates
/// the second against the host registry. A filesystem target carries what the
/// submitter asserted — the text typed, the absolute path, the kind and the
/// requested access (docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ReferenceTarget {
    /// A filesystem target. `original` is what the submitter typed; `path` is
    /// the absolute path the client supplied, checked for form at submission
    /// and never re-resolved; `access` records what the submission asked for.
    /// What the reference authorizes is decided when the turn is admitted
    /// (docs/permissions.md).
    Filesystem {
        original: String,
        path: String,
        target: ReferenceTargetKind,
        access: ReferenceAccess,
    },
    /// A stored upload (docs/inbox.md), carried as the server resolved it.
    Attachment(Arrival),
    /// An exact session identity on this host, validated at submission against
    /// the host's live registry and stored independently of the label.
    Agent { session: String },
}

/// One chip definition: what the submitter labeled, and where it points.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ReferenceDefinition {
    /// The submitter's display label. Presentation only — never authorization,
    /// never identity.
    pub label: String,
    pub target: ReferenceTarget,
}

impl ReferenceDefinition {
    /// An attachment reference for a resolved arrival: the store's name is the
    /// label the legacy body shape implies.
    pub fn attachment(arrival: Arrival) -> Self {
        Self {
            label: arrival.name.clone(),
            target: ReferenceTarget::Attachment(arrival),
        }
    }

    /// An agent reference the host registry has already validated.
    pub fn agent(label: impl Into<String>, session: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            target: ReferenceTarget::Agent {
                session: session.into(),
            },
        }
    }

    /// A filesystem reference the submission gate has validated: `path` absolute and
    /// lexically normalized, the target present and of the claimed `kind`, and `access`
    /// one the policy can act on. What it authorizes is decided at admission
    /// (docs/permissions.md), not here.
    pub fn filesystem(
        label: impl Into<String>,
        original: impl Into<String>,
        path: impl Into<String>,
        kind: ReferenceTargetKind,
        access: ReferenceAccess,
    ) -> Self {
        Self {
            label: label.into(),
            target: ReferenceTarget::Filesystem {
                original: original.into(),
                path: path.into(),
                target: kind,
                access,
            },
        }
    }

    /// The definition's line in a rendered document's `references:` block
    /// (docs/log-and-projection.md): readable metadata about what the submission
    /// referenced. User-message data, so it carries no harness marker.
    pub fn describe_line(&self) -> String {
        match &self.target {
            ReferenceTarget::Filesystem {
                original,
                path,
                target,
                access,
            } => {
                let kind = match target {
                    ReferenceTargetKind::File => "file",
                    ReferenceTargetKind::Directory => "directory",
                };
                format!(
                    "{} — {kind} {original} → {path} (requested: {})",
                    self.label,
                    access.requested()
                )
            }
            // The description leads with the store's name — the attachment's own
            // metadata, verbatim (docs/inbox.md).
            ReferenceTarget::Attachment(arrival) => attachment::describe(arrival),
            ReferenceTarget::Agent { session } => format!("{} — @{session}", self.label),
        }
    }

    /// The readable flat spelling an occurrence stands for in a draft or a
    /// status view: what re-typing the chip as text grants nothing by
    /// (docs/control-api.md). An attachment spells nothing; a caller that
    /// shows this to a person shows the upload's notice or name too.
    pub fn readable(&self) -> String {
        match &self.target {
            ReferenceTarget::Filesystem { original, .. } => original.clone(),
            ReferenceTarget::Agent { session } => format!("@{session}"),
            ReferenceTarget::Attachment(_) => String::new(),
        }
    }

    /// The definition as a client would re-submit it. An attachment keeps only
    /// its store name — the one fact a client may assert about it, which the
    /// server resolves against its store. A filesystem target keeps the text
    /// typed, the absolute path, the kind and the requested access the client
    /// supplied: the gate validates those and never re-derives the path from
    /// `original` against a working directory that has since changed
    /// (docs/control-api.md). Agent targets keep the session id.
    pub fn submitted(&self) -> SubmittedReference {
        SubmittedReference {
            label: self.label.clone(),
            target: match &self.target {
                ReferenceTarget::Filesystem {
                    original,
                    path,
                    target,
                    access,
                } => SubmittedTarget::Filesystem {
                    original: original.clone(),
                    path: path.clone(),
                    target: *target,
                    access: *access,
                },
                ReferenceTarget::Attachment(arrival) => SubmittedTarget::Attachment {
                    name: arrival.name.clone(),
                },
                ReferenceTarget::Agent { session } => SubmittedTarget::Agent {
                    session: session.clone(),
                },
            },
        }
    }
}

/// A reference definition as a client submits it: the label it shows, and a
/// target that names things without supplying resolved facts. The submission
/// gate turns each one into a stored [`ReferenceDefinition`] or refuses the
/// turn (docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SubmittedReference {
    pub label: String,
    pub target: SubmittedTarget,
}

/// What a client points a reference at. An attachment names a store upload,
/// which the server resolves against that store, so a client-supplied arrival
/// is not trusted. A filesystem target carries the text the submitter typed,
/// the absolute path it means, and the requested access; the gate validates
/// its form and stores it, and the reference grants at admission
/// (docs/permissions.md). An agent target names a session the host registry
/// must know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SubmittedTarget {
    Filesystem {
        original: String,
        /// The absolute path the submitter means, supplied by the client. The
        /// gate validates it and never re-derives it from `original` against a
        /// working directory that has since changed, so a recalled target
        /// keeps its identity (docs/control-api.md).
        path: String,
        target: ReferenceTargetKind,
        #[serde(default)]
        access: ReferenceAccess,
    },
    Attachment {
        name: String,
    },
    Agent {
        session: String,
    },
}

#[cfg(test)]
mod test;
