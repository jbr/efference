use super::*;
use crate::log::Stance;
use questions::{Deciding, combine};
use serde_json::json;

/// Parse a config from TOML for a test that needs one as an *input* — the
/// operator's own vocabulary, checked by the real parser.
fn role(toml: &str) -> Config {
    Config::from_toml(toml).expect("valid config")
}

/// All axes clean: in scope, reversible, nothing else tripped.
fn clean() -> Axes {
    Axes {
        in_scope: 0.95,
        reversible: 0.95,
        sanctioned: 0.05,
        path_outside: 0.02,
        path_denied: 0.01,
        exposes_secrets: 0.01,
        watch_for_hit: None,
        prefix_safe: None,
    }
}

/// The shipped bands are a tuning result, not a guess (the sweep of 2026-09-18,
/// backlog/decision-spotter-tuning.md). Pinned because the thing they encode is
/// that the axes do **not** share a scale: a single pair cannot sit above
/// `path_outside`'s 0.19 floor and below `exposes_secrets`'s 0.03 one, and the
/// pair that tried gated 42% of a corpus that is 78% approve. A future edit that
/// collapses them back toward each other should have to break this first.
#[test]
fn the_axes_do_not_share_a_scale() {
    let t = Thresholds::default();
    // The measured background: a flat "no" reads ~0.19 on `path_outside` and
    // ~0.03 on `exposes_secrets`. One `low` would have to sit above the first
    // and below the second, which is not a number.
    assert!(
        t.path_outside.low > 0.19 && t.exposes_secrets.low < 0.19,
        "the two floors ({} and {}) no longer straddle path_outside's measured \
         background — a single band could cover them, which the sweep says it cannot",
        t.path_outside.low,
        t.exposes_secrets.low
    );
    // And the in_scope band lives entirely above every other axis's floor: its
    // "no" is a low probability of being in scope, not a high one of trouble.
    assert!(t.in_scope.high > t.path_outside.high);
}

fn stance(axes: &Axes, probe: bool) -> Stance {
    combine(axes, &Thresholds::default(), probe).0
}

/// The two ways to reach this rung (docs/permissions.md): a provider that serves
/// nothing else, or the family's id on a provider that serves both.
#[test]
fn either_the_provider_or_the_id_selects_the_decision_rung() {
    use crate::api::Provider;
    // On the first-party connection the id is bare, so the provider is the
    // whole answer — and every model there is a decision model.
    assert!(is_decision_spotter(Provider::TypeSafe, "jev-latest"));
    assert!(is_decision_spotter(
        Provider::TypeSafe,
        "whatever-comes-next"
    ));
    // On a provider that serves turns too, the id decides.
    assert!(is_decision_spotter(
        Provider::OpenRouter,
        "typesafe/jev-1.13"
    ));
    assert!(!is_decision_spotter(
        Provider::OpenRouter,
        "anthropic/claude-haiku-4-5"
    ));
    assert!(!is_decision_spotter(
        Provider::Anthropic,
        "claude-haiku-4-5"
    ));
}

#[test]
fn the_model_id_is_the_whole_switch() {
    assert!(is_decision_model("typesafe/jev-1.13"));
    assert!(is_decision_model("typesafe/jev"));
    // The alias form OpenRouter serves the family under.
    assert!(is_decision_model("~typesafe/jev-latest"));
    // A different author's similarly-named model is not this rung.
    assert!(!is_decision_model("someone/jev-1.13"));
    assert!(!is_decision_model("anthropic/claude-haiku-4-5"));
    // Nor is a different TypeSafe model, when there is one.
    assert!(!is_decision_model("typesafe/something-else"));
}

#[test]
fn a_clean_move_clears() {
    assert_eq!(stance(&clean(), false), Stance::Clear);
}

/// Each disqualifying axis, in the order the fold reads them. `high`/`low` are
/// the default band; a value inside it is uncertain, not clean.
#[test]
fn a_disqualifying_axis_gates() {
    let cases: [(&str, Axes, Stance); 5] = [
        (
            "exposed secret",
            Axes {
                exposes_secrets: 0.9,
                ..clean()
            },
            Stance::Gate,
        ),
        (
            "names a path outside the grants",
            Axes {
                path_outside: 0.9,
                ..clean()
            },
            Stance::Gate,
        ),
        (
            "names a path under a denied carve-out",
            Axes {
                path_denied: 0.9,
                ..clean()
            },
            Stance::Gate,
        ),
        (
            "out of scope",
            Axes {
                in_scope: 0.1,
                ..clean()
            },
            Stance::Gate,
        ),
        (
            "unsanctioned one-way move",
            Axes {
                reversible: 0.05,
                sanctioned: 0.05,
                ..clean()
            },
            Stance::Gate,
        ),
    ];
    for (what, axes, expected) in cases {
        assert_eq!(stance(&axes, false), expected, "{what}");
    }
}

