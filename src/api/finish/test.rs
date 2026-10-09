use super::*;

/// The same ending reads the same across the three dialects' spellings.
#[test]
fn each_dialect_maps_onto_one_vocabulary() {
    let cap = [
        Finish::messages("max_tokens"),
        Finish::responses("incomplete", Some("max_output_tokens"), false),
        Finish::completions("length", false),
    ];
    assert!(cap.iter().all(|f| f.reason == FinishReason::OutputCap));
    let calls = [
        Finish::messages("tool_use"),
        Finish::responses("completed", None, true),
        Finish::completions("tool_calls", true),
        Finish::completions("stop", true),
    ];
    assert!(calls.iter().all(|f| f.reason == FinishReason::ToolCalls));
    assert_eq!(
        Finish::completions("content_filter", false).reason,
        FinishReason::Filtered
    );
    assert_eq!(Finish::messages("refusal").reason, FinishReason::Refused);
}

/// An unmapped word stays unmapped, with the provider's own word kept.
#[test]
fn an_unknown_word_is_other_and_kept() {
    let finish = Finish::completions("eos_token", false);
    assert_eq!(finish.reason, FinishReason::Other);
    assert_eq!(finish.raw, "eos_token");
    assert_eq!(
        Finish::responses("incomplete", Some("quota"), false).raw,
        "incomplete:quota"
    );
}
