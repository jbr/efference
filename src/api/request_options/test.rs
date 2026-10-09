use super::*;

fn body(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        panic!("a body is a table");
    };
    map
}

#[test]
fn a_body_of_vendor_fields_is_kept() {
    let options = RequestOptions::new(
        body(serde_json::json!({"parallel_tool_calls": false, "safe_prompt": true})),
        Some("max_completion_tokens"),
    )
    .unwrap();
    assert_eq!(options.body.len(), 2);
    assert_eq!(
        options.output_cap_field,
        OutputCapField::MaxCompletionTokens
    );
}

/// A key the encoder writes is refused, and the effort's refusal names the knob
/// that sets it.
#[test]
fn encoder_keys_are_refused() {
    let error = RequestOptions::new(body(serde_json::json!({"messages": []})), None).unwrap_err();
    assert!(error.contains("`messages`"), "{error}");
    let error = RequestOptions::new(body(serde_json::json!({"reasoning_effort": "high"})), None)
        .unwrap_err();
    assert!(error.contains("--effort"), "{error}");
}

#[test]
fn an_unknown_cap_spelling_is_refused() {
    let error = RequestOptions::new(Map::new(), Some("max_output_tokens")).unwrap_err();
    assert!(error.contains("max_output_tokens"), "{error}");
    assert_eq!(
        RequestOptions::new(Map::new(), None).unwrap(),
        RequestOptions::default()
    );
}
