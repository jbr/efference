//! Admission-time grants for a submitted turn's filesystem references
//! (docs/permissions.md, docs/control-api.md).
//!
//! A reference grants nothing when it is typed, queued, withdrawn, or merely accepted by
//! `POST /v1/turns`. The grant is created when the turn it arrived with is **admitted** to the
//! log — the moment the next request will carry it — and not before, so a withdrawn turn leaves
//! no permission behind. An ask reply carrying references is the same case: its answer lands as
//! a tool result, so the document is read from there too.
//!
//! The whole transaction runs under the session's policy section
//! ([`Session::policy_section`]), so its grants reach the log and the effective policy in one
//! order, and no other committed edit can interleave with them.

use std::collections::BTreeSet;
use std::path::Path;

use super::watch::{raise_pass, refresh_locked};
use super::*;
use crate::config::policy::{filesystem_grants, has_filesystem_references};
use crate::record::log::{ConfigChange, ConfigEdit, ContentBlock, ReferenceTarget, UserContent};

/// The turn's structured document, when it carries a filesystem reference — the admission
/// transaction's trigger. `None` for every other payload, so a plain turn takes exactly the
/// append it always did: no policy lock, no config file, no mask probe.
pub(super) fn filesystem_document(payload: &Payload) -> Option<UserContent> {
    let document = match payload {
        Payload::User(blocks) => blocks.iter().find_map(|block| match block {
            ContentBlock::Document(content) => Some(content),
            _ => None,
        }),
        Payload::ToolResult { raw, .. } => raw.document.as_deref(),
        _ => None,
    }?;
    has_filesystem_references(document).then(|| document.clone())
}

