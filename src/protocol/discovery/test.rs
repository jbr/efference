use super::*;
use crate::protocol::channel_value;
use crate::protocol::sessions::ListedStatus;

#[test]
fn the_path_answer_is_the_pinned_wire_bytes() {
    let answer = PathCandidates {
        prefix: "./".to_string(),
        directory: "/w".to_string(),
        filter: String::new(),
        truncated: false,
        problem: None,
        entries: vec![PathCandidate {
            name: "a".to_string(),
            original: "./a".to_string(),
            path: "/w/a".to_string(),
            target: ReferenceTargetKind::File,
            symlink: false,
        }],
    };
    assert_eq!(
        channel_value(&answer).to_string(),
        r#"{"directory":"/w","entries":[{"name":"a","original":"./a","path":"/w/a","symlink":false,"target":"file"}],"filter":"","prefix":"./","truncated":false}"#
    );
}

#[test]
fn a_problem_is_a_field_of_an_empty_answer() {
    let answer = PathCandidates {
        prefix: "nope/".to_string(),
        directory: "/w/nope".to_string(),
        filter: String::new(),
        truncated: false,
        problem: Some("no such directory".to_string()),
        entries: Vec::new(),
    };
    assert_eq!(
        channel_value(&answer).to_string(),
        r#"{"directory":"/w/nope","entries":[],"filter":"","prefix":"nope/","problem":"no such directory","truncated":false}"#
    );
}

#[test]
fn the_agent_answer_is_the_pinned_wire_bytes() {
    let answer = AgentCandidates {
        prefix: "@on".to_string(),
        truncated: false,
        problem: None,
        agents: Vec::new(),
    };
    assert_eq!(
        channel_value(&answer).to_string(),
        r#"{"agents":[],"prefix":"@on","truncated":false}"#
    );
}

#[test]
fn an_agent_candidate_is_the_listings_own_row() {
    let listed = ListedSession::new("one", ListedStatus::Idle);
    let answer = AgentCandidates {
        prefix: "@".to_string(),
        truncated: true,
        problem: None,
        agents: vec![listed.clone()],
    };
    assert_eq!(
        channel_value(&answer)["agents"][0],
        serde_json::to_value(&listed).unwrap()
    );
}
