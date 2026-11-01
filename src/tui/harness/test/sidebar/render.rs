//! What the session drawer displays: processes and stats, folding control
//! characters, retention, the cold-cache-streak warning, session identity, hosted
//! roles, the drawer-only slice, policy with a tentative proposal, opening on
//! demand, and the background process panel.

use super::*;
use crate::api::Usage;
use crate::config::CommandStance;
use crate::config::{Perms, ToolSettings};
use crate::inference::{InferenceIdentity, TriageIdentity};
use crate::policy::ReviewLevel;
use crate::protocol::state::{
    ContextWindow, Cost, CurrentOperations, KeepaliveSkip, KeepaliveSkipReason, LoopPhase,
    OpenCompaction, PendingProposal, RoleView, SessionIdentity, UsageView, VerbAnswer, Verbs,
};
use crate::record::log::{ConfigChange, Provider, Timestamp};
use crate::record::project::Retention;
use crate::tui::harness::{cold, process, session_identity};
use crate::user_config::ModelSelection;

/// The session drawer (docs/tui.md): PROCESSES and STATS render in the panel over
/// the transcript's right side, with a `│` divider. The process panel above the
/// composer shows the same process whether or not the drawer is open.
#[test]
fn sidebar_shows_processes_and_stats() {
    let mut play = Screenplay::new(110, 20);
    play.user("Watch the build.");
    play.assistant("Started it — I'll keep working.", vec![]);
    play.processes(vec![process(1, "cargo watch -x test")]);
    play.process_output(1, "test result: ok. 12 passed");
    play.process_times(1, 252, 8);
    play.topic(TopicUpdate::Usage(Some(UsageView {
        cumulative: Usage {
            input_tokens: 12400,
            output_tokens: 3100,
            cache_read_input_tokens: 8900,
            cache_creation_input_tokens: 5200,
        },
        // The boot-fetched window and the last request's occupancy
        // (docs/transport.md): 14.1k of 200k → the `ctx 7% · 14k/200k` line.
        context: Some(ContextWindow {
            limit: Some(200_000),
            used: Some(14_106),
        }),
        // Billed session cost from the provider's accounting → `cost $0.0421`.
        cost: Some(Cost {
            usd: 0.0421,
            unpriced: 0,
        }),
        // Triage retention (docs/triage.md): 13.2k of 24.1k lines kept →
        // the `kept 55% · 13k/24k lines` line. No gate failed, so nothing withheld.
        retention: Retention {
            kept_lines: 13_276,
            shown_lines: 24_114,
            ..Retention::default()
        },
        // The cache is being read (`cache_read_input_tokens` above), so no streak.
        cold_streak: 0,
        ..UsageView::default()
    })));
    play.since_last_request(8);
    play.open_drawer();
    play.snapshot("sidebar_processes_and_stats");
}

/// A process's command line is whatever the model wrote — a multi-line `python -c`
/// script, say. Newlines go to the sidebar as the replacement glyph rather than into
/// a cell, where ratatui's `cell_width` would assert and kill the TUI thread (and
/// keep killing it on every restart while the process lives in the daemon).
#[test]
fn sidebar_folds_control_characters_in_a_process_command() {
    let mut play = Screenplay::new(110, 20);
    play.user("Start a producer.");
    play.assistant("Started.", vec![]);
    play.processes(vec![process(
        5,
        "python3 -u -c 'import time\ncount = 0\nwhile True:\n\tcount += 1'",
    )]);
    play.process_output(5, "heartbeat count=1\x1b[0m\r");
    let rendered = play.render();
    assert!(rendered.contains("'import time\u{fffd}cou"), "{rendered}");
}

/// Retention is absent, not 0%, before the first result is triaged — and a withheld
/// result is reported as its own condition rather than folded into the ratio
/// (docs/triage.md).
#[test]
fn sidebar_retention_before_any_triage_and_after_a_failed_triage() {
    let stats = |kept, shown, withheld| {
        TopicUpdate::Usage(Some(UsageView {
            cumulative: Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Usage::default()
            },
            retention: Retention {
                kept_lines: kept,
                shown_lines: shown,
                withheld,
                ..Retention::default()
            },
            ..UsageView::default()
        }))
    };

    let mut play = Screenplay::new(110, 16);
    play.user("go");
    play.topic(stats(0, 0, 0));
    let before = play.sidebar_grid();
    assert!(
        !before.contains("kept"),
        "nothing triaged yet — an unknown ratio must not render as 0%:\n{before}"
    );

    play.topic(stats(30, 120, 2));
    let after = play.sidebar_grid();
    assert!(after.contains("25% · 30/120 lines"), "{after}");
    assert!(
        after
            .lines()
            .any(|l| l.contains("withheld") && l.contains(" 2 ")),
        "a failed gate is its own line, not a lower ratio:\n{after}"
    );
}

