//! Initial session facts recorded in the header (docs/log-and-projection.md).

use super::SessionRequest;
use crate::log::{LoggedInference, RecordedRoles, TriageModel};
use crate::persist::SessionMetadata;

/// The spotter and triage selections `inference` resolved, in the shape the log records.
pub(super) fn roles(inference: &crate::inference::SessionInference) -> RecordedRoles {
    let selection = |value: &crate::inference::ModelSelection| {
        LoggedInference::new(&*value.connection, value.provider, &*value.model)
    };
    RecordedRoles {
        spotter: selection(inference.spotter().0),
        triage: match inference.triage() {
            None => TriageModel::Shared,
            Some((model, _, settings)) => TriageModel::Separate {
                selection: selection(model),
                thinking: settings.thinking,
                auto_choice: settings.auto_choice,
                effort: settings.effort.clone(),
            },
        },
    }
}

pub(super) fn metadata(request: &SessionRequest) -> SessionMetadata {
    let main = {
        let (main, _) = request.inference.main();
        LoggedInference::new(&*main.connection, main.provider, &*main.model)
    };
    let RecordedRoles { spotter, triage } = roles(&request.inference);
    let mut metadata = SessionMetadata::new(
        request
            .session_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        request.directories.project(),
        main,
        spotter,
        triage,
    );
    let working = request.directories.working();
    if working != request.directories.project() {
        metadata.working = Some(working.to_path_buf());
    }
    let create = &request.create;
    metadata.temperature = create.options.temperature;
    metadata.effort = create.options.effort.clone();
    metadata.upstream = create.upstream.clone();
    metadata.created_by = request.lineage.created_by.clone();
    metadata.succeeds = request.lineage.succeeds.clone();
    metadata.autonomous = create.autonomous;
    metadata.keep_scratch = create.keep_scratch;
    metadata.detached = create.detached;
    metadata
}
