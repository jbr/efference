use super::*;
use crate::config::{CommandStance, Perms, ToolSettings};
use crate::protocol::state::{PendingProposal, RoleView};
use crate::tui::topics::{TopicUpdate, Topics};

fn labels(entries: &[PolicyEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.label.as_str()).collect()
}

/// Nothing is drawn until the `role` topic has arrived.
#[test]
fn there_is_no_section_before_the_role_arrives() {
    let topics = Topics {
        proposals: vec![PendingProposal {
            seq: 3,
            change: ConfigChange::Command {
                command: "git log".into(),
            },
        }],
        ..Topics::default()
    };
    assert!(PolicyView::of(&topics).is_none());
}

/// The drawer shows each `[tools]` entry with the fields it sets, so a carve-out reads as
/// one; a proposed grant is labelled as the entry it becomes (docs/tui.md).
#[test]
fn tool_entries_show_their_fields_and_proposals_their_committed_label() {
    let topics = Topics {
        role: Some(RoleView {
            tools: [
                (
                    "efference".to_string(),
                    ToolSettings {
                        enabled: Some(true),
                        granted: Some(true),
                    },
                ),
                (
                    "efference.bash".to_string(),
                    ToolSettings {
                        enabled: Some(false),
                        granted: None,
                    },
                ),
                (
                    "agents".to_string(),
                    ToolSettings {
                        enabled: Some(true),
                        granted: None,
                    },
                ),
            ]
            .into(),
            ..RoleView::default()
        }),
        proposals: vec![PendingProposal {
            seq: 7,
            change: ConfigChange::Tool {
                name: "agents__message".into(),
            },
        }],
        ..Topics::default()
    };
    let view = PolicyView::of(&topics).expect("a role is a section");
    assert_eq!(
        labels(&view.tools),
        [
            "agents  enabled",
            "agents.message  granted",
            "efference  enabled · granted",
            "efference.bash  off",
        ]
    );
    let proposed = &view.tools[1];
    assert_eq!((proposed.proposed, proposed.seq), (true, Some(7)));
}

/// A command stance other than `clear` is tagged, as a path carries its perms: an untagged
/// carve-out would read as a grant.
#[test]
fn commands_and_paths_carry_their_stance_and_perms() {
    let topics = Topics {
        role: Some(RoleView {
            commands: [
                ("git".to_string(), CommandStance::Clear),
                ("git push".to_string(), CommandStance::Deny),
            ]
            .into(),
            paths: [
                (
                    "src".to_string(),
                    Perms {
                        read: true,
                        write: false,
                    },
                ),
                (
                    "/tmp/scratch".to_string(),
                    Perms {
                        read: true,
                        write: true,
                    },
                ),
                ("/dev".to_string(), Perms::default()),
            ]
            .into(),
            ..RoleView::default()
        }),
        ..Topics::default()
    };
    let view = PolicyView::of(&topics).unwrap();
    assert_eq!(labels(&view.commands), ["git", "git push  deny"]);
    assert_eq!(
        labels(&view.paths),
        ["/dev  -", "/tmp/scratch  rw", "src  r"]
    );
}

/// The proposals topic as the host sends it — change variants spelled in lowercase — puts
/// each proposal in its section, in its sorted position, flagged and addressable by `seq`.
/// The scope proposal sits under the committed scope instead of among the lists.
#[test]
fn proposals_as_the_host_sends_them_join_their_sections() {
    let mut topics = Topics {
        role: Some(RoleView {
            scope: Some("the parser".into()),
            commands: [("cargo".to_string(), CommandStance::Clear)].into(),
            ..RoleView::default()
        }),
        ..Topics::default()
    };
    let frame = serde_json::json!([
        {"seq": 4, "change": {"command": {"command": "git log"}}},
        {"seq": 5, "change": {"path": {"path": "/refs", "read": true, "write": false}}},
        {"seq": 6, "change": {"scope": {"text": "the parser and the lexer"}}},
    ]);
    let update = TopicUpdate::decode("proposals", frame).unwrap().unwrap();
    topics.replace(update);

    let view = PolicyView::of(&topics).unwrap();
    assert_eq!(labels(&view.commands), ["cargo", "git log"]);
    assert!(!view.commands[0].proposed);
    assert_eq!(
        (view.commands[1].proposed, view.commands[1].seq),
        (true, Some(4))
    );
    assert_eq!(labels(&view.paths), ["/refs  r"]);
    assert_eq!(
        view.scope_proposal,
        Some((6, "the parser and the lexer".into()))
    );
    assert_eq!(view.proposals().collect::<Vec<_>>(), [6, 4, 5]);
}

/// The position in force is reported only where it differs from the dial.
#[test]
fn the_position_in_force_is_stated_only_when_it_differs() {
    use crate::config::policy::ReviewLevel::{HumanOnly, SpotterHuman, Supervisor};
    let mut topics = Topics {
        role: Some(RoleView::default()),
        review_level: Some(HumanOnly),
        resolved_review_level: Some(HumanOnly),
        ..Topics::default()
    };
    let view = PolicyView::of(&topics).unwrap();
    assert_eq!(
        (view.review_level, view.resolved_review_level),
        (Some(HumanOnly), None)
    );

    topics.review_level = Some(Supervisor);
    topics.resolved_review_level = Some(SpotterHuman);
    let view = PolicyView::of(&topics).unwrap();
    assert_eq!(view.resolved_review_level, Some(SpotterHuman));
}