/// The cold-cache warning beside the context meter (docs/log-and-projection.md):
/// silent below the threshold, stated once a run of unexplained cold main-loop reads
/// crosses it, and gone as soon as a warm read resets the daemon's count. A moved
/// prefix is otherwise invisible from the client — every turn just costs full price.
#[test]
fn sidebar_warns_on_a_cold_cache_streak() {
    let stats = |cold_streak| {
        TopicUpdate::Usage(Some(UsageView {
            cumulative: Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Usage::default()
            },
            context: Some(ContextWindow {
                limit: Some(200_000),
                used: Some(14_106),
            }),
            cold_streak,
            ..UsageView::default()
        }))
    };

    let mut play = Screenplay::new(110, 16);
    play.user("go");
    play.topic(stats(crate::protocol::state::COLD_STREAK_WARN - 1));
    let below = play.sidebar_grid();
    assert!(
        !below.contains("cache misses"),
        "a run of one or two is normal operation, not a warning:\n{below}"
    );

    play.topic(stats(7));
    let warned = play.sidebar_grid();
    assert!(
        warned.contains("⚠ cache misses ×7"),
        "the streak is stated beside the ctx meter:\n{warned}"
    );
    assert!(
        warned.contains("7% · 14k/200k"),
        "beside, not instead of:\n{warned}"
    );

    play.topic(stats(0));
    assert!(
        !play.sidebar_grid().contains("cache misses"),
        "a warm read clears it"
    );
}

/// The keepalive's stance beside the cache readout (docs/transport.md): stated only
/// while the loop skips its pings or the upstream rejected them, parsed from the
/// `usage.keepalive_skipped` field, and gone once pings resume.
#[test]
fn sidebar_states_the_keepalive_stance() {
    let usage = |skipped: Option<KeepaliveSkipReason>| {
        TopicUpdate::Usage(Some(UsageView {
            cumulative: Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Usage::default()
            },
            context: Some(ContextWindow {
                limit: Some(200_000),
                used: Some(14_106),
            }),
            keepalive_skipped: skipped.map(|why| KeepaliveSkip {
                at: Timestamp(1),
                why,
            }),
            ..UsageView::default()
        }))
    };
    let mut play = Screenplay::new(110, 16);
    play.user("go");

    play.topic(usage(None));
    assert!(!play.sidebar_grid().contains("keepalive"));

    play.topic(usage(Some(KeepaliveSkipReason::Discard {
        output_tokens: 10_000,
        completion_usd: Some(0.005),
        prefix_tokens: 5_000,
        premium_usd: None,
    })));
    let paused = play.sidebar_grid();
    assert!(
        paused.contains("keepalive paused: 10k out ($0.0050)"),
        "{paused}"
    );

    play.topic(usage(Some(KeepaliveSkipReason::Rejected {
        detail: "400".into(),
    })));
    assert!(play.sidebar_grid().contains("keepalive off: ping rejected"));

    play.topic(usage(Some(KeepaliveSkipReason::Exhausted {
        detail: "usage limit reached".into(),
        resets_at: None,
    })));
    assert!(
        play.sidebar_grid()
            .contains("keepalive paused: usage limit")
    );

    play.topic(usage(None));
    assert!(!play.sidebar_grid().contains("keepalive"));
}

/// The SESSION section names the model and the working directory, from the
/// `session_identity` topic (docs/control-api.md). Nothing in the transcript says
/// which model is answering, and cheap and flagship models mix across sessions.
#[test]
fn sidebar_names_the_model_and_working_directory() {
    let mut play = Screenplay::new(110, 16);
    play.user("go");
    let before = play.sidebar_grid();
    assert!(
        !before.contains("Session"),
        "no section before the topic arrives:\n{before}"
    );

    play.topic(TopicUpdate::SessionIdentity(Some(session_identity(
        "amber-heron",
        "anthropic/claude-opus-4",
    ))));
    let after = play.sidebar_grid();
    assert!(after.contains("Session"), "{after}");
    assert!(after.contains("amber-heron"), "{after}");
    assert!(after.contains("anthropic/claude-opus-4"), "{after}");
    assert!(after.contains("~/code/efference"), "{after}");

    // A title, once written, leads the section (docs/session-title.md); the
    // topic is re-delivered whole, so the view is replaced rather than patched.
    play.topic(TopicUpdate::SessionIdentity(Some(SessionIdentity {
        title: Some("Session names: slug ids and titles".into()),
        ..session_identity("amber-heron", "anthropic/claude-opus-4")
    })));
    let titled = play.sidebar_grid();
    let title_row = titled
        .lines()
        .position(|l| l.contains("Session names: slug ids and titles"))
        .expect("the title renders");
    let id_row = titled
        .lines()
        .position(|l| l.contains("amber-heron"))
        .expect("the id renders");
    assert!(title_row < id_row, "{titled}");
}

