use super::*;
use crate::curation::triage::{ASK_BANNER, TRIAGE_INSTRUCTION};
use crate::record::project::WireTurn;

fn attempt(inbox: Vec<(Seq, Disposition)>, background: Vec<(String, BgVerdict)>) -> IdleAttempt {
    IdleAttempt {
        origin: Origin::Model,
        inbox,
        background,
        retries: Vec::new(),
        thinking: Vec::new(),
        usage: Vec::new(),
        elapsed: Duration::from_secs(2),
    }
}

fn bare_case(expects_wake: bool) -> IdleCase {
    IdleCase {
        name: "bare",
        expects_wake,
        rationale: "for scoring tests only",
        prefix: Vec::new(),
        inbox: Vec::new(),
        background: Vec::new(),
    }
}

#[test]
fn the_corpus_is_balanced_and_labelled() {
    let cases = synthetic_cases();
    assert!(
        cases.len() >= 4,
        "a comparison needs enough cases to be worth a run"
    );
    assert!(cases.iter().any(|c| c.expects_wake), "no wake case");
    assert!(cases.iter().any(|c| !c.expects_wake), "no no-wake case");
    for case in &cases {
        assert!(!case.name.is_empty());
        assert!(!case.rationale.is_empty(), "{} has no rationale", case.name);
        assert!(
            !case.prefix.is_empty(),
            "{} has no session context",
            case.name
        );
        assert!(
            !case.inbox.is_empty() || !case.background.is_empty(),
            "{} shows no event at all",
            case.name
        );
        for notice in &case.inbox {
            assert!(
                !notice.wake,
                "{}: must-wake pins before the triage runs",
                case.name
            );
        }
    }
}

#[test]
fn the_ask_carries_the_event_and_ends_with_the_instruction() {
    let case = synthetic_cases().remove(0);
    let ask = case.ask();
    assert!(ask.contains(ASK_BANNER), "{ask}");
    assert!(ask.contains(TRIAGE_INSTRUCTION), "{ask}");
    for notice in &case.inbox {
        assert!(ask.contains(&notice.body), "{ask}");
    }

    let request = case.request();
    assert_eq!(request.len(), case.prefix.len() + 2);
    assert!(matches!(request[request.len() - 2], WireTurn::Breakpoint));
    assert!(matches!(request.last(), Some(WireTurn::User(_))));
}

#[test]
fn the_background_ask_shows_the_tail() {
    let case = synthetic_cases()
        .into_iter()
        .find(|c| !c.background.is_empty())
        .expect("a case exercising the running-tail arm");
    let ask = case.ask();
    let bg = &case.background[0];
    assert!(ask.contains(&bg.handle), "{ask}");
    assert!(ask.contains(&bg.command), "{ask}");
    assert!(ask.contains(bg.tail.lines().next().unwrap()), "{ask}");
}

#[test]
fn a_wake_needs_an_inbox_pin() {
    assert!(attempt(vec![(2, Disposition::Pin)], Vec::new()).wakes());
    assert!(!attempt(vec![(2, Disposition::Dismiss)], Vec::new()).wakes());
    // Snapshotting a running tail puts it on the wire but starts no trajectory.
    assert!(!attempt(Vec::new(), vec![("p#5".to_string(), BgVerdict::Pin)]).wakes());
}

#[test]
fn a_failed_parse_is_not_scored_as_a_dismissal() {
    let mut fallen = attempt(Vec::new(), Vec::new());
    fallen.origin = Origin::Fallback;
    let scored = tally(&bare_case(false), vec![fallen]);
    assert_eq!(scored.no_verdicts, 1);
    assert_eq!(scored.matches_label, 0);
    assert_eq!(scored.false_dismisses, 0);
    assert_eq!(scored.misses(), 1);
}

#[test]
fn misses_are_scored_against_the_label() {
    let wake_case = bare_case(true);
    let scored = tally(
        &wake_case,
        vec![
            attempt(vec![(2, Disposition::Pin)], Vec::new()),
            attempt(vec![(2, Disposition::Dismiss)], Vec::new()),
        ],
    );
    assert_eq!(scored.matches_label, 1);
    assert_eq!(scored.false_dismisses, 1);
    assert_eq!(scored.false_wakes, 0);

    let quiet_case = bare_case(false);
    let scored = tally(
        &quiet_case,
        vec![attempt(vec![(2, Disposition::Pin)], Vec::new())],
    );
    assert_eq!(scored.false_wakes, 1);
    assert_eq!(scored.matches_label, 0);
}

#[test]
fn the_first_miss_keeps_the_reasoning() {
    let mut missed = attempt(vec![(2, Disposition::Dismiss)], Vec::new());
    missed.thinking = vec!["the operator probably does not care".to_string()];
    let scored = tally(&bare_case(true), vec![missed]);
    let first = scored.first_miss.expect("a miss is recorded");
    assert!(first.contains("dismiss [2]"), "{first}");
    assert!(first.contains("probably does not care"), "{first}");
}

#[test]
fn the_modal_decision_is_over_usable_verdicts_only() {
    let split = IdleArmTally {
        draws: 3,
        wakes: 1,
        matches_label: 1,
        false_dismisses: 1,
        no_verdicts: 1,
        ..Default::default()
    };
    assert_eq!(split.modal_wake(), None, "a tie is no modal decision");

    let mostly_wake = IdleArmTally {
        draws: 3,
        wakes: 2,
        matches_label: 2,
        false_dismisses: 1,
        ..Default::default()
    };
    assert_eq!(mostly_wake.modal_wake(), Some(true));

    // On a no-wake case `matches_label` counts dismissals, so a mode derived from the label
    // would call this a wake; the raw count is what keeps the two directions apart.
    let mostly_quiet = IdleArmTally {
        draws: 3,
        wakes: 1,
        matches_label: 2,
        false_wakes: 1,
        ..Default::default()
    };
    assert_eq!(mostly_quiet.modal_wake(), Some(false));

    let uncovered = IdleArmTally {
        draws: 2,
        no_verdicts: 2,
        ..Default::default()
    };
    assert_eq!(uncovered.modal_wake(), None);
}

#[test]
fn the_report_names_the_label_and_the_disagreement() {
    let case = bare_case(true);
    let report = case_report(
        &case,
        vec![attempt(vec![(2, Disposition::Pin)], Vec::new())],
        vec![attempt(vec![(2, Disposition::Dismiss)], Vec::new())],
    );
    assert!(report.disagrees);

    let comparison = IdleComparison {
        model: "test-model".to_string(),
        lower_effort: Some("low".to_string()),
        cases: vec![report],
    };
    let text = comparison.report();
    assert!(text.contains("expects wake"), "{text}");
    assert!(text.contains("effort=low"), "{text}");
    assert!(text.contains("1/1 correct"), "{text}");
    assert!(text.contains("modal decision differs"), "{text}");
}

#[tokio::test]
async fn an_empty_corpus_builds_both_arms_and_spends_nothing() {
    // Exercises the runner's arm construction — the clone that carries everything but the
    // effort — without a request, since no case means no draw.
    let endpoint = crate::inference::api::Endpoint::from_credentials(
        crate::record::log::Provider::Anthropic,
        "sk-ant-xyz",
        None,
        None,
    )
    .unwrap();
    let agent = Client::new(endpoint, "test-model", "test-system");
    let comparison = compare_idle_efforts(&agent, &[], Some("low"), 2, &[]).await;
    assert_eq!(comparison.model, "test-model");
    assert!(comparison.cases.is_empty());
    assert!(comparison.report().contains("effort=low"));
}
