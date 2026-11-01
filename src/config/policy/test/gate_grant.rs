//! The grant a gate offers (docs/permissions.md).

use super::*;

fn dirs() -> Directories {
    Directories::at(Path::new(CWD))
}

/// The grant a gate on `call` offers under `role`, with `ledger` as the session's
/// probe evidence. Every offer is also checked to clear the call once applied.
fn offered(role: &Config, ledger: &ProbeLedger, call: &ToolCall) -> Option<GateGrant> {
    let grant = gate_grant(&dirs(), role, ledger, call)?;
    let mut amended = role.clone();
    apply_config_change(&mut amended, &grant.change);
    assert_eq!(
        classify(&dirs(), &amended, call),
        Verdict::Clear,
        "the offered grant clears the call it was offered on"
    );
    Some(grant)
}

fn read_call(paths: &[&str]) -> ToolCall {
    ToolCall::new("read", "toolu_x", serde_json::json!({ "paths": paths }))
}

/// A read outside the grants offers a read-only grant for the directory it reached,
/// labelled with what it allows.
#[test]
fn a_read_outside_the_grants_offers_reading_its_directory() {
    let grant = offered(
        &fs_role(),
        &ProbeLedger::default(),
        &read_call(&["/elsewhere/codex/README.md"]),
    )
    .expect("one reason, one grant");
    assert_eq!(
        grant.change,
        ConfigChange::Path {
            path: "/elsewhere/codex".into(),
            read: true,
            write: false,
        }
    );
    assert_eq!(grant.label, "always allow reading /elsewhere/codex");
}

/// A write outside the grants offers write, which a `[paths]` entry spells as both; a
/// directory already keyed is replaced under its own spelling rather than keyed twice.
#[test]
fn a_write_offers_write_and_reuses_an_existing_key() {
    let call = ToolCall::new(
        "write",
        "toolu_x",
        serde_json::json!({ "path": "/refs/notes.md", "content": "x" }),
    );
    let grant = offered(&fs_role(), &ProbeLedger::default(), &call).unwrap();
    assert_eq!(
        grant.change,
        ConfigChange::Path {
            path: "/refs".into(),
            read: true,
            write: true,
        }
    );
    assert_eq!(grant.label, "always allow reading and writing /refs");
}

/// A batched read offers the shallowest directory it reached when that contains the
/// rest, and nothing when the targets diverge.
#[test]
fn a_batched_read_offers_one_directory_or_none() {
    let ledger = ProbeLedger::default();
    let nested = offered(
        &fs_role(),
        &ledger,
        &read_call(&["/elsewhere/a/x.rs", "/elsewhere/a/b/y.rs"]),
    )
    .unwrap();
    assert!(
        matches!(&nested.change, ConfigChange::Path { path, .. } if path == "/elsewhere/a"),
        "{nested:?}"
    );
    assert_eq!(
        offered(
            &fs_role(),
            &ledger,
            &read_call(&["/elsewhere/a/x.rs", "/other/y.rs"])
        ),
        None
    );
}

/// A target under a no-access grant offers nothing: the deny is an answer already given.
#[test]
fn a_denied_path_offers_nothing() {
    let role = ConfigFixture::new()
        .with_tool("read")
        .with_path(".", true, true)
        .with_path(".efference", false, false)
        .build();
    assert_eq!(
        offered(
            &role,
            &ProbeLedger::default(),
            &read_call(&[".efference/other/log.jsonl"])
        ),
        None
    );
}

/// A structured call missing only its tool grant offers the tool.
#[test]
fn a_structured_tool_hold_offers_the_tool() {
    let call = ToolCall::new(
        "ferritin__get",
        "toolu_x",
        serde_json::json!({ "path": "serde::Deserialize" }),
    );
    let grant = offered(&fs_role(), &ProbeLedger::default(), &call).unwrap();
    assert_eq!(
        grant.change,
        ConfigChange::Tool {
            name: "ferritin__get".into()
        }
    );
    assert_eq!(grant.label, "always allow the tool ferritin.get");
}

/// A call missing both its tool and a path grant is held for two reasons, and no one
/// change clears it.
#[test]
fn a_tool_and_a_path_together_offer_nothing() {
    let role = ConfigFixture::new().with_path(".", true, true).build();
    assert_eq!(
        offered(
            &role,
            &ProbeLedger::default(),
            &read_call(&["/elsewhere/x.rs"])
        ),
        None
    );
}

/// A `[commands]` gate on `git push origin main` offers the key that held it, which a
/// clear grant replaces; the shorter `git` would leave the more specific key governing.
#[test]
fn a_commands_gate_offers_the_prefix_that_clears() {
    let role = ConfigFixture::new()
        .with_command("git push", CommandStance::Gate)
        .build();
    let grant = offered(
        &role,
        &ProbeLedger::default(),
        &bash_call("git push origin main"),
    )
    .unwrap();
    assert_eq!(
        grant.change,
        ConfigChange::Command {
            command: "git push".into()
        }
    );
    assert_eq!(
        grant.label,
        "always allow running git push with any arguments"
    );
}

/// With no evidence a bash hold offers the program name; a prefix a probe voted not safe
/// is descended past to the next, as the ledger's own descent does.
#[test]
fn a_bash_hold_offers_the_shortest_prefix_the_evidence_supports() {
    let role = Config::default();
    let call = bash_call("git push origin main");
    let command = |ledger: &ProbeLedger| match offered(&role, ledger, &call).unwrap().change {
        ConfigChange::Command { command } => command,
        other => panic!("{other:?}"),
    };
    let mut ledger = ProbeLedger::default();
    assert_eq!(command(&ledger), "git");
    ledger.vote("git", false);
    assert_eq!(command(&ledger), "git push");
    ledger.vote("git push", false);
    ledger.vote("git push origin", true);
    assert_eq!(command(&ledger), "git push origin");
}

/// A `[commands]` deny offers nothing, and neither does a command stage 1 rejects —
/// one run in an ungranted directory, or a pipeline — since no prefix would clear it.
#[test]
fn a_deny_and_an_unclearable_command_offer_nothing() {
    let ledger = ProbeLedger::default();
    let deny = ConfigFixture::new()
        .with_command("git push", CommandStance::Deny)
        .build();
    assert_eq!(
        offered(&deny, &ledger, &bash_call("git push origin main")),
        None
    );
    let role = ConfigFixture::new().with_path(".", true, true).build();
    for command in ["cd /elsewhere/codex && rg -n foo", "rg -n foo | head -20"] {
        assert_eq!(
            offered(&role, &ledger, &bash_call(command)),
            None,
            "{command}"
        );
    }
    // The same command after a `cd` into a granted directory is judged bare.
    let grant = offered(&role, &ledger, &bash_call("cd /work && rg -n foo")).unwrap();
    assert_eq!(
        grant.change,
        ConfigChange::Command {
            command: "rg".into()
        }
    );
}
