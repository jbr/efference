use super::*;
use crate::env_table::{ChildEnv, EnvPolicy, Resolver};

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
    let base = snapshot(&[
        ("KEEP", "base"),
        ("REMOVE", "stripped"),
        (crate::auth::ROOT_ENV, "host-only-secret"),
    ]);
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
    for output in [std_cmd.output().unwrap(), tokio_cmd.output().await.unwrap()] {
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let mut lines: Vec<_> = text.lines().collect();
        lines.sort();
        assert_eq!(lines, ["ADDED=new", "KEEP=override"]);
    }
}

#[test]
fn snapshots_validate_and_debug_never_prints_values() {
    let base = snapshot(&[
        ("IDENTITY", "private-value"),
        (crate::auth::ROOT_ENV, "root-secret"),
    ]);
    assert_eq!(base.get(crate::auth::ROOT_ENV), None);
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

#[test]
fn provider_lookup_does_not_fall_back_to_host_credentials() {
    let cwd = std::env::current_dir().unwrap();
    let empty = EnvSnapshot::default();
    assert!(crate::api::Endpoint::from_snapshot(&cwd, &empty, &BTreeMap::new()).is_err());
    let base = snapshot(&[
        ("EFFERENCE_PROVIDER", "openrouter"),
        ("EFFERENCE_API_KEY", "caller-key"),
    ]);
    let endpoint = crate::api::Endpoint::from_snapshot(&cwd, &base, &BTreeMap::new()).unwrap();
    assert_eq!(endpoint.auth_value, "Bearer caller-key");
    let resolved = [
        ("EFFERENCE_PROVIDER".into(), "anthropic".into()),
        ("EFFERENCE_API_KEY".into(), "resolved-key".into()),
    ]
    .into();
    let endpoint = crate::api::Endpoint::from_snapshot(&cwd, &base, &resolved).unwrap();
    assert_eq!(endpoint.auth_value, "resolved-key");
    let codex = snapshot(&[("EFFERENCE_PROVIDER", "openai-codex")]);
    let error = crate::api::Endpoint::from_snapshot(&cwd, &codex, &BTreeMap::new()).unwrap_err();
    assert!(error.to_string().contains("neither CODEX_HOME nor HOME"));
}
