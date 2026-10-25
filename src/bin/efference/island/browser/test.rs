use super::*;

#[test]
fn a_link_lands_on_the_session_page() {
    assert_eq!(
        session_link(
            "http://localhost:51234/#claim=Zm9vYmFyYmF6cXV4",
            "keen-fern"
        ),
        "http://localhost:51234/sessions/keen-fern/#claim=Zm9vYmFyYmF6cXV4"
    );
}
