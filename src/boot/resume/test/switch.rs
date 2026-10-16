//! Switching the spotter and triage models at resume (docs/resumption.md).

use super::*;
use crate::inference::{ResumeSwitch, TriageChoice};
use crate::log::RecordedRoles;
use crate::persist::TriageModel;

fn switch_to_terra_and_luna() -> ResumeSwitch {
    ResumeSwitch {
        spotter_connection: Some("oai".into()),
        spotter_model: Some("gpt-5.6-luna".into()),
        triage: Some(TriageChoice::separate(
            "oai/gpt-5.6-terra".into(),
            false,
            false,
            None,
        )),
    }
}

fn preambles(entries: &[crate::log::LogEntry]) -> Vec<&crate::log::LogEntry> {
    entries
        .iter()
        .filter(|e| matches!(e.payload, Payload::Preamble { .. }))
        .collect()
}

/// A switch from shared triage to a separate model, and to another spotter, is recorded in a
/// preamble that repeats the frozen one; the main projection is the same with and without
/// that entry, and a second resume with no switch keeps the switched roles.
#[tokio::test]
async fn a_resume_switches_spotter_and_triage_and_the_next_keeps_them() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-switch");
    left_behind(&root, "sw", |log| {
        log.append(1, Payload::User(vec![ContentBlock::Text("hi".into())]))
            .unwrap();
    })
    .await;
    let registry = codex_registry(root.path(), "sw");

    let mut request = resume_request(&root, "sw", &registry);
    request.switch = switch_to_terra_and_luna();
    let (boot, options) = resume(request).await.expect("a switched resume");
    assert_eq!(options.spotter_model.as_deref(), Some("gpt-5.6-luna"));
    let identity = boot.inference.clone().unwrap();
    assert!(!identity.triage.shared);
    assert_eq!(identity.triage.selection.model, "gpt-5.6-terra");
    assert_eq!(identity.spotter.model, "gpt-5.6-luna");

    let entries = boot.log.entries().to_vec();
    let both = preambles(&entries);
    assert_eq!(both.len(), 2, "the boot preamble and the switch's");
    let (Payload::Preamble { system: first, .. }, Payload::Preamble { system, roles, .. }) =
        (&both[0].payload, &both[1].payload)
    else {
        unreachable!()
    };
    assert_eq!(first, system, "the frozen system text is repeated verbatim");
    let RecordedRoles { spotter, triage } = roles.clone().expect("the switch records roles");
    assert_eq!(spotter.model, "gpt-5.6-luna");
    assert!(
        matches!(&triage, TriageModel::Separate { selection, .. } if selection.model == "gpt-5.6-terra"),
        "{triage:?}"
    );
    assert_eq!(
        crate::log::recorded_inference(&entries),
        crate::log::recorded_inference(&entries[..both[1].seq as usize]),
        "the main identity is unchanged"
    );

    // The preamble projects nothing: removing it leaves the wire as it was.
    let without: Vec<_> = entries
        .iter()
        .filter(|e| e.seq != both[1].seq)
        .cloned()
        .collect();
    assert_eq!(
        serde_json::to_string(&crate::project::project(&entries)).unwrap(),
        serde_json::to_string(&crate::project::project(&without)).unwrap(),
    );
    drop_boot(boot);

    // No switch: the recorded (switched) roles stand, and nothing new is written.
    let (boot, options) = resume(resume_request(&root, "sw", &registry))
        .await
        .expect("a second resume");
    assert_eq!(options.spotter_model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(
        boot.inference.as_ref().unwrap().triage.selection.model,
        "gpt-5.6-terra"
    );
    assert_eq!(preambles(boot.log.entries()).len(), 2);
    drop_boot(boot);

    // Naming what the session already runs records nothing; switching triage back to
    // shared does.
    let mut request = resume_request(&root, "sw", &registry);
    request.switch = switch_to_terra_and_luna();
    let (boot, _) = resume(request).await.expect("an unchanged switch");
    assert_eq!(preambles(boot.log.entries()).len(), 2);
    drop_boot(boot);
    let mut request = resume_request(&root, "sw", &registry);
    request.switch.triage = Some(TriageChoice::Shared);
    let (boot, options) = resume(request).await.expect("back to shared");
    assert_eq!(options.spotter_model.as_deref(), Some("gpt-5.6-luna"));
    assert!(boot.inference.as_ref().unwrap().triage.shared);
    assert_eq!(preambles(boot.log.entries()).len(), 3);
    drop_boot(boot);
}

/// A selection the creation checks refuse refuses the resume, and leaves the directory as it was.
#[tokio::test]
async fn a_switch_creation_would_refuse_refuses_the_resume() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-switch-refused");
    left_behind(&root, "bad", |_| {}).await;
    let registry = codex_registry(root.path(), "bad");
    let before = dir_contents(&root.session_dir("bad"));

    let mut request = resume_request(&root, "bad", &registry);
    request.switch.spotter_connection = Some("nowhere".into());
    request.switch.spotter_model = Some("m".into());
    let error = failure(resume(request).await, "an unknown connection is refused");
    assert!(error.to_string().contains("spotter_model"), "{error}");
    assert_eq!(dir_contents(&root.session_dir("bad")), before);
}
