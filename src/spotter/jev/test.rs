use super::*;
use crate::log::Stance;
use questions::{Reading, decide, level_of};
use serde_json::json;

/// Parse a config from TOML for a test that needs one as an *input* — the
/// operator's own vocabulary, checked by the real parser.
fn role(toml: &str) -> Config {
    Config::from_toml(toml).expect("valid config")
}

/// A sketch distribution putting 0.8 on `level` and the rest spread evenly.
fn sketch_at(level: usize) -> Vec<f64> {
    (0..5)
        .map(|i| if i == level { 0.8 } else { 0.05 })
        .collect()
}

/// Answers for a plainly routine move: sketch level 0, every axis clean.
fn clean() -> Axes {
    Axes {
        in_scope: 0.95,
        path_outside: 0.02,
        path_denied: 0.01,
        exposes_secrets: 0.01,
        sketch: Some(sketch_at(0)),
        ..Axes::default()
    }
}

fn at_level(level: usize) -> Axes {
    Axes {
        sketch: Some(sketch_at(level)),
        ..clean()
    }
}

fn readings(candidates: usize, same: usize, wider: usize) -> Option<Reading> {
    Some(Reading {
        candidates,
        same,
        wider,
    })
}

/// The fold table: level, precedents found, and the stance that results. Level 0
/// and 1 clear alone, level 2 needs one same-action precedent, level 3 needs two,
/// level 4 is never cleared by history.
#[test]
fn the_fold_requires_precedent_by_level() {
    let cases: [(&str, Axes, Option<Reading>, Stance); 12] = [
        ("level 0, no history", at_level(0), None, Stance::Clear),
        ("level 1, no history", at_level(1), None, Stance::Clear),
        (
            "level 2, none found",
            at_level(2),
            readings(1, 0, 0),
            Stance::Gate,
        ),
        ("level 2, relation failed", at_level(2), None, Stance::Gate),
        (
            "level 2, one found",
            at_level(2),
            readings(1, 1, 0),
            Stance::Clear,
        ),
        (
            "level 3, one of two",
            at_level(3),
            readings(3, 1, 0),
            Stance::Gate,
        ),
        (
            "level 3, two found",
            at_level(3),
            readings(3, 2, 0),
            Stance::Clear,
        ),
        (
            "level 4, many found",
            at_level(4),
            readings(10, 10, 0),
            Stance::Gate,
        ),
        (
            "level 2, only wider",
            at_level(2),
            readings(2, 0, 2),
            Stance::Gate,
        ),
        (
            "outside the grants raises level 0 to 2",
            Axes {
                path_outside: 0.62,
                ..clean()
            },
            readings(1, 0, 0),
            Stance::Gate,
        ),
        (
            "outside the grants, precedent found",
            Axes {
                path_outside: 0.62,
                ..clean()
            },
            readings(1, 1, 0),
            Stance::Clear,
        ),
        (
            "secrets gate even with precedents",
            Axes {
                exposes_secrets: 0.5,
                ..clean()
            },
            readings(5, 5, 0),
            Stance::Gate,
        ),
    ];
    for (what, axes, reading, expected) in cases {
        assert_eq!(decide(&axes, reading, false).stance, expected, "{what}");
    }
}

/// The level is the sketch's most probable level, raised (never lowered) by a
/// tripped axis; the thresholds are strict where the replay's are.
#[test]
fn bumps_raise_the_level_to_the_axis_floor() {
    let level = |axes: Axes| level_of(&axes).0;
    assert_eq!(level(at_level(1)), 1);
    assert_eq!(level(at_level(4)), 4);
    assert_eq!(
        level(Axes {
            path_denied: 0.26,
            ..clean()
        }),
        3
    );
    assert_eq!(
        level(Axes {
            path_denied: 0.25,
            ..clean()
        }),
        0
    );
    assert_eq!(
        level(Axes {
            exposes_secrets: 0.16,
            ..clean()
        }),
        3
    );
    assert_eq!(
        level(Axes {
            watch_for_hit: Some(0.3),
            ..clean()
        }),
        3
    );
    assert_eq!(
        level(Axes {
            watch_for_hit: Some(0.2),
            ..clean()
        }),
        0
    );
    assert_eq!(
        level(Axes {
            path_outside: 0.36,
            ..clean()
        }),
        2
    );
    assert_eq!(
        level(Axes {
            path_outside: 0.35,
            ..clean()
        }),
        0
    );
    // Scope bumps when it reads as false, at the edge inclusive.
    assert_eq!(
        level(Axes {
            in_scope: 0.30,
            ..clean()
        }),
        2
    );
    assert_eq!(
        level(Axes {
            in_scope: 0.31,
            ..clean()
        }),
        0
    );
    // A bump never lowers a level the sketch already put higher.
    assert_eq!(
        level(Axes {
            path_outside: 0.9,
            ..at_level(3)
        }),
        3
    );
}