/// The references' original spellings, for a notice that names what did not happen.
fn named(originals: &[String]) -> String {
    originals
        .iter()
        .map(|original| format!("`{original}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Session {
    /// Admit `payload` as a user turn, adding the path grants its document's filesystem
    /// references ask for (docs/permissions.md, docs/control-api.md).
    ///
    /// All of it is under the policy section, in this order:
    ///
    /// 1. Refresh and adopt the config file, and reconcile what it holds against the policy in
    ///    force — the committed edits fs-watch would append — so the grant is computed against
    ///    the current perimeter and cannot narrow a grant an operator just wrote.
    /// 2. Compute the additive grant union ([`filesystem_grants`]).
    /// 3. Mask-probe each computed grant — both its key and the absolute target the document
    ///    stores, which can differ. Either one the `[env]` masker would rewrite is refused
    ///    rather than written as a placeholder, before anything is touched.
    /// 4. Append the message. The turn is durable before any grant exists, so no grant can ever
    ///    authorize a turn whose append failed.
    /// 5. Replace the config file once with the whole batch.
    /// 6. Append and fold one committed edit per grant, on the message's turn.
    ///
    /// A failure after the message is admitted has two shapes, and one notice names which one
    /// happened. A **config-file write** that fails adds nothing: the file keeps its prior bytes
    /// and its prior held document, no grant is appended, and the message stands under the
    /// permissions the session already had. An **append of the grant edits** that fails partway
    /// through leaves the file holding every grant while the log and the effective policy hold
    /// the ones before the failure: those are in force, with their own notices, and the rest take
    /// effect when a later refresh diffs the file against the policy in force and appends the
    /// missing edits. Nothing is rolled back, nothing retries, and there is no journal or repair
    /// pass beyond that diff; the file-behind-the-log window is likewise left for the next
    /// refresh to close.
    pub(super) fn admit(
        &self,
        turn: Turn,
        payload: Payload,
        submission_id: Option<String>,
        source: Option<String>,
        document: &UserContent,
    ) -> io::Result<Seq> {
        let mut refused: Vec<String> = Vec::new();
        let mut grant_notices: Vec<(String, String)> = Vec::new();
        // Grants the file holds whose log entry failed: (how many were recorded, how many there
        // were, the error). Distinct from `refused`, which is grants no one holds.
        let mut partial: Option<(usize, usize, String)> = None;
        let (seq, pass) = {
            let _section = self.policy_section();

            // 1. The file first: adopt what an operator wrote and reconcile what the policy in
            //    force has not seen yet.
            let pass = refresh_locked(self);

            // 2. What the references ask for, additively. An invalid file that could not be
            //    restored is the one state the batch cannot be written to, so the message is
            //    admitted without grants.
            let dirs = self.directories();
            let role = self.effective_config();
            let (grants, unusable) = if pass.grants_usable() {
                filesystem_grants(document, &role, &dirs)
            } else {
                (Vec::new(), Vec::new())
            };
            for unusable in unusable {
                refused.push(format!("`{}` — {}", unusable.original, unusable.reason));
            }
            if !pass.grants_usable() {
                refused.push("the config file is invalid and could not be restored".to_string());
            }

            // 3. The mask probe: what the log would store is what the file gets. Two strings
            //    determine a grant: the key the file is written at, and the absolute target the
            //    document keeps. The key can be the shortened project-relative form, so a
            //    needle that matches only the absolute path leaves it untouched; probing the
            //    key alone would then grant access at a target the log does not hold.
            let masked = self.masked_targets(document, &dirs);
            let mut changes = Vec::new();
            let mut originals = Vec::new();
            for grant in grants {
                if self.mask_change(&grant.change) == grant.change
                    && !masked.contains(&grant.target)
                {
                    originals.push(grant.original);
                    changes.push(grant.change);
                } else {
                    refused.push(format!(
                        "`{}` — the path would be rewritten by the session's `[env]` mask",
                        grant.original
                    ));
                }
            }

            // 4. The message.
            let seq = self.append_log(turn, payload, submission_id, source)?;

            // 5. One replacement for the batch, then 6. the edits that make it survive the file
            //    being loaded again. A file that is not there (the built-in default role) has
            //    nothing to write; the edits alone carry the grant.
            if !changes.is_empty() {
                match self.apply_config_changes(&changes) {
                    Ok(()) => {
                        let edits = changes
                            .iter()
                            .zip(&originals)
                            .map(|(change, original)| {
                                ConfigEdit::new(EditState::Committed, change.clone()).with_note(
                                    format!(
                                        "reference grant: `{original}` was referenced by the \
                                         turn admitted at {seq}"
                                    ),
                                )
                            })
                            .collect::<Vec<_>>();
                        let batch = self.append_committed_locked(turn, &edits);
                        // The edits that landed keep their notices either way: a later failure
                        // must not hide a grant that is in force.
                        grant_notices = batch.notices;
                        if let Some(e) = batch.error {
                            self.warn(format_args!(
                                "reference grant: {} of {} grant edits appended: {e}",
                                batch.landed,
                                edits.len()
                            ));
                            partial = Some((batch.landed, edits.len(), e.to_string()));
                        }
                    }
                    Err(e) => {
                        // The file was not changed and nothing was folded, so no grant exists
                        // anywhere: the message stands under the perimeter it had.
                        self.warn(format_args!(
                            "reference grant: writing the config file failed: {e}"
                        ));
                        refused.push(format!(
                            "{} — the config file could not be written ({e})",
                            named(&originals)
                        ));
                    }
                }
            }
            (seq, pass)
        };

        // Everything that speaks happens with the section released: `notify` re-enters `append`
        // for a slot-less notification, and the `[env]` absorb may block on a resolver.
        let _ = raise_pass(self, pass);
        self.raise_config_notices(grant_notices);
        let notice = not_landed_notice(&refused, partial);
        if !notice.is_empty() {
            // One slot for every admission whose grants did not all land: a session hears the
            // latest set, and the body names each reference, so a superseded notice never hides
            // a live one.
            self.notify(Notification::new(
                "config",
                notice,
                Some("config:grant-refused".to_string()),
            ));
        }
        Ok(seq)
    }

    /// `change` as the log would store it (docs/env.md): the `[env]` mask applied exactly as an
    /// append applies it. Admission refuses a grant whose path this rewrites, rather than
    /// writing a key the log would not name.
    fn mask_change(&self, change: &ConfigChange) -> ConfigChange {
        let mut payload =
            Payload::ConfigEdit(ConfigEdit::new(EditState::Committed, change.clone()));
        self.mask_payload(&mut payload);
        match payload {
            // Masking rewrites text in place and never changes the payload's shape.
            Payload::ConfigEdit(edit) => edit.change,
            _ => change.clone(),
        }
    }
    /// The judged targets whose stored spelling the masker rewrites — the identity the log
    /// keeps for them, and the one a grant is founded on. The masking is the one the append
    /// will apply, run on a copy of the document, so a definition whose absolute `path` becomes
    /// a placeholder is found here rather than after the fact. A grant whose key is
    /// project-relative can leave this string unreached by [`Self::mask_change`], which is why
    /// the two are checked separately (docs/permissions.md).
    fn masked_targets(
        &self,
        document: &UserContent,
        dirs: &crate::config::policy::Directories,
    ) -> BTreeSet<PathBuf> {
        let mut probe = Payload::User(vec![ContentBlock::Document(document.clone())]);
        self.mask_payload(&mut probe);
        let Payload::User(blocks) = &probe else {
            return BTreeSet::new();
        };
        let Some(ContentBlock::Document(masked)) = blocks.first() else {
            return BTreeSet::new();
        };
        document
            .references
            .iter()
            .filter_map(|(id, definition)| {
                let ReferenceTarget::Filesystem { path, .. } = &definition.target else {
                    return None;
                };
                let after = masked.references.get(id).map(|masked| &masked.target);
                let Some(ReferenceTarget::Filesystem { path: after, .. }) = after else {
                    return None;
                };
                (after != path).then(|| dirs.judged(Path::new(path)))
            })
            .collect()
    }
}

/// The one notice an admission raises when not all of its grants landed (docs/permissions.md):
/// what was refused outright, what the file holds but the log does not, or both.
fn not_landed_notice(refused: &[String], partial: Option<(usize, usize, String)>) -> String {
    let mut notice = String::new();
    if !refused.is_empty() {
        notice.push_str(&format!(
            "Filesystem access for this turn's references was not added, so the agent \
             continues under the permissions it already had: {}.",
            refused.join("; ")
        ));
    }
    if let Some((landed, total, error)) = partial {
        if !notice.is_empty() {
            notice.push(' ');
        }
        notice.push_str(&format!(
            "The config file holds this turn's grants, but only {landed} of {total} were \
             recorded ({error}). The rest may take effect when the config file is next \
             refreshed, or when the session resumes."
        ));
    }
    notice
}

#[cfg(test)]
mod test;
