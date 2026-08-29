use super::*;
use serde_json::json;

/// The signature as one line of text — the shape the header spans render, flattened
/// so a test can pin it. Mirrors the web client's `callSignature`
/// (`web/src/render/parse.ts`), which exists there for the gate cue.
fn sig(name: &str, input: serde_json::Value) -> String {
    let view = call_view(name, &input);
    let name = match &view.server {
        Some(server) => format!("{server}.{}", view.tool),
        None => view.tool.clone(),
    };
    match &view.args {
        None => match &view.target {
            None => name,
            Some(target) => format!("{name} · {target}"),
        },
        Some(args) => {
            let args = args
                .iter()
                .map(|arg| format!("{}: {}", arg.key, arg.text))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({args})")
        }
    }
}

/// The header spans of a rendered call, flattened to their text.
fn header(name: &str, input: serde_json::Value, arriving: bool) -> String {
    call_spans(name, &input, arriving)
        .0
        .iter()
        .map(|span| span.text.as_ref())
        .collect()
}

#[test]
fn mounted_server_call_is_dotted_with_the_server_its_own_segment() {
    let view = call_view(
        "ferritin__search",
        &json!({ "crate": "serde", "query": "Deserialize" }),
    );
    assert_eq!(view.server.as_deref(), Some("ferritin"));
    assert_eq!(view.tool, "search");
    assert_eq!(
        sig(
            "ferritin__search",
            json!({ "crate": "serde", "query": "Deserialize" })
        ),
        r#"ferritin.search(crate: "serde", query: "Deserialize")"#
    );
    // A built-in has no server half — it is not given an `efference.` prefix.
    assert_eq!(call_view("read", &json!({})).server, None);
    // The boundary is the first `__`; a server's own `__` keeps it.
    assert_eq!(split_tool_name("srv__a__b"), (Some("srv"), "a__b"));

    // The server segment is a dimmed span of its own, ahead of the tool name.
    let spans = call_spans("ferritin__get", &json!({}), false).0;
    assert_eq!(spans[0].text.as_ref(), "ferritin.");
    assert_eq!(spans[0].style, SpanStyle::Comment);
    assert_eq!(spans[1].text.as_ref(), "get");
    assert_eq!(spans[1].style, SpanStyle::FunctionName);
}

#[test]
fn a_call_with_no_arguments_is_empty_not_an_envelope_of_braces() {
    let view = call_view("check_inbox", &json!({}));
    assert_eq!(view.args, Some(Vec::new()));
    assert!(view.bodies.is_empty());
    assert_eq!(sig("check_inbox", json!({})), "check_inbox()");
    assert!(call_spans("check_inbox", &json!({}), false).1.is_empty());
    // Nor does an intent-only call have arguments: the intent is its own line.
    assert_eq!(sig("list", json!({ "intent": "look around" })), "list()");
}

#[test]
fn an_argument_is_inline_when_scalar_and_fitting_elided_otherwise() {
    // Unfamiliar tool, arbitrary shape — no per-tool knowledge involved. Keys read
    // in the order `serde_json::Value` holds them, which is sorted.
    assert_eq!(
        sig(
            "watch_process",
            json!({
                "handle": "p#4",
                "pattern": "Listening on port",
                "lines": 40,
                "all": true,
                "region": { "x": 1, "y": 2 },
            })
        ),
        r#"watch_process(all: true, handle: "p#4", lines: 40, pattern: "Listening on port", region: {"x":1,"y":2})"#
    );

    // A value past the budget is elided to its shape and rendered below.
    let long = "x".repeat(200);
    let wide = call_view(
        "mystery",
        &json!({ "note": long, "deep": { "a": [1, 2, 3], "because": long } }),
    );
    assert_eq!(
        wide.args,
        Some(vec![
            SigArg {
                key: "deep".into(),
                text: "{2 keys}".into(),
                elided: true
            },
            SigArg {
                key: "note".into(),
                text: "200 chars".into(),
                elided: true
            },
        ])
    );
    assert_eq!(wide.bodies[1].key.as_deref(), Some("note"));
    assert_eq!(wide.bodies[1].lang, None);
    assert_eq!(wide.bodies[1].text, long);
    assert_eq!(wide.bodies[0].lang.as_deref(), Some("json"));
}

