//! The `[updates]` question (docs/update-check.md): asked, yes preselected, wherever
//! neither the file nor the run has an answer, and changeable from the management list.

use super::*;

const QUESTION: &str = "confirm: Check for new versions of efference? [default yes]";

/// A first run asks last, with yes preselected, and writes the answer either way.
#[tokio::test]
async fn a_first_run_asks_and_writes_the_answer() {
    let probe = Table {
        env: BTreeMap::from([("ANTHROPIC_API_KEY".to_string(), "sk-real".to_string())]),
        accepted_keys: strings(&["sk-real"]),
        ..Default::default()
    };
    for (answer, written, said) in [
        (Answer::Yes, true, "efference will check once a day"),
        (Answer::No, false, "efference will not check"),
    ] {
        let (draft, script) = run_script(
            vec![
                Answer::Pick("Anthropic"),
                Answer::Type(""),
                Answer::Pick("An environment variable"),
                Answer::Type(""),
                Answer::No,
                answer,
            ],
            &probe,
            "",
        )
        .await;
        let draft = draft.unwrap();
        assert!(script.said(QUESTION), "{:?}", script.transcript);
        assert!(script.said("efference.net once a day"));
        assert!(script.said(said));
        assert_eq!(draft.update_check, Some(written));
        let text = render("", &draft).unwrap();
        assert!(
            text.ends_with(&format!("\n[updates]\ncheck = {written}\n")),
            "{text}"
        );
        let parsed = crate::config::user_config::InferenceConfig::from_toml(&text);
        assert!(parsed.is_ok());
    }
}

/// A file that already answered is not asked again, and its answer is left as
/// written.
#[tokio::test]
async fn an_answered_file_is_not_asked_again() {
    let existing = r#"
[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }

[updates]
check = false # decided
"#;
    let (draft, script) = run_script(vec![Answer::Pick("Done")], &Table::default(), existing).await;
    let draft = draft.unwrap();
    assert!(!script.said("Check for new versions"));
    assert_eq!(draft.update_check, None);
    assert_eq!(render(existing, &draft).unwrap(), existing);
}

/// The management list states the answer, and its entry changes it, preselecting
/// what the file says; leaving after that asks nothing more.
#[tokio::test]
async fn the_management_entry_changes_the_answer() {
    let existing = r#"
[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }

[updates]
check = true
"#;
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Update check"),
            Answer::No,
            Answer::Pick("Done"),
        ],
        &Table::default(),
        existing,
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said(QUESTION), "{:?}", script.transcript);
    assert_eq!(draft.update_check, Some(false));
    let text = render(existing, &draft).unwrap();
    assert!(text.contains("[updates]\ncheck = false\n"), "{text}");
    assert!(!text.contains("check = true"), "{text}");
}

/// The entry's summary reads the file's answer, and an unanswered file says so.
#[test]
fn the_management_summary_reads_the_answer() {
    assert_eq!(super::super::updates::summary(Some(true)), "once a day");
    assert_eq!(super::super::updates::summary(Some(false)), "off");
    assert_eq!(super::super::updates::summary(None), "not answered");
}

/// An answer that is not a boolean is refused when the file is read, as a host
/// would refuse it.
#[test]
fn a_non_boolean_answer_is_an_error() {
    let error = Existing::from_toml("[updates]\ncheck = \"yes\"\n").unwrap_err();
    assert!(error.contains("[updates]"), "{error}");
    assert_eq!(
        Existing::from_toml("[updates]\ncheck = true\n")
            .unwrap()
            .update_check,
        Some(true)
    );
    assert_eq!(Existing::from_toml("").unwrap().update_check, None);
}