/// A hosted session names each role by connection: the main line becomes the
/// qualified selection, and the spotter and triage rows follow it.
#[test]
fn sidebar_names_hosted_roles_by_connection() {
    let selection = |connection: &str, model: &str| ModelSelection {
        connection: connection.into(),
        provider: Provider::Anthropic,
        model: model.into(),
    };
    let mut play = Screenplay::new(110, 16);
    play.user("go");
    play.topic(TopicUpdate::SessionIdentity(Some(SessionIdentity {
        inference: Some(InferenceIdentity {
            main: selection("work", "claude-opus-4"),
            spotter: selection("personal", "claude-haiku-4-5"),
            triage: TriageIdentity {
                shared: true,
                selection: selection("work", "claude-opus-4"),
            },
        }),
        ..session_identity("amber-heron", "claude-opus-4")
    })));
    let grid = play.sidebar_grid();
    assert!(grid.contains("work/claude-opus-4"), "{grid}");
    for (label, value) in [
        ("spotter", "personal/claude-haiku-4-5"),
        ("triage", "shared"),
    ] {
        assert!(
            grid.lines().any(|l| l.contains(label) && l.contains(value)),
            "{label} names {value}:\n{grid}"
        );
    }
}

/// The sidebar-only screenshot slice (`Alt-S`) frames just the sidebar columns:
/// narrow, and excluding the transcript — so a wide-terminal capture isn't verbose.
#[test]
fn sidebar_slice_excludes_the_transcript() {
    let mut play = Screenplay::new(110, 16);
    play.user("A long transcript line that must NOT appear in a sidebar-only slice.");
    play.assistant("ok", vec![]);
    play.topic(TopicUpdate::Role(Some(RoleView {
        scope: Some("this repo".into()),
        commands: [("cargo".to_string(), CommandStance::Clear)].into(),
        ..RoleView::default()
    })));
    play.topic(TopicUpdate::ReviewLevel(Some(ReviewLevel::HumanOnly)));

    let slice = play.sidebar_grid();
    assert!(
        slice.contains("Policy"),
        "the slice shows the sidebar:\n{slice}"
    );
    assert!(
        slice.contains("human_only"),
        "the sidebar surfaces the review-level dial:\n{slice}"
    );
    assert!(
        !slice.contains("must NOT appear"),
        "the slice excludes the transcript:\n{slice}"
    );
    let widest = slice.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    let drawer = crate::tui::render_frame::DRAWER_WIDTH as usize;
    assert!(
        widest < drawer + 10,
        "the slice is narrow (≈drawer width), not the full 110 cols; got {widest}"
    );
}

/// The POLICY sidebar section: committed grants plus a *proposed* one rendered in
/// its sorted position with a trailing `⊕` (docs/permissions.md) — `git log` among
/// the commands, `/refs` among the paths. Committing would drop the `⊕` in place.
#[test]
fn sidebar_policy_with_a_tentative_proposal() {
    let mut play = Screenplay::new(110, 28);
    play.user("Work on the repo.");
    play.assistant("On it.", vec![]);
    play.topic(TopicUpdate::Role(Some(RoleView {
        scope: Some("work only in this repo".into()),
        commands: [
            ("cargo".to_string(), CommandStance::Clear),
            ("git diff".to_string(), CommandStance::Clear),
        ]
        .into(),
        paths: [(
            ".".to_string(),
            Perms {
                read: true,
                write: true,
            },
        )]
        .into(),
        tools: [
            ("read".to_string(), ToolSettings::default()),
            ("write".to_string(), ToolSettings::default()),
        ]
        .into(),
        ..RoleView::default()
    })));
    play.topic(TopicUpdate::Proposals(vec![
        PendingProposal {
            seq: 7,
            change: ConfigChange::Command {
                command: "git log".into(),
            },
        },
        PendingProposal {
            seq: 9,
            change: ConfigChange::Path {
                path: "/refs".into(),
                read: true,
                write: false,
            },
        },
    ]));

    play.open_drawer();
    let bar = play.find("⊕").expect("the proposed grant shows a ⊕ marker");
    let drawer = play
        .state
        .viewport
        .sidebar_area
        .expect("the drawer is open");
    assert!(bar.0 > drawer.x, "the proposal marker is in the drawer");
    play.snapshot("sidebar_policy_with_proposal");
}

