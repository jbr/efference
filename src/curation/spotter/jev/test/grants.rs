//! Whether the harness places a move outside the grants (`state::outside_grants`), the
//! one input to the fold that is not an answer (docs/permissions.md).

use super::*;
use crate::config::policy::Directories;
use crate::record::names::tools::BASH;

fn role() -> Config {
    ConfigFixture::new()
        .with_path(".", true, true)
        .with_path(".efference", false, false)
        .with_path("../docs", true, false)
        .build()
}

fn outside(call: &ToolCall) -> bool {
    let dirs = Directories::at(std::path::Path::new("/work/proj"));
    state::outside_grants(
        &role(),
        call,
        Some(&dirs),
        Some(std::path::Path::new("/tmp/scratch")),
    )
}

fn bash(command: &str) -> ToolCall {
    ToolCall::new(BASH, "c1", json!({ "command": command }))
}

/// A shell command is outside when a path it names is under no grant or a denied one,
/// or expands from the home directory; a granted path, the scratch directory and a
/// command naming no path are inside.
#[test]
fn a_shell_command_is_judged_by_its_path_facts() {
    let cases = [
        ("cargo test", false),
        ("cat src/lib.rs", false),
        ("cat ../docs/a.md", false),
        ("cat \"$TMPDIR/out.txt\"", false),
        ("cat /etc/hosts", true),
        ("cat ../other/x", true),
        ("cat .efference/s/log.jsonl", true),
        ("rg x ~/.cargo/registry", true),
        ("ls $HOME/x", true),
        // A `cd` into the working directory is left out, as it is from the move.
        ("cd /work/proj && cat src/lib.rs", false),
    ];
    for (command, expected) in cases {
        assert_eq!(outside(&bash(command)), expected, "{command}");
    }
}

/// A structured call is judged by the paths in its `path`, `file_path`, `cwd` and
/// `paths` arguments, whatever the tool.
#[test]
fn a_structured_call_is_judged_by_its_path_arguments() {
    let call = |name: &str, input: Value| ToolCall::new(name, "c1", input);
    let cases = [
        (call("read", json!({ "paths": ["src/lib.rs"] })), false),
        (
            call("read", json!({ "paths": ["src/lib.rs", "/etc/hosts"] })),
            true,
        ),
        (call("edit", json!({ "file_path": "../docs/a.md" })), false),
        (call("list", json!({ "path": ".efference" })), true),
        (call("search", json!({ "paths": ["src/**/*.rs"] })), false),
        (
            call("read", json!({ "paths": [["../other/x", 1, 10]] })),
            true,
        ),
        (call("read", json!({ "paths": ["~/notes.md"] })), true),
        (
            call("write", json!({ "path": "/tmp/scratch/out.txt" })),
            false,
        ),
        (
            call(
                "agents__create",
                json!({ "cwd": "/elsewhere", "text": ["go"] }),
            ),
            true,
        ),
        (
            call("fs__fetch", json!({ "url": "https://example.com" })),
            false,
        ),
    ];
    for (call, expected) in cases {
        assert_eq!(outside(&call), expected, "{call:?}");
    }
}

/// With no directories at hand nothing resolves, so only a path written from the home
/// directory is outside.
#[test]
fn without_directories_only_the_home_directory_is_outside() {
    let read = |path: &str| ToolCall::new("read", "c1", json!({ "paths": [path] }));
    assert!(!state::outside_grants(
        &role(),
        &read("/etc/hosts"),
        None,
        None
    ));
    assert!(state::outside_grants(&role(), &read("~/x"), None, None));
    assert!(!state::outside_grants(
        &role(),
        &bash("cat /etc/hosts"),
        None,
        None
    ));
}
