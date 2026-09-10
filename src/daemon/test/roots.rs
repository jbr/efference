//! Session-root isolation without changing process cwd (docs/control-channel.md).

use super::*;

struct Roots(PathBuf);

impl Roots {
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-roots-{nonce:016x}"));
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        Self(root.canonicalize().unwrap())
    }
}

impl Drop for Roots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_resolve_policy_config_references_and_children_in_their_own_roots() {
    use crate::policy::{Verdict, bash_tokens, classify, is_config_write, missing_grants};
    let roots = Roots::new();
    let first = roots.0.join("one");
    let second = roots.0.join("two");
    let mut sessions = Vec::new();
    for (cwd, label) in [(&first, "one"), (&second, "two")] {
        std::fs::write(cwd.join("identity.txt"), label).unwrap();
        std::fs::write(cwd.join("efference.toml"), format!(
            "scope = '{label}'\ntools = ['read', 'write']\n[paths]\n'.' = true\n[env]\nIDENTITY = {{ cmd = 'cat identity.txt' }}\n"
        )).unwrap();
        let role = crate::config::load_in(cwd, None).unwrap();
        assert_eq!(role.source, ConfigSource::Path(cwd.join("efference.toml")));
        let resolver = crate::env_table::Resolver::at_cwd(cwd.clone());
        let (values, errors) = resolver.resolve_table(&role.role.env);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(values[0].value, label);
        let mut role = role;
        role.source = ConfigSource::Path("efference.toml".into());
        let (session, _turns) = Session::with_role(cwd.clone(), Log::in_memory(), role);
        assert_eq!(
            session.config_path(),
            Some(cwd.join("efference.toml").as_path())
        );
        let (values, errors) = session
            .env_policy
            .resolver()
            .resolve_table(&session.effective_config().env);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(values[0].value, label);
        let toolbox = crate::tools::Toolbox::bash_and_mcps(cwd.clone(), None).unwrap();
        let foreground = toolbox
            .run(
                &ToolCall::new(
                    "bash",
                    "foreground-root",
                    serde_json::json!({"command": "cat identity.txt"}),
                ),
                std::future::pending(),
                |_| {},
                |_| {},
            )
            .await;
        let crate::bash::Outcome::Done(result) = foreground else {
            session.kill_all_bg();
            panic!("short foreground command unexpectedly detached");
        };
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(result.content, label);
        let handle = session
            .spawn_background("cat identity.txt".into(), None)
            .unwrap();
        sessions.push((session, handle, label));
    }
    let call = ToolCall::new(
        "read",
        "read-root",
        serde_json::json!({"paths": [first.join("identity.txt")]}),
    );
    let write = ToolCall::new(
        "write",
        "write-config",
        serde_json::json!({"path": "efference.toml", "contents": ""}),
    );
    for (session, handle, label) in &sessions {
        let role = session.effective_config();
        let is_first = *label == "one";
        assert_eq!(
            classify(session.cwd(), &role, &call),
            if is_first {
                Verdict::Clear
            } else {
                Verdict::Gate
            }
        );
        assert_eq!(
            missing_grants(session.cwd(), &role, &call).is_none(),
            is_first
        );
        assert_eq!(
            is_config_write(session.cwd(), &write, &first.join("efference.toml")),
            is_first
        );
        let command = format!("cd '{}' && cargo test", first.display());
        assert_eq!(
            bash_tokens(session.cwd(), &role, &command).is_some(),
            is_first
        );
        let completed = tokio::time::timeout(Duration::from_secs(5), async {
            while !session
                .pending_notifications()
                .iter()
                .any(|n| n.source == *handle)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if completed.is_err() {
            for (session, _, _) in &sessions {
                session.kill_all_bg();
            }
            panic!("background command did not finish");
        }
        assert_eq!(
            session.process_output_read(1, None).unwrap().content,
            *label
        );
    }
}

#[test]
fn rooted_config_loading_preserves_optional_default_and_required_explicit_paths() {
    let roots = Roots::new();
    let cwd = roots.0.join("one");
    assert_eq!(
        crate::config::load_in(&cwd, None).unwrap().source,
        ConfigSource::Default
    );
    assert!(crate::config::load_in(&cwd, Some(std::path::Path::new("missing.toml"))).is_err());
}
