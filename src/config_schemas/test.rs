use super::*;
use std::path::Path;

/// The committed schemas are what the generator emits now. A config type change that
/// moves a schema fails here; regenerate with `EFFERENCE_WRITE_SCHEMAS=1 cargo test
/// config_schemas` and commit the result, which is what efference.net publishes.
#[test]
fn committed_schemas_match_the_generator() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR);
    let write = std::env::var_os("EFFERENCE_WRITE_SCHEMAS").is_some();
    for (name, schema) in all() {
        let path = dir.join(name);
        let generated = render(&schema);
        if write {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &generated).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert!(
            committed == generated,
            "{} differs from the generated schema; regenerate with \
             EFFERENCE_WRITE_SCHEMAS=1 cargo test config_schemas",
            path.display()
        );
    }
}

/// The default session config names the project schema at the URL it is published under,
/// on its first line, where Taplo reads a header directive. `CONFIG_SCAFFOLD` is a literal,
/// so this is what keeps it in step with [`directive`].
#[test]
fn the_session_scaffold_opens_with_the_schema_line() {
    assert!(crate::boot::CONFIG_SCAFFOLD.starts_with(&format!("{}\n", directive(PROJECT))));
}

#[test]
fn directives_return_to_the_first_lines() {
    let original = "#:schema https://example/s.json\n# note\n";
    assert_eq!(
        keep_directives_first(
            original,
            "tools = []\n#:schema https://example/s.json\n# note\n".into()
        ),
        "#:schema https://example/s.json\ntools = []\n# note\n"
    );
    let unmoved = "#:schema https://example/s.json\nscope = \"x\"\n";
    assert_eq!(keep_directives_first(original, unmoved.into()), unmoved);
    assert_eq!(
        keep_directives_first("scope = \"x\"\n", "tools = []\n".into()),
        "tools = []\n"
    );
}
