use super::*;

#[test]
fn an_env_hook_naming_no_program_is_refused() {
    // Reading `[]` as "no hook" would make a typo read as a deliberate choice to
    // inherit the host environment.
    let error = match InferenceConfig::from_toml(
        r#"
env_hook = []

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    ) {
        Ok(_) => panic!("an empty env_hook unexpectedly parsed"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("names no program"), "{error}");
}

#[test]
fn an_env_hook_is_not_a_shell_command_string() {
    // The array is the whole spelling: a bare string would be run through
    // whatever shell the host happened to have, which is the thing the argv form
    // exists to avoid.
    let error = match InferenceConfig::from_toml(
        r#"
env_hook = "direnv exec . /usr/bin/env -0"

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    ) {
        Ok(_) => panic!("a bare env_hook string unexpectedly parsed"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("invalid type"), "{error}");
}