#[test]
fn a_multi_line_value_is_a_shape_in_the_signature_and_a_block_below() {
    let contents = (0..505).map(|n| format!("line {n}")).collect::<Vec<_>>();
    let contents = contents.join("\n");
    let input = json!({
        "intent": "lay it down",
        "contents": contents,
        "path": "src/main.rs",
        "overwrite": true,
    });
    // Inline arguments lead whatever order the input serialized in — they are the
    // call's identity, and a 505-line shape between them buries it.
    assert_eq!(
        sig("write", input.clone()),
        r#"write(overwrite: true, path: "src/main.rs", contents: 505 lines)"#
    );
    let (_, blocks) = call_spans("write", &input, false);
    // The block is labelled with its key and highlighted by the path's extension.
    assert_eq!(blocks.len(), 2);
    assert!(matches!(&blocks[1], DocumentNode::CodeBlock { lang, code }
        if lang.as_deref() == Some("rust") && code.as_ref() == contents));
}

#[test]
fn bash_keeps_its_command_bare() {
    let view = call_view("bash", &json!({ "command": "ls -la", "intent": "peek" }));
    assert_eq!(view.args, None);
    assert_eq!(view.target.as_deref(), Some("ls -la"));
    assert!(view.bodies.is_empty());
    assert_eq!(
        header("bash", json!({ "command": "ls -la" }), false),
        "bash · ls -la"
    );
    // Only a newline makes it a block.
    let multi = call_view("bash", &json!({ "command": "a\nb" }));
    assert_eq!(multi.target, None);
    assert_eq!(
        multi.bodies,
        vec![CallBody {
            key: None,
            lang: Some("bash".into()),
            text: "a\nb".into()
        }]
    );
    // No key, so no label line — the block is the command.
    let (_, blocks) = call_spans("bash", &json!({ "command": "a\nb" }), false);
    assert_eq!(blocks.len(), 1);
}

#[test]
fn the_edit_family_signs_its_arguments_but_emits_no_body() {
    let input = json!({
        "file_path": "a.rs",
        "operation": "replace",
        "anchor": "fn a() {\n  b();\n}",
    });
    assert_eq!(
        sig("edit", input.clone()),
        r#"edit(file_path: "a.rs", operation: "replace", anchor: 3 lines)"#
    );
    // The `===DIFF===` result below is the representation; a body would show the
    // change twice.
    assert!(call_view("edit", &input).bodies.is_empty());
    assert!(call_spans("edit", &input, false).1.is_empty());
    // `undo_edit` takes no arguments at all.
    assert_eq!(sig("undo_edit", json!({})), "undo_edit()");
}

#[test]
fn read_search_and_list_need_no_special_case() {
    assert_eq!(
        sig("read", json!({ "paths": ["a.rs", "b.rs"] })),
        r#"read(paths: ["a.rs","b.rs"])"#
    );
    assert_eq!(
        sig("search", json!({ "pattern": "foo", "path": "src" })),
        r#"search(path: "src", pattern: "foo")"#
    );
    assert_eq!(
        sig("read_image", json!({ "path": "m.png", "long_edge": 2576 })),
        r#"read_image(long_edge: 2576, path: "m.png")"#
    );
}

#[test]
fn a_shape_says_how_much_there_is_and_of_what() {
    assert_eq!(shape_of(&json!("one line, but very long")), "23 chars");
    assert_eq!(shape_of(&json!("a\nb\nc")), "3 lines");
    assert_eq!(shape_of(&json!([1, 2, 3])), "[3 items]");
    assert_eq!(shape_of(&json!({ "a": 1 })), "{1 key}");
    assert_eq!(shape_of(&json!({})), "{0 keys}");
    // The budget is the boundary, inclusive.
    assert!(inline_value(&json!("x".repeat(48))).is_some());
    assert_eq!(inline_value(&json!("x".repeat(49))), None);
    assert_eq!(inline_value(&json!(null)).as_deref(), Some("null"));
}

#[test]
fn a_still_arriving_call_puts_its_ellipsis_inside_the_parentheses() {
    // The signature never reads as closed before it is (`projection/partial.rs`).
    assert_eq!(
        header("write", json!({ "path": "src/api.rs" }), true),
        r#"write(path: "src/api.rs", …)"#
    );
    assert_eq!(header("check_inbox", json!({}), true), "check_inbox(…)");
    assert_eq!(
        header("bash", json!({ "command": "ls" }), true),
        "bash · ls…"
    );
}
