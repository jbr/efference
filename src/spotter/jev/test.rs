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
        outside_grants: 0.02,
        exposes_secrets: 0.01,
        watch_for_hit: None,
        prefix_safe: None,
    }
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
    let cases: [(&str, Axes, Stance); 4] = [
        (
            "exposed secret",
            Axes {
                exposes_secrets: 0.9,
                ..clean()
            },
            Stance::Gate,
        ),
        (
            "outside the grants",
            Axes {
                outside_grants: 0.9,
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
            outside_grants: 0.5,
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
    let five = json!({
        "in_scope": { "type": "noul", "noul": 0.9 },
        "reversible": { "type": "noul", "noul": 0.9 },
        "sanctioned": { "type": "noul", "noul": 0.1 },
        "outside_grants": { "type": "noul", "noul": 0.1 },
        "exposes_secrets": { "type": "noul", "noul": 0.1 },
    });
    // Not asked: the axis is absent and the read succeeds.
    assert_eq!(
        read_axes(&answered(five.clone()), false, false)
            .expect("the unconditional set is complete")
            .watch_for_hit,
        None
    );
    // Asked and unanswered: no verdict.
    assert!(read_axes(&answered(five.clone()), true, false).is_none());
    // One unconditional axis missing: no verdict.
    let mut four = five;
    four.as_object_mut().expect("object").remove("reversible");
    assert!(read_axes(&answered(four), false, false).is_none());
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