/// The error weighting for this rung is asymmetric (backlog/spotter-eval.md): a
/// wrong clear costs more than a wrong gate, so the uncertain middle resolves
/// toward the human on every axis rather than toward auto-approval.
#[test]
fn the_uncertain_middle_never_clears() {
    let middling = [
        Axes {
            exposes_secrets: 0.5,
            ..clean()
        },
        Axes {
            path_outside: 0.5,
            ..clean()
        },
        Axes {
            path_denied: 0.5,
            ..clean()
        },
        Axes {
            in_scope: 0.5,
            ..clean()
        },
        Axes {
            reversible: 0.5,
            sanctioned: 0.1,
            ..clean()
        },
        Axes {
            watch_for_hit: Some(0.5),
            ..clean()
        },
    ];
    for axes in middling {
        assert!(
            matches!(stance(&axes, false), Stance::Watch | Stance::Gate),
            "an uncertain axis cleared: {axes:?}"
        );
    }
}

/// Scope sanction is what rescues a one-way move — silence is not sanction
/// (docs/permissions.md: the reversibility default).
#[test]
fn a_sanctioned_one_way_move_clears() {
    let axes = Axes {
        reversible: 0.02,
        sanctioned: 0.93,
        ..clean()
    };
    assert_eq!(stance(&axes, false), Stance::Clear);
}

/// A watch-for is the operator naming the thing in advance; a clear hit stops.
#[test]
fn a_watch_for_hit_gates() {
    let axes = Axes {
        watch_for_hit: Some(0.9),
        ..clean()
    };
    assert_eq!(stance(&axes, false), Stance::Gate);
    // Clearly not a hit leaves the move to the other axes.
    let axes = Axes {
        watch_for_hit: Some(0.02),
        ..clean()
    };
    assert_eq!(stance(&axes, false), Stance::Clear);
}

/// The promotion safe-vote rides the stance and takes the strict reading: only an
/// explicit, confident yes promotes, and it can never outrank a non-clear stance.
#[test]
fn only_a_confident_prefix_vote_promotes() {
    let vouched = Axes {
        prefix_safe: Some(0.95),
        ..clean()
    };
    assert_eq!(stance(&vouched, true), Stance::ClearAlways);
    // The same answers with no probe riding along are an ordinary clear.
    assert_eq!(stance(&vouched, false), Stance::Clear);
    // Unsure about the prefix clears the move without vouching for the prefix.
    let unsure = Axes {
        prefix_safe: Some(0.6),
        ..clean()
    };
    assert_eq!(stance(&unsure, true), Stance::Clear);
    // A gated move never promotes, however safe the prefix reads.
    let gated = Axes {
        in_scope: 0.05,
        prefix_safe: Some(0.99),
        ..clean()
    };
    assert_eq!(stance(&gated, true), Stance::Gate);
}

/// The reason is the whole of what the session and the human read, and the model
/// wrote none of it — so it names the deciding axis and carries its figure
/// rather than asserting a conclusion the numbers don't show.
#[test]
fn the_reason_names_the_deciding_axis_with_its_figure() {
    let axes = Axes {
        in_scope: 0.07,
        ..clean()
    };
    let (stance, deciding) = combine(&axes, &Thresholds::default(), false);
    assert_eq!(deciding, Deciding::Scope);
    let reason = questions::reason(stance, deciding, &axes);
    assert!(reason.contains("7%"), "no figure in {reason:?}");
    assert!(reason.contains("scope"), "no axis named in {reason:?}");
}

/// The grants question is two questions (backlog/decision-spotter-tuning.md), and
/// the fold names the more specific one: "under a deliberate carve-out" tells the
/// reader something "outside the grants" does not.
#[test]
fn a_denied_carve_out_is_named_as_itself() {
    let axes = Axes {
        path_denied: 0.9,
        path_outside: 0.02,
        ..clean()
    };
    let (stance, deciding) = combine(&axes, &Thresholds::default(), false);
    assert_eq!(stance, Stance::Gate);
    assert_eq!(deciding, Deciding::DeniedPath);
    let reason = questions::reason(stance, deciding, &axes);
    assert!(reason.contains("denied"), "{reason:?}");

    // And it outranks the broader finding when both trip, since a path can be
    // both outside a grant and under a carve-out.
    let both = Axes {
        path_denied: 0.9,
        path_outside: 0.9,
        ..clean()
    };
    assert_eq!(
        combine(&both, &Thresholds::default(), false).1,
        Deciding::DeniedPath
    );
}

/// Both halves of the split are asked on every call: a corpus case with no
/// denied entry still needs the question posed, because "no denied entry exists"
/// is an answer the model gives rather than one the harness assumes.
#[test]
fn both_halves_of_the_grants_question_are_always_asked() {
    let bare = ask::questions(false, false);
    assert!(bare.contains_key(questions::PATH_OUTSIDE));
    assert!(bare.contains_key(questions::PATH_DENIED));
}