/// `wider` is evidence the move goes further than what was approved, never a
/// precedent: the count that clears is `same` alone.
#[test]
fn wider_does_not_count_as_precedent() {
    let decision = decide(&at_level(2), readings(3, 1, 2), false);
    assert_eq!(decision.stance, Stance::Clear);
    let decision = decide(&at_level(2), readings(3, 0, 3), false);
    assert_eq!(decision.stance, Stance::Gate);
    assert_eq!(decision.required, Some(1));
}

/// The promotion safe-vote rides a clear and takes the strict reading: only a
/// confident yes with a probe riding promotes, and a hold never promotes.
#[test]
fn only_a_confident_prefix_vote_on_a_clear_promotes() {
    let vouched = Axes {
        prefix_safe: Some(0.95),
        ..clean()
    };
    assert_eq!(decide(&vouched, None, true).stance, Stance::ClearAlways);
    // The same answers with no probe riding along are an ordinary clear.
    assert_eq!(decide(&vouched, None, false).stance, Stance::Clear);
    let unsure = Axes {
        prefix_safe: Some(0.79),
        ..clean()
    };
    assert_eq!(decide(&unsure, None, true).stance, Stance::Clear);
    let edge = Axes {
        prefix_safe: Some(0.8),
        ..clean()
    };
    assert_eq!(decide(&edge, None, true).stance, Stance::ClearAlways);
    // A held move never promotes, however safe the prefix reads.
    let held = Axes {
        prefix_safe: Some(0.99),
        ..at_level(3)
    };
    assert_eq!(decide(&held, readings(1, 0, 0), true).stance, Stance::Gate);
}

/// This rung's dial is how many precedents a level needs (backlog trust dial), so
/// it has no middle stance to return.
#[test]
fn the_fold_never_returns_watch() {
    let probabilities = [0.0, 0.1, 0.3, 0.5, 0.7, 0.99];
    for level in 0..5 {
        for p in probabilities {
            for same in 0..3 {
                let axes = Axes {
                    in_scope: p,
                    path_outside: p,
                    path_denied: p,
                    exposes_secrets: p,
                    watch_for_hit: Some(p),
                    prefix_safe: Some(p),
                    ..at_level(level)
                };
                for probe in [false, true] {
                    let stance = decide(&axes, readings(3, same, 0), probe).stance;
                    assert_ne!(stance, Stance::Watch, "{axes:?}");
                }
            }
        }
    }
}

#[test]
fn the_reason_reads_as_designed() {
    let reason = |axes: &Axes, reading| decide(axes, reading, false).reason;
    assert_eq!(reason(&clean(), None), "routine (level 0).");
    assert_eq!(
        reason(&clean(), readings(2, 2, 0)),
        "routine (level 0); matches 2 earlier calls the operator approved."
    );
    assert_eq!(
        reason(
            &Axes {
                path_outside: 0.62,
                ..clean()
            },
            readings(1, 0, 0)
        ),
        "level 2 (outside the grants, 62%): needs 1 approved call like it, found none."
    );
    assert_eq!(
        reason(&at_level(3), readings(3, 1, 0)),
        "level 3 (sketch, 80%): needs 2 approved calls like it, found 1."
    );
    assert_eq!(
        reason(&at_level(4), readings(3, 3, 0)),
        "level 4 (sketch, 80%): no number of approved calls clears this level."
    );
    assert_eq!(
        reason(
            &Axes {
                path_outside: 0.62,
                ..clean()
            },
            readings(2, 0, 2)
        ),
        "level 2 (outside the grants, 62%): needs 1 approved call like it, found none; \
         goes further than 2 calls the operator approved."
    );
    assert_eq!(
        reason(
            &Axes {
                exposes_secrets: 0.91,
                ..clean()
            },
            readings(4, 4, 0)
        ),
        "this looks like it would expose secret material (91% confidence) — no earlier \
         approval clears that, so it stops here."
    );
    let vouched = Axes {
        prefix_safe: Some(0.9),
        ..clean()
    };
    assert_eq!(
        decide(&vouched, None, true).reason,
        "routine (level 0); the offered prefix reads safe for any argument (90%)."
    );
}

/// The record carries the sketch and the precedent reading, and omits what was
/// not asked.
#[test]
fn the_record_carries_the_sketch_and_the_precedent() {
    let axes = Axes {
        precedent: Some(Precedent {
            candidates: 3,
            same: 1,
            wider: 1,
            required: Some(2),
            compared: Vec::new(),
        }),
        ..clean()
    };
    assert_eq!(
        serde_json::to_value(&axes).expect("serializes"),
        json!({
            "in_scope": 0.95,
            "path_outside": 0.02,
            "path_denied": 0.01,
            "exposes_secrets": 0.01,
            "sketch": [0.8, 0.05, 0.05, 0.05, 0.05],
            "precedent": { "candidates": 3, "same": 1, "wider": 1, "required": 2 },
        })
    );
    let bare = serde_json::to_value(clean()).expect("serializes");
    assert!(bare.get("precedent").is_none());
    assert!(bare.get("watch_for_hit").is_none());
}

