use super::*;
use crate::config::env_table::{ChildEnv, EnvPolicy, Resolver};

fn snapshot(entries: &[(&str, &str)]) -> EnvSnapshot {
    EnvSnapshot::new(
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
    .unwrap()
}

#[tokio::test]
async fn child_policy_replaces_inheritance_in_std_and_tokio() {
    let base = snapshot(&[("KEEP", "base"), ("REMOVE", "stripped")]);
    let child = ChildEnv::from_snapshot(base, vec!["REMOVE".into()]).with_inject(
        [
            ("KEEP".into(), "override".into()),
            ("ADDED".into(), "new".into()),
        ]
        .into(),
    );
    let mut std_cmd = std::process::Command::new("/usr/bin/env");
    std_cmd.env("NOT_IN_SNAPSHOT", "must-not-survive");
    child.apply(&mut std_cmd);
    let mut tokio_cmd = tokio::process::Command::new("/usr/bin/env");
    tokio_cmd.env("NOT_IN_SNAPSHOT", "must-not-survive");
    child.apply_tokio(&mut tokio_cmd);
    // Through the spawn lock, as every spawn in this binary must be (src/platform/spawn.rs).
    tokio_cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let tokio_output = crate::platform::spawn::tokio(&mut tokio_cmd)
        .unwrap()
        .wait_with_output()
        .await
        .unwrap();
    for output in [
        crate::platform::spawn::std_output(&mut std_cmd).unwrap(),
        tokio_output,
    ] {
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let mut lines: Vec<_> = text.lines().collect();
        lines.sort();
        assert_eq!(lines, ["ADDED=new", "KEEP=override"]);
    }
}

#[test]
fn snapshots_validate_and_debug_never_prints_values() {
    let base = snapshot(&[("IDENTITY", "private-value")]);
    assert_eq!(base.get("IDENTITY"), Some("private-value"));
    let child = ChildEnv::from_snapshot(base.clone(), vec![])
        .with_inject([("INJECTED".into(), "injected-secret".into())].into());
    let debug = format!("{base:?} {child:?}");
    assert!(!debug.contains("private-value"));
    assert!(!debug.contains("injected-secret"));
    for (name, value) in [
        ("", "value"),
        ("A=B", "value"),
        ("A\0B", "value"),
        ("A", "bad\0value"),
    ] {
        assert!(EnvSnapshot::new([(name.into(), value.into())].into()).is_err());
    }
}

#[test]
fn references_and_masking_edits_use_each_sessions_base() {
    let cwd = std::env::current_dir().unwrap();
    let role =
        crate::config::Config::from_toml("[env]\nFETCHED = { cmd = 'printf %s \"$IDENTITY\"' }\n")
            .unwrap();
    for identity in ["first-private-value", "second-private-value"] {
        let base = snapshot(&[("IDENTITY", identity)]);
        let resolver = Arc::new(Resolver::with_environment(cwd.clone(), base.clone()));
        let (values, errors) = resolver.resolve_table(&role.env);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(values[0].value, identity);
        let policy = EnvPolicy::build_snapshot(&BTreeMap::new(), base, &[], resolver).policy;
        assert_eq!(policy.mask(identity), identity);
        let edit = crate::config::Config::from_toml("[env]\nIDENTITY = { mask = true }\nNEW = { cmd = 'printf %s \"${IDENTITY}-later\"' }\n").unwrap();
        assert!(policy.absorb_config(&edit.env).is_empty());
        assert_ne!(policy.mask(identity), identity);
        assert!(!policy.mask(&format!("{identity}-later")).contains(identity));
    }
}