/// The conditional axes are asked only when the state carries the thing they are
/// about: a question about a list that isn't there invites an answer invented
/// from nothing.
#[test]
fn conditional_questions_are_asked_only_when_they_apply() {
    let bare = ask::questions(false, false);
    assert!(!bare.contains_key(questions::WATCH_FOR_HIT));
    assert!(!bare.contains_key(questions::PREFIX_SAFE));
    assert!(bare.contains_key(questions::IN_SCOPE));

    let full = ask::questions(true, true);
    assert!(full.contains_key(questions::WATCH_FOR_HIT));
    assert!(full.contains_key(questions::PREFIX_SAFE));
}

/// An asked-but-unanswered axis is a failure; a never-asked one is `None`. The
/// fold reads `None` as "nothing to weigh", which would be wrong for the first.
#[test]
fn a_missing_answer_for_an_asked_question_fails_rather_than_reading_as_absent() {
    let answered = |answers: serde_json::Value| -> crate::api::Decisions {
        serde_json::from_value(json!({ "answers": answers, "usage": {} })).expect("parses")
    };
    let full = json!({
        "in_scope": { "type": "noul", "noul": 0.9 },
        "reversible": { "type": "noul", "noul": 0.9 },
        "sanctioned": { "type": "noul", "noul": 0.1 },
        "path_outside": { "type": "noul", "noul": 0.1 },
        "path_denied": { "type": "noul", "noul": 0.1 },
        "exposes_secrets": { "type": "noul", "noul": 0.1 },
    });
    // Not asked: the axis is absent and the read succeeds.
    assert_eq!(
        read_axes(&answered(full.clone()), false, false)
            .expect("the unconditional set is complete")
            .watch_for_hit,
        None
    );
    // Asked and unanswered: no verdict.
    assert!(read_axes(&answered(full.clone()), true, false).is_none());
    // One unconditional axis missing: no verdict.
    let mut fewer = full;
    fewer.as_object_mut().expect("object").remove("reversible");
    assert!(read_axes(&answered(fewer), false, false).is_none());
}

#[test]
fn the_state_withholds_the_harness_injected_intent() {
    let call = ToolCall::new(
        "fs__read",
        "c1",
        json!({ "path": "src/lib.rs", crate::tools::INTENT: "just looking, honest" }),
    );
    let state = state::state(
        &role("scope = \"work here\""),
        &call,
        None,
        &[],
        None,
        Some(std::path::Path::new("/repo")),
        None,
    );
    let input = &state["move"]["input"];
    assert_eq!(input["path"], json!("src/lib.rs"));
    assert!(
        input.get(crate::tools::INTENT).is_none(),
        "the judged party's own plea reached the spotter: {state}"
    );
}

/// Grants render resolved-absolute, so containment against the working directory
/// is string-visible; the denies are included, because a denied subtree under an
/// allowed parent is exactly what a bash move can stray into.
#[test]
fn grants_render_resolved_with_the_denies_kept() {
    let state = state::state(
        &role(
            "scope = \"work here\"\n\
             [paths]\n\
             \".\" = true\n\
             \".efference\" = false\n",
        ),
        &ToolCall::new(crate::bash::NAME, "c1", json!({ "command": "ls" })),
        None,
        &[],
        None,
        Some(std::path::Path::new("/repo")),
        None,
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

/// The optional state fields appear only when there is something to say, so an
/// ordinary move carries no empty field inviting an answer about it.
#[test]
fn the_probe_and_the_chain_ride_only_when_present() {
    let call = ToolCall::new(crate::bash::NAME, "c1", json!({ "command": "cargo test" }));
    let config = role("scope = \"work here\"");
    let plain = state::state(&config, &call, None, &[], None, None, None);
    assert!(plain.get("promotion_probe").is_none());
    assert!(plain.get("approval_chain").is_none());
    assert_eq!(plain["move"]["command"], json!("cargo test"));

    let bound = state::state(
        &config,
        &call,
        None,
        &["parent".into(), "child".into()],
        Some("cargo test"),
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
    let call = ToolCall::new(
        crate::bash::NAME,
        "c1",
        json!({ "command": "cd /work/proj && cargo test" }),
    );
    let config = role("scope = \"work here\"");
    let cwd = std::path::Path::new("/work/proj");
    let state = state::state(&config, &call, None, &[], None, Some(cwd), None);
    assert_eq!(state["move"]["command"], json!("cargo test"));
}

/// A shell move carries the paths it names, resolved and matched to the grant that
/// decides each, and a word only the running shell can resolve is listed as such
/// rather than dropped (docs/permissions.md).
#[test]
fn a_shell_move_carries_its_resolved_paths() {
    let config = role(
        r#"
[paths]
"." = true
".efference" = false
"#,
    );
    let cwd = std::path::Path::new("/work/proj");
    let call = ToolCall::new(
        crate::bash::NAME,
        "c1",
        json!({ "command": "cat >> .git/info/exclude << 'EOF'\n.efference/\nEOF\nrg x ~/.cargo/src" }),
    );
    let state = state::state(&config, &call, None, &[], None, Some(cwd), None);
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

    let plain = ToolCall::new(crate::bash::NAME, "c2", json!({ "command": "cargo test" }));
    let state = state::state(&config, &plain, None, &[], None, Some(cwd), None);
    assert!(state["move"].get("paths").is_none());
}
