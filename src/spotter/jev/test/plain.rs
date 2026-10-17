//! The decision rung's hold in plain words (`questions::plain_reason`).

use super::*;

/// The precedent reading the fold recorded beside the axes.
fn with_precedent(axes: Axes, same: usize, wider: usize, required: Option<usize>) -> Axes {
    Axes {
        precedent: Some(Precedent {
            candidates: same + wider + 1,
            same,
            wider,
            required,
            compared: Vec::new(),
        }),
        ..axes
    }
}

/// Each cause the level can have reads as a clause about the move, then what the
/// session's allowed calls said; no level numbers or figures appear.
#[test]
fn a_hold_reads_as_its_cause_then_its_history() {
    let scope = Axes {
        in_scope: 0.21,
        ..clean()
    };
    assert_eq!(
        plain_reason(&with_precedent(scope.clone(), 0, 0, Some(1))).as_deref(),
        Some(
            "its reviewer is unsure this fits the session's scope; no earlier call like it \
             was allowed"
        )
    );
    let outside = Axes {
        path_outside: 0.62,
        ..clean()
    };
    assert_eq!(
        plain_reason(&with_precedent(outside, 0, 2, Some(1))).as_deref(),
        Some(
            "it reaches outside the paths this session is granted; no earlier call like it \
             was allowed; it goes further than 2 calls allowed earlier"
        )
    );
    assert_eq!(
        plain_reason(&with_precedent(at_level(3), 1, 0, Some(2))).as_deref(),
        Some(
            "its reviewer rates it serious: hard to undo, or reaching other people; only 1 \
             earlier call like it was allowed, and a call like this needs 2"
        )
    );
    assert_eq!(
        plain_reason(&with_precedent(at_level(4), 3, 0, None)).as_deref(),
        Some(
            "its reviewer rates it severe: it could expose a credential or destroy data; no \
             earlier approval clears a call this risky"
        )
    );
    let denied = Axes {
        path_denied: 0.4,
        ..clean()
    };
    assert_eq!(
        plain_reason(&with_precedent(denied, 0, 0, Some(2))).as_deref(),
        Some("it names a path this session is denied; no earlier call like it was allowed")
    );
    let watched = Axes {
        watch_for_hit: Some(0.6),
        ..clean()
    };
    assert!(
        plain_reason(&with_precedent(watched, 0, 0, Some(2)))
            .unwrap()
            .starts_with("it may match something the operator asked to watch for;")
    );
}

/// A printed secret gates whatever the history says, and says only that.
#[test]
fn a_secret_is_its_own_reason() {
    let secret = Axes {
        exposes_secrets: 0.91,
        ..clean()
    };
    assert_eq!(
        plain_reason(&with_precedent(secret, 4, 0, Some(2))).as_deref(),
        Some("it may expose secret material")
    );
}

/// No reading to report leaves the history out; answers that fold to a clear held
/// nothing, so there is no reason to phrase.
#[test]
fn a_missing_reading_and_a_clear() {
    let scope = Axes {
        in_scope: 0.21,
        ..clean()
    };
    assert_eq!(
        plain_reason(&scope).as_deref(),
        Some("its reviewer is unsure this fits the session's scope")
    );
    assert_eq!(plain_reason(&clean()), None);
    assert_eq!(
        plain_reason(&with_precedent(at_level(2), 1, 0, Some(1))),
        None
    );
}

/// The phrasing never carries the rung's own terms.
#[test]
fn the_plain_reason_names_no_level_or_figure() {
    let cases = [
        with_precedent(at_level(2), 0, 1, Some(1)),
        with_precedent(at_level(3), 1, 0, Some(2)),
        with_precedent(at_level(4), 0, 0, None),
        Axes {
            path_outside: 0.62,
            ..clean()
        },
    ];
    for axes in cases {
        let reason = plain_reason(&axes).expect("held");
        for term in ["level", "%", "found", "approved call"] {
            assert!(!reason.contains(term), "{term:?} in {reason:?}");
        }
    }
}
