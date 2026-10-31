use super::*;
use crate::protocol::answers::StatusWord;

/// A claim's two answers are told apart by their keys alone, and each reads back as the
/// one it was.
#[test]
fn a_claim_answer_is_the_token_or_the_word_pending() {
    let approved = SignInClaim::Approved {
        token: "abc".into(),
    };
    let waiting = SignInClaim::Waiting(StatusBody::new(StatusWord::Pending));
    assert_eq!(
        serde_json::to_string(&approved).unwrap(),
        r#"{"token":"abc"}"#
    );
    assert_eq!(
        serde_json::to_string(&waiting).unwrap(),
        r#"{"status":"pending"}"#
    );
    for answer in [approved, waiting] {
        let text = serde_json::to_string(&answer).unwrap();
        assert_eq!(serde_json::from_str::<SignInClaim>(&text).unwrap(), answer);
    }
}