/// The drawer is closed until asked for, at any width: the transcript takes the
/// full width, and the header's **session** button (or Tab from command mode)
/// opens it over the transcript's right side.
#[test]
fn the_drawer_opens_on_demand() {
    for width in [80, 110] {
        let mut play = Screenplay::new(width, 10);
        play.user("hi");
        assert!(
            play.find("Processes").is_none(),
            "closed by default at {width} cols"
        );
        assert!(play.find("session").is_some(), "the header offers it");
        play.open_drawer();
        assert!(
            play.find("Processes").is_some(),
            "open on demand at {width} cols"
        );
    }
}

/// The live background-process panel (docs/process-monitor.md): a `Processes` event renders a
/// persistent strip above the composer — one row per running process with handle,
/// command, running/idle timing, and the last output line. Fed by the `processes`
/// topic, it is chrome, not transcript (no reproject).
#[test]
fn background_process_panel() {
    let mut play = Screenplay::new(80, 16);
    play.user("watch the build");
    play.assistant("Started it — I'll keep working.", vec![]);
    play.processes(vec![
        process(1, "cargo watch -x test"),
        process(2, "npm run dev"),
    ]);
    play.process_output(
        1,
        "Compiling efference\nrunning 12 tests\ntest result: ok. 12 passed",
    );
    play.process_output(2, "ready - started server on 0.0.0.0:3000");
    play.process_times(1, 252, 8);
    play.process_times(2, 9, 9);
    play.snapshot("background_process_panel");
}

/// The `cold` topic in the header (backlog: cold-session.md): stated while the cache is
/// believed expired, with the compaction key while the `verbs` topic says the compact
/// route takes it, `compacting…` while a compaction is open in `current_operations`,
/// and gone when the topic clears. The park is the `cold_parked` loop phase.
#[test]
fn header_states_an_expired_cache() {
    let compacting = |open: bool| {
        TopicUpdate::CurrentOperations(open.then(|| CurrentOperations {
            requests: Vec::new(),
            tools: Vec::new(),
            compaction: Some(OpenCompaction::Waiting {
                warm: false,
                cut: 0,
                since: 0,
            }),
        }))
    };
    let verbs = |compact: VerbAnswer| {
        TopicUpdate::Verbs(Some(Verbs {
            turn: VerbAnswer::accepted(),
            continue_verb: VerbAnswer::accepted(),
            compact,
        }))
    };
    let mut play = Screenplay::new(110, 16);
    play.user("go");
    assert!(!play.render().contains("cache expired"));

    play.topic(TopicUpdate::Cold(Some(cold(None))));
    assert!(play.render().contains("cache expired · C-o compacts"));
    play.topic(verbs(VerbAnswer::refused(
        "an approval gate is pending — resolve it first",
    )));
    assert!(
        !play.render().contains("C-o compacts"),
        "no key is offered for a compaction the route would refuse"
    );
    // A compaction the route takes only with `warm: true` is not one `C-o` can send.
    play.topic(verbs(VerbAnswer::compacts(true)));
    assert!(
        !play.render().contains("C-o compacts"),
        "no key is offered for a compaction that needs `warm`"
    );
    play.topic(verbs(VerbAnswer::compacts(false)));
    assert!(play.render().contains("C-o compacts"));

    play.phase(LoopPhase::ColdParked);
    assert!(
        play.render()
            .contains("paused: cache expired · ⏎ to continue · C-o compacts")
    );
    play.phase(LoopPhase::Idle);

    play.topic(compacting(true));
    assert!(play.render().contains("cache expired · compacting…"));

    play.topic(TopicUpdate::Cold(None));
    assert!(!play.render().contains("cache expired"));
    assert!(
        play.render().contains("compacting…"),
        "a warm compaction shows too"
    );

    // A compaction at a park ends the belief, not the park: a send resumes it unasked.
    play.topic(compacting(false));
    play.phase(LoopPhase::ColdParked);
    assert!(play.render().contains("paused: summarized · ⏎ to continue"));
    play.phase(LoopPhase::Idle);
    assert!(!play.render().contains("compacting…"));
}
