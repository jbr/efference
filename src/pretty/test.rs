
use super::*;
use serde_json::json;

#[test]
fn scalar_array_folds_onto_one_line() {
    let v = json!({ "required": ["source", "destination"] });
    assert_eq!(to_pseudo_yaml(&v), "required: source, destination");
}

/// The motivating case: a multi-line string unfolds into an indented block
/// under its key (the exact shape requested for the dev view).
#[test]
fn multiline_string_unfolds_into_indented_block() {
    let v = json!({
        "include_extensions": {
            "description": "Maximum number of results to return\nDefault: 50"
        }
    });
    let expected = "\
include_extensions:
  description:
    Maximum number of results to return
    Default: 50";
    assert_eq!(to_pseudo_yaml(&v), expected);
}

/// An array of objects becomes a `- ` list with the first key riding the dash.
#[test]
fn array_of_objects_renders_as_dash_list() {
    let v = json!({
        "messages": [
            { "role": "user", "content": [ { "type": "text", "text": "hello" } ] }
        ]
    });
    let expected = "\
messages:
  - content:
      - text: hello
        type: text
    role: user";
    assert_eq!(to_pseudo_yaml(&v), expected);
}

/// Scalars stay inline; nesting indents by two spaces per level.
#[test]
fn nested_scalars_indent_two_per_level() {
    let v = json!({ "a": { "b": { "c": 1, "d": true } } });
    let expected = "\
a:
  b:
    c: 1
    d: true";
    assert_eq!(to_pseudo_yaml(&v), expected);
}

/// Empty containers render as compact tokens, not dangling keys.
#[test]
fn empty_containers_are_compact() {
    let v = json!({ "arr": [], "obj": {}, "s": "" });
    assert_eq!(to_pseudo_yaml(&v), "arr: []\nobj: {}\ns:");
}

/// `keyed` lets a caller emit a non-sorted top-level order.
#[test]
fn keyed_renders_a_single_entry() {
    assert_eq!(keyed("model", &json!("haiku")), vec!["model: haiku"]);
    assert_eq!(
        keyed("tool_choice", &json!({ "type": "auto" })),
        vec!["tool_choice:", "  type: auto"]
    );
}
