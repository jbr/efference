//! The topics a client deserializes (docs/control-api.md): each type the host serializes
//! reads back to the same bytes, with its optional fields present and absent. The host's
//! own bytes are pinned in `daemon/test/topic_bytes.rs`; this is the other direction.

use std::collections::BTreeMap;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::*;
use crate::config::policy::GateGrant;
use crate::config::user_config::ModelSelection;
use crate::config::{Perms, ToolSettings};
use crate::inference::api::{
    CacheMissReason, PlanLimit, PlanSnapshot, PlanWindow, RetrySurface, UpstreamFailure,
    UpstreamRetry, Usage,
};
use crate::inference::{InferenceIdentity, TriageIdentity};
use crate::protocol::agents::BoundAction;
use crate::protocol::status::Escalated;
use crate::record::log::{ConfigChange, Provider, Timestamp};
use crate::record::project::Retention;

const AT: u64 = 1_754_300_000_000;

/// `sample` as the channel carries it, read back and carried again: the same value.
fn round_trips<T: Serialize + DeserializeOwned>(sample: &T) {
    let sent = serde_json::to_value(sample).unwrap();
    let received: T = serde_json::from_value(sent.clone())
        .unwrap_or_else(|error| panic!("{error}\nreading back {sent}"));
    assert_eq!(serde_json::to_value(&received).unwrap(), sent);
}

fn bound_action() -> BoundAction {
    BoundAction {
        name: "bash".into(),
        input: json!({"command": "cargo test"}),
        intent: Some("run the suite".into()),
        cwd: "/work".into(),
        project: "/work".into(),
        session: "child".into(),
        chain: vec!["child".into(), "grandchild".into()],
    }
}

#[test]
fn a_gate_reads_back_with_and_without_its_optional_parts() {
    let bare = Gate {
        call_id: "toolu_1".into(),
        name: "bash".into(),
        input: json!({"command": "ls"}),
        reason: None,
        acting_on: None,
        grant: None,
        escalations: vec![],
    };
    round_trips(&bare);
    let full = Gate {
        reason: Some("deletes outside the tree".into()),
        acting_on: Some(bound_action()),
        grant: Some(GateGrant {
            change: ConfigChange::Command {
                command: "cargo".into(),
            },
            label: "always allow cargo".into(),
        }),
        escalations: vec![
            Escalated {
                source: Some("session[child]".into()),
                to: Some("parent".into()),
                note: Some("not mine to decide".into()),
            },
            Escalated {
                source: None,
                to: None,
                note: None,
            },
        ],
        ..bare
    };
    round_trips(&full);
}

#[test]
fn proposals_are_read_back_by_the_lowercase_tags_the_channel_carries() {
    let wire = json!([
        {"seq": 2, "change": {"path": {"path": "/tmp/scratch", "read": true, "write": false}}},
        {"seq": 3, "change": {"command": {"command": "git log"}}},
        {"seq": 4, "change": {"tool": {"name": "agents__message"}}},
        {"seq": 5, "change": {"scope": {"text": "the parser only"}}},
    ]);
    let proposals: Vec<PendingProposal> = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(proposals.len(), 4);
    assert_eq!(
        proposals[0].change,
        ConfigChange::Path {
            path: "/tmp/scratch".into(),
            read: true,
            write: false
        }
    );
    assert_eq!(serde_json::to_value(&proposals).unwrap(), wire);
}

#[test]
fn a_role_reads_back_with_every_section_filled() {
    round_trips(&RoleView::default());
    round_trips(&RoleView {
        source: Some("/work/efference.toml".into()),
        scope: Some("Fix the parser.".into()),
        watch_for: vec!["deleting tests".into()],
        commands: BTreeMap::from([
            ("git".to_string(), crate::config::CommandStance::Clear),
            ("git push".to_string(), crate::config::CommandStance::Deny),
        ]),
        tools: BTreeMap::from([
            (
                "efference".to_string(),
                ToolSettings {
                    enabled: Some(true),
                    granted: None,
                },
            ),
            ("ferritin".to_string(), ToolSettings::default()),
        ]),
        paths: BTreeMap::from([(
            "src".to_string(),
            Perms {
                read: true,
                write: false,
            },
        )]),
    });
}

fn plan() -> PlanSnapshot {
    PlanSnapshot {
        plan_type: Some("plus".into()),
        active: Some("codex".into()),
        limits: BTreeMap::from([(
            "codex".to_string(),
            PlanLimit {
                name: None,
                primary: Some(PlanWindow {
                    used_percent: 12.5,
                    window_minutes: Some(300),
                    resets_at: Some(AT + 60_000),
                }),
                secondary: None,
            },
        )]),
        observed_at: AT,
    }
}

