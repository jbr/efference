//! Switching the spotter and triage models at resume (docs/resumption.md).

use super::*;
use crate::inference::{ResumeSwitch, TriageChoice};
use crate::log::{RecordedRoles, TriageModel};

fn switch_to_terra_and_luna() -> ResumeSwitch {
    ResumeSwitch {
        connection: None,
        model: None,
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

fn preambles(entries: &[LogEntry]) -> Vec<&LogEntry> {
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
    let (Payload::Preamble { roles, .. }, Some(first), Some(system)) = (
        &both[1].payload,
        both[0].payload.preamble_system(),
        both[1].payload.preamble_system(),
    ) else {
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

/// The registry behind [`codex_registry`] plus a Messages-dialect connection, so a switch
/// can name a model on the other side of the dialect line. Nothing contacts `msg`: the
/// dialect check refuses before capabilities are read.
fn two_dialect_registry(root: &std::path::Path, id: &str) -> InferenceRegistry {
    let codex_home = root.join(format!("codex-home-{id}"));
    let _ = codex_registry(root, id);
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[connections.oai]
provider = "openai-codex"
auth = { kind = "codex-cache" }

[connections.msg]
dialect = "messages"
base-url = "http://127.0.0.1:9/v1"
auth = { kind = "env", key = "EFFERENCE_SWITCH_TEST_KEY" }
"#,
    )
    .unwrap();
    let host_env = crate::env_table::EnvSnapshot::new(
        [
            (
                "CODEX_HOME".to_string(),
                codex_home.to_string_lossy().into_owned(),
            ),
            (
                "EFFERENCE_SWITCH_TEST_KEY".to_string(),
                "not-a-real-credential".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    InferenceRegistry::from_config(
        config,
        std::path::Path::new("/host"),
        &host_env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
}

fn assistant_turn() -> Payload {
    Payload::Assistant {
        output: AssistantOutput::Responses(Vec::new()),
        excluded: None,
    }
}

/// A main switch within the dialect replaces the identity line over the same body and
/// records the new inference; the session starts with its prefix marked lost, and a
/// later resume keeps the new model.
#[tokio::test]
async fn a_resume_switches_the_main_model_within_a_dialect() {
    let root = Root::new("resume-switch-main");
    left_behind(&root, "mn", |log| {
        log.append(1, Payload::User(vec![ContentBlock::Text("hi".into())]))
            .unwrap();
        log.append(1, assistant_turn()).unwrap();
    })
    .await;
    let registry = codex_registry(root.path(), "mn");

    let mut request = resume_request(&root, "mn", &registry);
    request.switch.model = Some("oai/gpt-5.6-terra".into());
    let (boot, options) = resume(request).await.expect("a main switch");
    assert_eq!(options.model.as_deref(), Some("gpt-5.6-terra"));
    assert!(boot.cache_lost_at.is_some(), "the new prefix starts cold");
    assert_eq!(boot.model, "gpt-5.6-terra");

    let entries = boot.log.entries().to_vec();
    let both = preambles(&entries);
    assert_eq!(both.len(), 2);
    let (
        Payload::Preamble {
            identity: old_identity,
            body: old_body,
            ..
        },
        Payload::Preamble {
            identity,
            body,
            inference,
            roles,
            ..
        },
    ) = (&both[0].payload, &both[1].payload)
    else {
        unreachable!()
    };
    assert_eq!(old_identity, "You are gpt-5.6.");
    assert_eq!(identity, "You are gpt-5.6-terra.");
    assert_eq!(body, old_body, "the frozen body is repeated verbatim");
    assert_eq!(inference.as_ref().unwrap().model, "gpt-5.6-terra");
    assert!(roles.is_some(), "the latest preamble carries every role");
    assert_eq!(
        crate::log::recorded_inference(&entries).unwrap().model,
        "gpt-5.6-terra"
    );
    drop_boot(boot);

    // The next resume reads the switched model from the latest preamble, not the header,
    // and a switch naming the model it already runs records nothing.
    let (boot, options) = resume(resume_request(&root, "mn", &registry))
        .await
        .expect("a second resume");
    assert_eq!(options.model.as_deref(), Some("gpt-5.6-terra"));
    assert!(boot.cache_lost_at.is_none());
    assert_eq!(preambles(boot.log.entries()).len(), 2);
    drop_boot(boot);
    let mut request = resume_request(&root, "mn", &registry);
    request.switch.model = Some("oai/gpt-5.6-terra".into());
    let (boot, _) = resume(request).await.expect("an unchanged main");
    assert!(boot.cache_lost_at.is_none());
    assert_eq!(preambles(boot.log.entries()).len(), 2);
    drop_boot(boot);
}

/// A model on another wire dialect than the recorded turns is refused, naming both, and
/// so is a log whose prompt has no identity line to replace.
#[tokio::test]
async fn a_main_switch_across_dialects_is_refused() {
    let root = Root::new("resume-switch-dialect");
    left_behind(&root, "xd", |log| {
        log.append(1, assistant_turn()).unwrap();
    })
    .await;
    left_behind(&root, "bare", |log| {
        let (body, context, inference) = match &log.entries()[0].payload {
            Payload::Preamble {
                identity,
                body,
                context,
                inference,
                ..
            } => (
                format!("{identity}{body}"),
                context.clone(),
                inference.clone(),
            ),
            other => panic!("the preamble is first: {other:?}"),
        };
        log.append(0, Payload::preamble("", body, context, inference))
            .unwrap();
    })
    .await;
    let registry = two_dialect_registry(root.path(), "xd");
    let before = dir_contents(&root.session_dir("xd"));

    let mut request = resume_request(&root, "xd", &registry);
    request.switch.connection = Some("msg".into());
    request.switch.model = Some("some-model".into());
    let error = failure(resume(request).await, "a cross-dialect switch is refused");
    let message = error.to_string();
    assert!(
        message.contains("Responses") && message.contains("Messages"),
        "{message}"
    );
    assert_eq!(dir_contents(&root.session_dir("xd")), before);

    let mut request = resume_request(&root, "bare", &registry);
    request.switch.model = Some("oai/gpt-5.6-terra".into());
    let error = failure(resume(request).await, "no identity line to replace");
    assert!(error.to_string().contains("identity line"), "{error}");
}
