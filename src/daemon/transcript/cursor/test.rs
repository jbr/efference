use super::*;

const RENDERED: &str = "transcript of child, seq 5–9\n5  child: done\n";

/// A reader with no cursor gets the tail of the conversation; the render's header says
/// what was cut and the plan adds that it is the first read. The cursor lands on the last
/// seq.
#[test]
fn a_first_read_is_the_tail() {
    let plan = ReadPlan::new(None, None, None, Some(99));
    assert_eq!(
        (plan.since, plan.before, plan.tail),
        (None, Some(100), Some(FIRST_READ_TAIL))
    );
    assert_eq!(plan.cursor_after(None), Some(99));
    let cut = "transcript of child, seq 60–99: the last 40 lines; 12 lines before \
               — since: 0 reads from the start\n5  child: done\n";
    assert_eq!(
        plan.framed("transcript of child", cut.into()),
        "transcript of child, seq 60–99: the last 40 lines; 12 lines before \
         — since: 0 reads from the start (your first read)\n5  child: done\n"
    );

    let short = ReadPlan::new(None, None, None, Some(9));
    assert_eq!(
        short.framed("transcript of child", RENDERED.into()),
        "transcript of child, seq 5–9 (your first read)\n5  child: done\n"
    );
}

/// A reader with a cursor reads what came after it, and the cursor moves to the end.
#[test]
fn a_later_read_is_what_is_new() {
    let plan = ReadPlan::new(Some(4), None, None, Some(9));
    assert_eq!(
        (plan.since, plan.before, plan.tail),
        (Some(5), Some(10), None)
    );
    assert_eq!(plan.cursor_after(Some(4)), Some(9));
    assert_eq!(
        plan.framed("transcript of child", RENDERED.into()),
        "transcript of child, new since your last read: seq 5–9\n5  child: done\n"
    );

    let nothing = ReadPlan::new(Some(9), None, None, Some(9));
    assert_eq!(nothing.cursor_after(Some(9)), Some(9));
    assert_eq!(
        nothing.framed(
            "transcript of child",
            "transcript of child (no entries)\n".into()
        ),
        "transcript of child: nothing new since your last read (seq 9)\n"
    );
}

/// An explicit window is read as asked, keeps the render's own header, and still moves
/// the cursor to the end of what it returned.
#[test]
fn an_asked_window_moves_the_cursor_to_its_end() {
    let since = ReadPlan::new(Some(2), Some(5), None, Some(9));
    assert_eq!((since.since, since.tail), (Some(5), None));
    assert_eq!(since.cursor_after(Some(2)), Some(9));
    assert_eq!(
        since.framed("transcript of child", RENDERED.into()),
        RENDERED
    );

    let tail = ReadPlan::new(None, None, Some(3), Some(9));
    assert_eq!(tail.cursor_after(None), Some(9));

    // A window past the end returns nothing, and leaves the cursor where it was.
    let past = ReadPlan::new(Some(2), Some(12), None, Some(9));
    assert_eq!(past.cursor_after(Some(2)), Some(2));
}

/// An empty log has nothing to frame and no cursor to set.
#[test]
fn an_empty_log() {
    let plan = ReadPlan::new(None, None, None, None);
    assert_eq!(plan.before, None);
    assert_eq!(plan.cursor_after(None), None);
    let empty = "transcript of child (no entries)\n";
    assert_eq!(plan.framed("transcript of child", empty.into()), empty);
}
