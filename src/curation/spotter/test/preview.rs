use super::*;
use crate::config::policy::Directories;
use crate::inference::api::Endpoint;
use crate::record::log::Provider;
use crate::record::utterances::{Speaker, Utterance};

/// The preview is the request `assess` sends: the persona, then one user turn of the
/// scope block, the utterances and the move, each block ending at its breakpoint.
#[test]
fn the_preview_is_the_persona_and_the_assessed_turn() {
    let role = ConfigFixture::new()
        .with_path(".", true, true)
        .with_scope("tidy the build")
        .build();
    let call = ToolCall::new(BASH, "x", json!({ "command": "cargo clean" }));
    let said = [Utterance::new(Speaker::Operator, "clean it out", None)];
    let circumstances = Circumstances::default().with_utterances(Some(&said));
    let dirs = Directories::at(Path::new("/proj"));
    let (persona, turns) = preview_request(&role, &call, &circumstances, &dirs, None);
    assert_eq!(persona, PERSONA);
    let [WireTurn::User(parts)] = turns.as_slice() else {
        panic!("one user turn: {turns:?}");
    };
    let breakpoints: Vec<usize> = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| matches!(part, UserPart::Breakpoint))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(breakpoints, [1, 4], "{parts:?}");
    let UserPart::Text(scope) = &parts[0] else {
        panic!("{parts:?}")
    };
    assert!(scope.starts_with("Mission:\ntidy the build"), "{scope}");
    let UserPart::Text(utterance) = &parts[3] else {
        panic!("{parts:?}")
    };
    assert_eq!(utterance, "Operator:\nclean it out\n\n");
    let Some(UserPart::Text(fresh)) = parts.last() else {
        panic!("{parts:?}")
    };
    assert!(fresh.contains("    cargo clean"), "{fresh}");
}

/// The body preview is the first request `assess` sends, built without sending it: the
/// persona as `system`, the forced `spot` call, and the turn's text.
#[test]
fn the_body_preview_is_the_forced_spot_request() {
    let spot = ForkSpot::new(
        Endpoint::without_url(Provider::Anthropic),
        "claude-haiku-4-5",
    )
    .with_directories(Directories::at(Path::new("/proj")));
    let call = ToolCall::new(BASH, "x", json!({ "command": "cargo clean" }));
    let body = spot
        .request_body(&Config::default(), &call, &Circumstances::default())
        .expect("the body serializes");
    let body: Value = serde_json::from_str(&body).expect("the body is JSON");
    assert_eq!(body["system"], PERSONA);
    assert_eq!(body["tool_choice"]["name"], SPOT);
    assert_eq!(body["tools"][0]["name"], SPOT);
    let text = body["messages"][0]["content"].to_string();
    assert!(text.contains("cargo clean"), "{text}");
    assert!(text.contains("cache_control"), "{text}");
}
