use super::*;
use std::sync::Arc;

/// A resolver whose runner records the commands it was asked to run and answers
/// from a fixed table — no test ever invokes a real vault binary.
fn stub(answers: &'static [(&'static str, &'static str)]) -> (Resolver, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let resolver = Resolver::new(Box::new(move |command: &str| {
        recorder.lock().unwrap().push(command.to_string());
        answers
            .iter()
            .find(|(c, _)| *c == command)
            .map(|(_, v)| (*v).to_string())
            .ok_or_else(|| format!("stub has no answer for `{command}`"))
    }));
    (resolver, seen)
}

fn table(pairs: &[(&str, EnvEntry)]) -> BTreeMap<String, EnvEntry> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn bare(value: &str) -> EnvEntry {
    EnvEntry {
        bare: Some(value.to_string()),
        ..EnvEntry::default()
    }
}

#[test]
fn a_registered_scheme_resolves_through_its_command_template() {
    let (resolver, seen) = stub(&[("op read 'op://Private/Thing/credential'", "vault-value-1")]);
    let (values, errors) = resolver.resolve_table(&table(&[(
        "SOME_KEY",
        bare("op://Private/Thing/credential"),
    )]));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        values,
        vec![ResolvedValue {
            name: "SOME_KEY".into(),
            value: "vault-value-1".into(),
            secret: true,
        }]
    );
    // The reference is shell-quoted into the template, so it is one word.
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["op read 'op://Private/Thing/credential'"]
    );
}

#[test]
fn an_unregistered_scheme_is_a_literal() {
    let (resolver, seen) = stub(&[]);
    let (values, errors) =
        resolver.resolve_table(&table(&[("DATABASE_URL", bare("postgres://localhost/db"))]));
    assert!(errors.is_empty());
    assert_eq!(values[0].value, "postgres://localhost/db");
    assert!(!values[0].secret, "a literal is not masked by construction");
    assert!(seen.lock().unwrap().is_empty(), "nothing should have run");
}

#[test]
fn an_explicit_value_forces_a_literal_even_for_a_registered_scheme() {
    let (resolver, seen) = stub(&[]);
    let (values, errors) = resolver.resolve_table(&table(&[(
        "NOTE",
        EnvEntry {
            value: Some("op://Private/Thing/credential".into()),
            ..EnvEntry::default()
        },
    )]));
    assert!(errors.is_empty());
    assert_eq!(values[0].value, "op://Private/Thing/credential");
    assert!(!values[0].secret);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn cmd_is_the_primitive_a_scheme_is_sugar() {
    let (resolver, _) = stub(&[("security find-generic-password -s foo -w", "keychain-value")]);
    let (values, errors) = resolver.resolve_table(&table(&[(
        "OTHER",
        EnvEntry {
            cmd: Some("security find-generic-password -s foo -w".into()),
            ..EnvEntry::default()
        },
    )]));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(values[0].value, "keychain-value");
    assert!(values[0].secret, "a command's output is a secret");
}

#[test]
fn a_repeated_reference_runs_its_command_once() {
    let (resolver, seen) = stub(&[("op read 'op://V/I/f'", "shared-value")]);
    let entries = table(&[("A_KEY", bare("op://V/I/f")), ("B_KEY", bare("op://V/I/f"))]);
    resolver.resolve_table(&entries);
    // Re-resolution — what a config edit does — hits the same cache, so a vault
    // that prompts prompts once per reference for the session.
    let (values, _) = resolver.resolve_table(&entries);
    assert_eq!(values.len(), 2);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_failed_reference_names_the_reference_and_never_the_value() {
    let (resolver, _) = stub(&[]);
    let (values, errors) =
        resolver.resolve_table(&table(&[("SOME_KEY", bare("op://V/Missing/f"))]));
    assert!(values.is_empty());
    let message = errors[0].to_string();
    assert!(message.contains("SOME_KEY"), "{message}");
    assert!(message.contains("op://V/Missing/f"), "{message}");
}

#[test]
fn value_and_cmd_together_is_a_resolution_error() {
    let (resolver, _) = stub(&[]);
    let (values, errors) = resolver.resolve_table(&table(&[(
        "SOME_KEY",
        EnvEntry {
            value: Some("literal".into()),
            cmd: Some("echo other".into()),
            ..EnvEntry::default()
        },
    )]));
    assert!(values.is_empty());
    assert!(errors[0].detail.contains("both `value` and `cmd`"));
}

#[test]
fn an_entry_with_no_value_field_resolves_to_nothing() {
    let (resolver, seen) = stub(&[]);
    let (values, errors) = resolver.resolve_table(&table(&[(
        "*_KEY",
        EnvEntry {
            mask: Some(true),
            ..EnvEntry::default()
        },
    )]));
    assert!(values.is_empty() && errors.is_empty());
    assert!(seen.lock().unwrap().is_empty());
}

// ------------------------------------------------------------- the real runner

#[test]
fn the_runner_returns_stdout_without_its_trailing_newline() {
    let value = run_command("echo resolved-value", Duration::from_secs(10)).unwrap();
    assert_eq!(value, "resolved-value");
}

#[test]
fn a_nonzero_exit_is_an_error_carrying_stderr() {
    let error = run_command("echo nope 1>&2; exit 3", Duration::from_secs(10)).unwrap_err();
    assert!(error.contains("exited 3"), "{error}");
    assert!(error.contains("nope"), "{error}");
}

#[test]
fn a_command_that_outlives_the_timeout_is_killed() {
    let error = run_command("sleep 30", Duration::from_millis(100)).unwrap_err();
    assert!(error.contains("did not finish"), "{error}");
}