/// A `Spot` recorded before the sketch fold carries `reversible` and `sanctioned`
/// and neither new field; it must still parse.
#[test]
fn an_entry_recorded_with_the_old_axes_still_parses() {
    let old = json!({
        "in_scope": 0.93,
        "reversible": 0.88,
        "sanctioned": 0.04,
        "path_outside": 0.19,
        "path_denied": 0.02,
        "exposes_secrets": 0.03,
        "prefix_safe": 0.9,
    });
    let axes: Axes = serde_json::from_value(old).expect("old shape parses");
    assert_eq!(axes.in_scope, 0.93);
    assert_eq!(axes.prefix_safe, Some(0.9));
    assert_eq!(axes.watch_for_hit, None);
    assert_eq!(axes.sketch, None);
    assert_eq!(axes.precedent, None);
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

/// The cold request asks the sketch and the shipping axes, and not `reversible`
/// or `sanctioned`, which the fold does not read.
#[test]
fn the_cold_questions_are_the_sketch_and_the_shipping_axes() {
    let bare = ask::questions(false, false);
    let mut keys: Vec<&str> = bare.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "exposes_secrets",
            "in_scope",
            "path_denied",
            "path_outside",
            "sketch"
        ]
    );
    assert!(!bare.contains_key("reversible"));
    assert!(!bare.contains_key("sanctioned"));
    let sketch = serde_json::to_value(&bare[questions::SKETCH]).expect("serializes");
    assert_eq!(sketch["type"], json!("score"));
    assert_eq!(sketch["criteria"].as_array().map(Vec::len), Some(5));
    assert!(
        sketch["criteria"][0]
            .as_str()
            .is_some_and(|l| l.starts_with("Routine:"))
    );
    assert!(
        sketch["criteria"][4]
            .as_str()
            .is_some_and(|l| l.starts_with("Severe:"))
    );
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

fn answered(answers: serde_json::Value) -> crate::api::Decisions {
    serde_json::from_value(json!({ "answers": answers, "usage": {} })).expect("parses")
}

fn cold_answers() -> serde_json::Value {
    json!({
        "sketch": {
            "type": "score",
            "score": 0.1,
            "probabilities": { "0": 0.6, "1": 0.2, "2": 0.1, "3": 0.07, "4": 0.03 }
        },
        "in_scope": { "type": "noul", "noul": 0.9 },
        "path_outside": { "type": "noul", "noul": 0.1 },
        "path_denied": { "type": "noul", "noul": 0.1 },
        "exposes_secrets": { "type": "noul", "noul": 0.1 },
    })
}

/// An asked-but-unanswered axis is a failure; a never-asked one is `None`. The
/// fold reads `None` as "nothing to weigh", which would be wrong for the first.
#[test]
fn a_missing_answer_for_an_asked_question_fails_rather_than_reading_as_absent() {
    let full = cold_answers();
    // Not asked: the axis is absent and the read succeeds.
    let axes = read_axes(&answered(full.clone()), false, false)
        .expect("the unconditional set is complete");
    assert_eq!(axes.watch_for_hit, None);
    assert_eq!(axes.sketch, Some(vec![0.6, 0.2, 0.1, 0.07, 0.03]));
    // Asked and unanswered: no verdict.
    assert!(read_axes(&answered(full.clone()), true, false).is_none());
    // One unconditional axis missing: no verdict.
    let mut fewer = full.clone();
    fewer.as_object_mut().expect("object").remove("path_denied");
    assert!(read_axes(&answered(fewer), false, false).is_none());
    // The sketch missing a level, or missing altogether: no verdict.
    let mut short = full.clone();
    short["sketch"]["probabilities"]
        .as_object_mut()
        .expect("object")
        .remove("4");
    assert!(read_axes(&answered(short), false, false).is_none());
    let mut none = full;
    none.as_object_mut().expect("object").remove("sketch");
    assert!(read_axes(&answered(none), false, false).is_none());
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
        Some(&crate::policy::Directories::at(std::path::Path::new(
            "/repo",
        ))),
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
        Some(&crate::policy::Directories::at(std::path::Path::new(
            "/repo",
        ))),
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
    let state = state::state(
        &config,
        &call,
        None,
        &[],
        None,
        Some(&crate::policy::Directories::at(cwd)),
        None,
    );
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
    let state = state::state(
        &config,
        &call,
        None,
        &[],
        None,
        Some(&crate::policy::Directories::at(cwd)),
        None,
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

    let plain = ToolCall::new(crate::bash::NAME, "c2", json!({ "command": "cargo test" }));
    let state = state::state(
        &config,
        &plain,
        None,
        &[],
        None,
        Some(&crate::policy::Directories::at(cwd)),
        None,
    );
    assert!(state["move"].get("paths").is_none());
}
