//! The dump grammar: the delimiter scheme, the record split, and what the
//! existing environment constraints add to it.

use super::*;

#[tokio::test]
async fn a_value_may_contain_newlines_and_equals_signs() {
    let cwd = Scratch::new("parser-values");
    // One printf, so the newlines inside `MULTI` survive to be part of its value.
    let hook = dumping_hook("MULTI=a\\nb=c\\nd\\000SIGNED=x=y=z\\000EMPTY=\\000");

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("hook ran");

    assert_eq!(
        entries(&established),
        BTreeMap::from([
            ("EMPTY".to_string(), String::new()),
            ("MULTI".to_string(), "a\nb=c\nd".to_string()),
            // Split at the *first* `=`, so the rest belongs to the value.
            ("SIGNED".to_string(), "x=y=z".to_string()),
        ]),
    );
}

#[tokio::test]
async fn the_empty_records_env_zero_ends_with_are_tolerated() {
    let cwd = Scratch::new("parser-trailing");
    // What `env -0` writes: a trailing delimiter, and here a doubled one.
    let hook = dumping_hook("A=1\\000B=2\\000\\000");

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("hook ran");

    assert_eq!(entries(&established).len(), 2);
}

#[tokio::test]
async fn no_output_with_a_successful_exit_is_an_empty_environment() {
    let cwd = Scratch::new("parser-empty");
    let hook = dumping_hook("");

    let established = launch(cwd.path(), Some(&hook), &host_env(&[("HOST", "gone")]))
        .await
        .expect("a deliberately cleared baseline is valid");

    assert!(
        entries(&established).is_empty(),
        "the hook reported no environment, so the session's base is empty",
    );
}

#[tokio::test]
async fn an_unterminated_final_record_is_rejected() {
    // A last record with no closing delimiter is what a truncated write looks
    // like; accepting it would seed a session with a partial environment.
    let cwd = Scratch::new("parser-unterminated");
    let hook = dumping_hook("A=1\\000B=hunter2");

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a dump that does not end in a delimiter is refused");

    assert!(
        matches!(&error, LaunchError::Unterminated { .. }),
        "expected Unterminated, got {error:?}",
    );
    assert!(
        !error.to_string().contains("hunter2"),
        "the diagnostic must not quote the record: {error}",
    );
}

#[tokio::test]
async fn a_record_without_an_equals_sign_is_rejected() {
    let cwd = Scratch::new("parser-no-equals");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        // The second record names no variable: no `=` at all.
        "printf 'A=1\\000TOKENhunter2\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a record with no `=` names no variable");

    assert!(
        matches!(&error, LaunchError::RecordWithoutName { record: 2, .. }),
        "expected the second record rejected, got {error:?}",
    );
    let text = error.to_string();
    assert!(
        text.contains("record 2"),
        "the position is reported: {text}"
    );
    assert!(
        !text.contains("hunter2"),
        "a malformed-output diagnostic must not quote the record: {text}",
    );
}

#[tokio::test]
async fn a_byte_that_is_not_utf8_is_rejected() {
    let cwd = Scratch::new("parser-not-utf8");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=\\377\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a non-UTF-8 value cannot become an environment entry");

    assert!(
        matches!(
            &error,
            LaunchError::RecordNotUtf8 {
                record: 1,
                part: "value",
                ..
            }
        ),
        "expected the value rejected, got {error:?}",
    );
}

#[tokio::test]
async fn a_name_that_is_not_utf8_is_rejected() {
    let cwd = Scratch::new("parser-bad-name");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '\\377=1\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a non-UTF-8 name cannot become an environment entry");

    assert!(
        matches!(
            &error,
            LaunchError::RecordNotUtf8 {
                record: 1,
                part: "name",
                ..
            }
        ),
        "expected the name rejected, got {error:?}",
    );
}

#[tokio::test]
async fn an_empty_variable_name_is_rejected_by_the_existing_constraints() {
    let cwd = Scratch::new("parser-empty-name");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '=novalue\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("an entry with no name is not an environment");

    assert!(
        matches!(&error, LaunchError::Environment { .. }),
        "the record splits cleanly, so this is the environment's own validation: \
         {error:?}",
    );
}
