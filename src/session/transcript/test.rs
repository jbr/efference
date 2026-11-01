use super::*;

/// A code span is fenced by a backtick run longer than any inside it, padded by a space
/// when the text opens or closes with a backtick.
#[test]
fn a_code_span_outgrows_the_backticks_inside_it() {
    assert_eq!(code_span("ls"), "`ls`");
    assert_eq!(code_span(""), "``");
    assert_eq!(code_span("echo `date`"), "`` echo `date` ``");
    assert_eq!(code_span("a ``b`` c"), "```a ``b`` c```");
    assert_eq!(code_span("`x"), "`` `x ``");
    assert_eq!(code_span("x```"), "```` x``` ````");
}
