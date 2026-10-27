//! The grant each gate offers, decided at the rung that held the call
//! (docs/permissions.md).
use super::cascade::{RecordingFrontend, approve_call, bound_leaf, parent_with};
use super::*;

/// Judge `call` under `role` with a spotter that gates, and return the session and the
/// grant the gate offered (`None` inside when the gate offered none). Panics if no gate
/// was reached.
async fn gate_on(
    role: &str,
    call: &ToolCall,
    review: Option<ReviewLevel>,
) -> (Arc<Session>, Option<policy::GateGrant>) {
    let shared = parent_with(crate::config::Config::from_toml(role).unwrap());
    if let Some(level) = review {
        shared.set_review_level(level);
    }
    let mut frontend = RecordingFrontend::answering(Approval::Abort);
    let spotter = crate::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "outside the mission".into(),
    };
    super::step::judge(
        &mut frontend,
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        call,
        None,
    )
    .await
    .unwrap();
    let [grant] = <[_; 1]>::try_from(frontend.grants).expect("one gate was reached");
    (shared, grant)
}

/// A read outside the grants, held by the spotter, offers a read-only grant for its
/// directory; committing that grant through the `config_edit` decision clears the call.
#[tokio::test]
async fn a_spotter_held_read_offers_its_directory_and_the_grant_clears_it() {
    let call = ToolCall::new(
        "read",
        "toolu_x",
        serde_json::json!({ "paths": ["/elsewhere/codex/README.md"] }),
    );
    let (shared, grant) = gate_on("[tools]\nefference = { enabled = true }\n\"efference.read\" = { granted = true }\n[paths]\n\".\" = true\n", &call, None).await;
    let grant = grant.expect("the gate offers a grant");
    assert_eq!(grant.label, "always allow reading /elsewhere/codex");
    let approval = commit_config_edit(&shared, 1, &call, grant.change, grant.label).unwrap();
    assert_eq!(approval, Approval::Approve);
}

/// A `[commands]` gate on `git push origin main` offers the prefix that clears it, under
/// the spotter and with the human in the spotter's place alike; committing it clears.
#[tokio::test]
async fn a_commands_gate_offers_the_prefix_and_the_grant_clears_it() {
    let call = bash_call("git push origin main");
    for review in [None, Some(ReviewLevel::HumanOnly)] {
        let (shared, grant) = gate_on("[commands]\n\"git push\" = \"gate\"\n", &call, review).await;
        let grant = grant.expect("the gate offers a grant");
        assert_eq!(
            grant.change,
            ConfigChange::Command {
                command: "git push".into()
            },
            "{review:?}"
        );
        let approval = commit_config_edit(&shared, 1, &call, grant.change, grant.label).unwrap();
        assert_eq!(approval, Approval::Approve);
    }
}

/// A structured tool held for its name alone offers the tool.
#[tokio::test]
async fn a_structured_tool_hold_offers_the_tool() {
    let call = ToolCall::new(
        "kill_process",
        "toolu_x",
        serde_json::json!({ "id": "p#1" }),
    );
    let (_, grant) = gate_on("", &call, None).await;
    assert_eq!(
        grant.map(|g| g.change),
        Some(ConfigChange::Tool {
            name: "kill_process".into()
        })
    );
}

/// A command run in an ungranted directory is held for two reasons and offers nothing;
/// so does a `[commands]` deny, which is the operator's answer already given.
#[tokio::test]
async fn a_command_in_an_ungranted_path_and_a_deny_offer_nothing() {
    let (_, grant) = gate_on(
        "[commands]\nrg = true\n",
        &bash_call("cd /elsewhere/codex && rg -n foo"),
        None,
    )
    .await;
    assert_eq!(grant, None);
    let (_, grant) = gate_on(
        "[commands]\n\"git push\" = \"deny\"\n",
        &bash_call("git push origin main"),
        None,
    )
    .await;
    assert_eq!(grant, None);
}

/// A bound gate — an allowing `agents__decide` whose leaf is held — offers nothing.
#[tokio::test]
async fn a_bound_gate_offers_nothing() {
    let shared = parent_with(crate::config::Config::default());
    let mut frontend = RecordingFrontend::answering(Approval::Abort);
    let spotter = crate::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "outside the mission".into(),
    };
    super::step::judge(
        &mut frontend,
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        &approve_call(),
        Some(&bound_leaf("npm install")),
    )
    .await
    .unwrap();
    assert_eq!(frontend.grants, [None]);
}
