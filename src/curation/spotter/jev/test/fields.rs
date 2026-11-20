//! The state a decisions request carries (`state.rs`).

use super::*;
use crate::config::policy::Directories;
use crate::record::names::tools::BASH;
use crate::record::utterances::{Speaker, Utterance, Utterances};

fn at(cwd: &str) -> Directories {
    Directories::at(std::path::Path::new(cwd))
}

fn bash(command: &str) -> ToolCall {
    ToolCall::new(BASH, "c1", json!({ "command": command }))
}

/// The state with no circumstances beyond the call.
fn plain(role: &Config, call: &ToolCall, dirs: Option<&Directories>) -> State {
    state::state(role, call, &Circumstances::default(), dirs, None)
}

/// The fields go on the wire in the order the battery was fitted on: the config, then
/// the guidance and the conversation, then the move, with the probe after it.
#[test]
fn the_fields_go_in_their_fitted_order() {
    let said = Utterances {
        id: "t".into(),
        said: vec![
            Utterance::new(Speaker::Operator, "tidy the build"),
            Utterance::new(Speaker::Supervisor("lead".into()), "only the target dir"),
            Utterance::new(Speaker::Agent, "I'll clean it."),
        ],
    };
    let chain = ["child".to_string()];
    let circumstances = Circumstances::default()
        .with_chain(&chain)
        .with_probe(Some("cargo"))
        .with_utterances(Some(&said));
    let state = state::state(
        &ConfigFixture::new().with_scope("work here").build(),
        &bash("cargo clean"),
        &circumstances,
        Some(&at("/repo")),
        Some(std::path::Path::new("/tmp/scratch")),
    );
    assert_eq!(
        state.keys().collect::<Vec<_>>(),
        [
            "mission",
            "spotter_notes",
            "working_directory",
            "path_grants",
            "scratch_directory",
            "worktrees",
            "approval_chain",
            "guidance",
            "conversation",
            "move",
            "promotion_probe",
        ]
    );
    let text = serde_json::to_string(&state).expect("serializes");
    assert!(
        text.starts_with(r#"{"mission":"work here","spotter_notes":[],"#),
        "{text}"
    );
    assert!(text.ends_with(r#""promotion_probe":"cargo"}"#), "{text}");
    assert_eq!(
        state["conversation"],
        json!([
            { "writer": "operator", "text": "tidy the build" },
            { "writer": "supervisor:lead", "text": "only the target dir" },
            { "writer": "agent", "text": "I'll clean it." },
        ])
    );
    assert_eq!(state["guidance"], json!(super::super::super::PERSONA));
}

/// A state with no session log at hand has no conversation, rather than an empty one
/// that reads as "nothing was said"; a session where nothing was said has an empty one.
#[test]
fn an_unknown_conversation_is_left_out_and_an_empty_one_is_empty() {
    let role = ConfigFixture::new().build();
    let unknown = plain(&role, &bash("ls"), None);
    assert!(unknown.get("conversation").is_none());
    assert!(unknown.get("guidance").is_some());

    let silent = Utterances {
        id: "t".into(),
        said: Vec::new(),
    };
    let known = state::state(
        &role,
        &bash("ls"),
        &Circumstances::default().with_utterances(Some(&silent)),
        None,
        None,
    );
    assert_eq!(known["conversation"], json!([]));
}

#[test]
fn the_state_withholds_the_harness_injected_intent() {
    let call = ToolCall::new(
        "fs__read",
        "c1",
        json!({ "path": "src/lib.rs", crate::record::names::tools::INTENT: "just looking, honest" }),
    );
    let state = plain(
        &ConfigFixture::new().with_scope("work here").build(),
        &call,
        Some(&at("/repo")),
    );
    let input = &state["move"]["input"];
    assert_eq!(input["path"], json!("src/lib.rs"));
    assert!(
        input.get(crate::record::names::tools::INTENT).is_none(),
        "the judged party's own plea reached the spotter: {state:?}"
    );
}

/// The state includes a tool's safe-direction hints as `claims` and omits the
/// unsafe-direction ones, as the prose brief does.
#[test]
fn the_state_carries_only_the_tools_guarantees() {
    let state_for = |read_only, destructive, open_world| {
        let definition = mcplease::types::Tool {
            name: "play__click".into(),
            title: None,
            description: Some("Click an element.".into()),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
            annotations: Some(mcplease::types::ToolAnnotations {
                title: None,
                read_only_hint: read_only,
                destructive_hint: destructive,
                idempotent_hint: None,
                open_world_hint: open_world,
            }),
            icons: None,
            meta: None,
        };
        let brief = crate::tools::ToolBrief {
            server: Some("play"),
            definition: &definition,
        };
        state::state(
            &ConfigFixture::new().with_scope("work here").build(),
            &ToolCall::new("play__click", "c1", json!({ "element": "OK" })),
            &Circumstances::default().with_brief(Some(brief)),
            None,
            None,
        )["move"]
            .clone()
    };

    let unsafe_only = state_for(Some(false), Some(true), Some(true));
    assert!(unsafe_only.get("tool_declarations").is_none());

    let mixed = state_for(Some(true), None, Some(true));
    assert_eq!(
        mixed["tool_declarations"]["claims"],
        json!(["read-only: it does not modify its environment"])
    );
}

/// Grants render resolved-absolute, so containment against the working directory
/// is string-visible; the denies are included, because a denied subtree under an
/// allowed parent is exactly what a bash move can stray into.
#[test]
fn grants_render_resolved_with_the_denies_kept() {
    let state = plain(
        &ConfigFixture::new()
            .with_scope("work here")
            .with_path(".", true, true)
            .with_path(".efference", false, false)
            .build(),
        &bash("ls"),
        Some(&at("/repo")),
    );
    let grants = state["path_grants"].as_array().expect("a list");
    let find = |path: &str| {
        grants
            .iter()
            .find(|g| g["path"] == json!(path))
            .unwrap_or_else(|| panic!("no grant for {path} in {grants:?}"))
    };
    assert_eq!(find("/repo")["access"], json!("read, write"));
    assert_eq!(find("/repo")["granted_as"], json!("."));
    assert_eq!(find("/repo/.efference")["access"], json!("denied"));
}

/// The optional fields appear only when there is something to say, so an ordinary
/// move carries no empty field inviting an answer about it.
#[test]
fn the_probe_and_the_chain_appear_only_when_present() {
    let call = bash("cargo test");
    let config = ConfigFixture::new().with_scope("work here").build();
    let ordinary = plain(&config, &call, None);
    assert!(ordinary.get("promotion_probe").is_none());
    assert!(ordinary.get("approval_chain").is_none());
    assert!(ordinary.get("worktrees").is_none());
    assert_eq!(ordinary["move"]["command"], json!("cargo test"));

    let chain = ["parent".to_string(), "child".to_string()];
    let bound = state::state(
        &config,
        &call,
        &Circumstances::default()
            .with_chain(&chain)
            .with_probe(Some("cargo test")),
        None,
        None,
    );
    assert_eq!(bound["promotion_probe"], json!("cargo test"));
    assert_eq!(bound["approval_chain"]["held_by"], json!("child"));
}

/// A leading `cd` into the working directory goes nowhere, so jev's move is the rest
/// — the same resolution the prose rung is spared (docs/permissions.md).
#[test]
fn a_cd_into_the_working_directory_is_left_out_of_the_move() {
    let config = ConfigFixture::new().with_scope("work here").build();
    let state = plain(
        &config,
        &bash("cd /work/proj && cargo test"),
        Some(&at("/work/proj")),
    );
    assert_eq!(state["move"]["command"], json!("cargo test"));
}

/// A shell move carries the paths it names, resolved and matched to the grant that
/// decides each, and a word only the running shell can resolve is listed as such
/// rather than dropped (docs/permissions.md).
#[test]
fn a_shell_move_carries_its_resolved_paths() {
    let config = ConfigFixture::new()
        .with_path(".", true, true)
        .with_path(".efference", false, false)
        .build();
    let dirs = at("/work/proj");
    let state = plain(
        &config,
        &bash("cat >> .git/info/exclude << 'EOF'\n.efference/\nEOF\nrg x ~/.cargo/src"),
        Some(&dirs),
    );
    assert_eq!(
        state["move"]["paths"],
        json!([
            {
                "as_written": ".git/info/exclude",
                "resolved": "/work/proj/.git/info/exclude",
                "under_grant": { "path": "/work/proj", "access": "read, write" },
                "written": true,
            },
            { "as_written": "~/.cargo/src", "resolved": null, "expands_at_run_time": true },
        ])
    );
    assert!(state["move"].get("paths_complete").is_none());

    let state = plain(&config, &bash("cargo test"), Some(&dirs));
    assert!(state["move"].get("paths").is_none());
}

/// The state carries the trimmed, non-empty notes under `spotter_notes`.
#[test]
fn the_state_carries_the_spotter_notes() {
    let call = ToolCall::new("fs__read", "c1", json!({ "path": "src/lib.rs" }));
    let config = ConfigFixture::new()
        .with_spotter_note("  deleting tests  ")
        .with_spotter_note("https://dev.example:7878 is this project's own server")
        .build();
    let state = plain(&config, &call, None);
    assert_eq!(
        state["spotter_notes"],
        json!([
            "deleting tests",
            "https://dev.example:7878 is this project's own server"
        ])
    );
}
