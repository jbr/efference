//! The model selections a session's log records (docs/resumption.md): what a resume
//! re-resolves when its switch leaves a role out, and what a dormant session's row
//! reports for a client to prefill a switch from (docs/control-api.md).

use crate::inference::{ResumeSwitch, TriageChoice};
use crate::log::{LoggedInference, RecordedRoles};
use crate::persist::{LoadedRecording, TriageModel};

/// The main, spotter and triage selections the session last ran on, in the shapes an
/// `active` patch takes. A role the log records nothing for is left out: a log with no
/// header metadata names only its main inference, and only when its boot preamble
/// carries one.
pub fn recorded_selections(recording: &LoadedRecording) -> ResumeSwitch {
    let mut selections = ResumeSwitch::default();
    if let Some(main) = recorded_main(recording) {
        selections.connection = Some(main.connection);
        selections.model = Some(main.model);
    }
    if let Some(RecordedRoles { spotter, triage }) = recorded_roles(recording) {
        selections.spotter_connection = Some(spotter.connection);
        selections.spotter_model = Some(spotter.model);
        selections.triage = Some(match triage {
            TriageModel::Shared => TriageChoice::Shared,
            TriageModel::Separate {
                selection,
                thinking,
                auto_choice,
                effort,
            } => TriageChoice::Separate {
                connection: Some(selection.connection),
                model: selection.model,
                thinking,
                auto_choice,
                effort,
            },
        });
    }
    selections
}

/// The spotter and triage the session last ran on: the latest preamble's roles when it
/// records any (a resume switched them), else the header's. `None` for a log with
/// neither.
pub(super) fn recorded_roles(recording: &LoadedRecording) -> Option<RecordedRoles> {
    if let Some(roles) = crate::log::recorded_roles(&recording.entries) {
        return Some(roles.clone());
    }
    let metadata = recording.header.metadata()?;
    Some(RecordedRoles {
        spotter: metadata.spotter.clone(),
        triage: metadata.triage.clone(),
    })
}

/// The main inference the session last ran on: the latest preamble's identity, else the
/// header's.
pub(super) fn recorded_main(recording: &LoadedRecording) -> Option<LoggedInference> {
    crate::log::recorded_inference(&recording.entries)
        .cloned()
        .or_else(|| recording.header.metadata().map(|m| m.main.clone()))
}
