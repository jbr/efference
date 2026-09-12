use super::*;

#[test]
fn names_are_prefixed_by_population() {
    assert_eq!(inference_name("work"), "inference:work");
    assert_eq!(
        mcp_name("https://mcp.example/mcp"),
        "mcp:https://mcp.example/mcp"
    );
}

#[test]
fn memory_store_round_trips_replaces_and_deletes() {
    let secrets = Secrets::in_memory();
    assert_eq!(secrets.get("inference:a").unwrap(), None);
    secrets.set("inference:a", "one").unwrap();
    assert_eq!(secrets.get("inference:a").unwrap().as_deref(), Some("one"));
    secrets.set("inference:a", "two").unwrap();
    assert_eq!(secrets.get("inference:a").unwrap().as_deref(), Some("two"));
    secrets.delete("inference:a").unwrap();
    assert_eq!(secrets.get("inference:a").unwrap(), None);
    // Absent is not an error.
    secrets.delete("inference:a").unwrap();
}

#[test]
fn errors_name_the_operation_and_item_but_never_the_value() {
    let error = SecretError {
        operation: "write",
        name: "inference:a".into(),
        detail: "keychain locked".into(),
    };
    assert_eq!(
        error.to_string(),
        "could not write secret `inference:a`: keychain locked"
    );
}

/// The `security -i` line: a value with quotes, backslashes, a `$`, and a
/// newline-free shell metacharacter soup arrives as one quoted word, and the
/// line ends with the newline the interactive parser needs.
#[test]
fn security_script_quotes_the_value_as_one_word() {
    let script = security_script("inference:a", r#"sk-"x"\y $z 'q' `w`"#);
    assert_eq!(
        script,
        "add-generic-password -U -s \"efference\" -a \"inference:a\" -w \"sk-\\\"x\\\"\\\\y $z 'q' `w`\"\n"
    );
}

/// A real keychain round trip through `security`, on macOS only. The item is
/// removed afterward; the name is unique to the process so parallel runs do
/// not collide.
#[cfg(target_os = "macos")]
#[test]
fn security_cli_round_trips_a_value_with_quotes() {
    let secrets = Secrets {
        backend: Backend::SecurityCli,
    };
    let name = format!("test:{}", std::process::id());
    let value = r#"sk-"x"\y $z 'q'"#;
    assert_eq!(secrets.get(&name).unwrap(), None);
    secrets.set(&name, value).unwrap();
    assert_eq!(secrets.get(&name).unwrap().as_deref(), Some(value));
    secrets.set(&name, "replaced").unwrap();
    assert_eq!(secrets.get(&name).unwrap().as_deref(), Some("replaced"));
    secrets.delete(&name).unwrap();
    assert_eq!(secrets.get(&name).unwrap(), None);
    secrets.delete(&name).unwrap();
}

/// A `cargo test` binary is ad-hoc signed, so detection picks `security`.
#[cfg(target_os = "macos")]
#[test]
fn a_test_binary_is_not_identity_signed() {
    assert!(!identity_signed());
    assert_eq!(
        Secrets::detect().unwrap().describe(),
        "the login keychain, through `security`"
    );
}
