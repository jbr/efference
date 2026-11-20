//! The decision rung's hold in plain words (`questions::plain_reason`).

use super::*;

/// Each hold reads as the clause its reason names, without the figure.
#[test]
fn a_hold_reads_as_what_held_it() {
    assert_eq!(
        plain_reason(&answers(&[(INSPECTS, 0.9), (SECRET, 0.85)])).as_deref(),
        Some("it would print, store or send a secret")
    );
    assert_eq!(
        plain_reason(&answers(&[(CONTAINED, 0.9)]).with_outside_grants(true)).as_deref(),
        Some(
            "it reaches outside the granted paths, and nothing the operator or a supervising \
             agent said asks for it"
        )
    );
    assert_eq!(
        plain_reason(&answers(&[(CONTAINED, 0.41)])).as_deref(),
        Some("it was not clearly asked for, agreed to, contained, or read-only")
    );
}

/// Answers that fold to a clear held nothing, and an entry recorded under an earlier
/// fold has no readings to fold: neither has a reason to phrase, and the status shows
/// the entry's own reason instead.
#[test]
fn a_clear_and_an_earlier_fold_have_no_plain_reason() {
    assert_eq!(plain_reason(&answers(&[(ASKED, 0.9)])), None);
    assert_eq!(plain_reason(&Axes::default()), None);
    assert_eq!(
        plain_reason(&Axes::default().with_prefix_safe(Some(0.9))),
        None
    );
}

/// The phrasing carries no figure and no question key.
#[test]
fn the_plain_reason_names_no_figure_or_key() {
    let holds = [
        answers(&[(INSPECTS, 0.9), (ONE_WAY, 0.7)]),
        Axes::new(
            answers(&[(INSPECTS, 0.9)])
                .readings
                .into_iter()
                .chain([(NOTE_WARNS.to_string(), reading(0.7))]),
        ),
        answers(&[(CONTAINED, 0.9)]).with_outside_grants(true),
        answers(&[(CONTAINED, 0.3)]),
    ];
    for axes in holds {
        let reason = plain_reason(&axes).expect("held");
        for term in ["0.", "best", "one-way", "note-warns", "held:"] {
            assert!(!reason.contains(term), "{term:?} in {reason:?}");
        }
    }
}