#[test]
fn usage_reads_back_empty_and_with_every_field_set() {
    round_trips(&UsageView::default());
    let stats = Usage {
        input_tokens: 10,
        output_tokens: 20,
        cache_creation_input_tokens: 30,
        cache_read_input_tokens: 40,
    };
    round_trips(&UsageView {
        last: Some(stats.clone()),
        cumulative: stats,
        last_request_at: Some(Timestamp(AT)),
        context: Some(ContextWindow {
            limit: Some(200_000),
            used: Some(42),
        }),
        cost: Some(Cost {
            usd: 0.0421,
            unpriced: 1,
        }),
        cold_streak: 2,
        triage_cold_streak: Some(0),
        last_cache_miss: Some(LastCacheMiss {
            reason: CacheMissReason::ToolsChanged,
            cache_missed_input_tokens: Some(12_280),
            at: Timestamp(AT + 1),
        }),
        keepalive_skipped: Some(KeepaliveSkip {
            at: Timestamp(AT + 2),
            why: KeepaliveSkipReason::Discard {
                output_tokens: 10_000,
                completion_usd: Some(0.005),
                prefix_tokens: 5_000,
                premium_usd: None,
            },
        }),
        triage_keepalive_skipped: Some(KeepaliveSkip {
            at: Timestamp(AT + 3),
            why: KeepaliveSkipReason::Exhausted {
                detail: "usage limit reached".into(),
                resets_at: Some(AT + 3_600_000),
            },
        }),
        plan: Some(plan()),
        triage_plan: Some(plan()),
        retention: Retention {
            kept_lines: 13,
            shown_lines: 24,
            truncated_lines: 1,
            withheld: 2,
        },
    });
}

#[test]
fn the_other_keepalive_stance_reads_back() {
    round_trips(&KeepaliveSkip {
        at: Timestamp(AT),
        why: KeepaliveSkipReason::Rejected {
            detail: "400 bad request".into(),
        },
    });
}

#[test]
fn a_process_reads_back() {
    round_trips(&ProcessState {
        id: 3,
        command: "cargo watch -x test".into(),
        call_id: Some("toolu_3".into()),
        pid: Some(4242),
        spawned_at: Timestamp(AT),
        background: true,
        muted: false,
    });
    round_trips(&ProcessState {
        id: 4,
        command: "sleep 600".into(),
        call_id: None,
        pid: None,
        spawned_at: Timestamp(AT),
        background: false,
        muted: true,
    });
}

#[test]
fn transport_diagnostics_read_back() {
    round_trips(&UpstreamFailure::new(Some(429), "slow down"));
    round_trips(&UpstreamFailure::new(None, "stream reset"));
    round_trips(&UpstreamRetry {
        surface: RetrySurface::Triage,
        attempt: 2,
        budget: 12,
        next_attempt_at: AT,
        message: "attempt 2 of 12 failed upstream, retrying".into(),
        detail: "connection reset".into(),
    });
    round_trips(&Stall {
        reason: StallReason::RetriesExhausted,
        attempts: Some(12),
        at: AT,
        message: "the trajectory stopped: 12 attempts in a row failed upstream".into(),
        detail: "connection reset".into(),
    });
    round_trips(&Stall {
        reason: StallReason::TriageUnavailable,
        attempts: None,
        at: AT,
        message: "the triage could not reach its model".into(),
        detail: String::new(),
    });
}

#[test]
fn a_session_identity_reads_back_standalone_and_hosted() {
    round_trips(&SessionIdentity {
        id: "amber-heron".into(),
        title: None,
        model: "claude-opus-4".into(),
        cwd: "/work".into(),
        inference: None,
        successor: None,
    });
    let selection = |connection: &str, model: &str| ModelSelection {
        connection: connection.into(),
        provider: Provider::Anthropic,
        model: model.into(),
    };
    round_trips(&SessionIdentity {
        id: "amber-heron".into(),
        title: Some("Session names".into()),
        model: "claude-opus-4".into(),
        cwd: "/work".into(),
        inference: Some(InferenceIdentity {
            main: selection("work", "claude-opus-4"),
            spotter: selection("personal", "claude-haiku-4-5"),
            triage: TriageIdentity {
                shared: false,
                selection: selection("personal", "claude-haiku-4-5"),
            },
        }),
        successor: Some("amber-heron-2".into()),
    });
}
