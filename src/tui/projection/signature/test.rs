//! The web client's `signature.test.ts` and `signatureLayout.test.ts`, case for
//! case, so the two clients' signature rules cannot drift apart unnoticed.

use serde_json::json;

use super::*;

fn sig(name: &str, input: &Value) -> String {
    flat_signature(&call_view(name, input))
}

/// The layout as the text it renders, one row per line, indents as spaces.
fn text(layout: &SigLayout) -> String {
    layout
        .rows
        .iter()
        .map(|row| {
            let tokens: String = row.tokens.iter().map(|t| t.text.as_str()).collect();
            format!("{}{tokens}", " ".repeat(row.indent))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn at(columns: Option<usize>, name: &str, input: &Value) -> SigLayout {
    layout_signature(&call_view(name, input), columns)
}

#[test]
fn a_mounted_servers_call_is_dotted_with_the_server_its_own_segment() {
    let view = call_view(
        "ferritin__search",
        &json!({"crate": "serde", "query": "Deserialize"}),
    );
    assert_eq!(view.server.as_deref(), Some("ferritin"));
    assert_eq!(view.tool, "search");
    assert_eq!(
        flat_signature(&view),
        r#"ferritin.search(crate: "serde", query: "Deserialize")"#
    );
}

#[test]
fn a_call_with_no_arguments_is_empty() {
    let view = call_view("check_inbox", &json!({}));
    assert_eq!(view.args, Some(vec![]));
    assert!(view.bodies.is_empty());
    assert_eq!(flat_signature(&view), "check_inbox()");
    // Nor does an intent-only call have arguments: the intent is its own line.
    assert_eq!(sig("list", &json!({"intent": "look around"})), "list()");
}

#[test]
fn a_builtins_primary_argument_reads_positionally_an_array_spread() {
    assert_eq!(
        sig("read", &json!({"paths": ["a.rs", "b.rs"], "from_line": 3})),
        r#"read("a.rs", "b.rs", from_line: 3)"#
    );
    // Positional first, whatever order the input serialized in.
    assert_eq!(
        sig("search", &json!({"path": "src", "pattern": "foo"})),
        r#"search("foo", path: "src")"#
    );
    assert_eq!(
        sig("read_image", &json!({"path": "m.png", "long_edge": 2576})),
        r#"read_image("m.png", long_edge: 2576)"#
    );
    // The key is kept on the argument, for the chip table.
    assert_eq!(
        call_view("read", &json!({"paths": ["a.rs"]})).args,
        Some(vec![SigArg {
            key: "paths".into(),
            label: None,
            value: json!("a.rs"),
        }])
    );
    // A mounted tool of the same name is not a built-in.
    assert_eq!(
        sig("srv__read", &json!({"paths": ["a.rs"]})),
        r#"srv.read(paths: ["a.rs"])"#
    );
}

#[test]
fn every_single_line_argument_stays_in_the_signature_whatever_its_length() {
    let long = "x".repeat(200);
    let view = call_view(
        "mystery",
        &json!({"note": long, "region": {"x": 1, "y": 2}}),
    );
    assert!(view.bodies.is_empty());
    assert_eq!(
        flat_signature(&view),
        format!(r#"mystery(note: "{long}", region: {{ x: 1, y: 2 }})"#)
    );
}

#[test]
fn a_multi_line_value_is_content_gone_from_the_signature_a_body_below() {
    let contents = (0..505)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let view = call_view(
        "write",
        &json!({"intent": "lay it down", "contents": contents, "path": "src/main.rs", "overwrite": true}),
    );
    assert_eq!(
        flat_signature(&view),
        r#"write("src/main.rs", overwrite: true)"#
    );
    assert_eq!(view.intent.as_deref(), Some("lay it down"));
    assert_eq!(
        view.bodies,
        vec![CallBody {
            key: Some("contents".into()),
            lang: Some("rust".into()),
            text: contents,
            prose: false,
        }]
    );
    // A collection holding a multi-line string is content too.
    let nested = call_view("mystery", &json!({"item": {"body": "a\nb"}, "n": 1}));
    assert_eq!(flat_signature(&nested), "mystery(n: 1)");
    assert_eq!(nested.bodies[0].lang.as_deref(), Some("json"));
}

#[test]
fn bash_keeps_its_command_bare() {
    let view = call_view("bash", &json!({"command": "ls -la", "intent": "triage"}));
    assert_eq!(
        view,
        CallView {
            intent: Some("triage".into()),
            server: None,
            tool: "bash".into(),
            args: None,
            target: Some("ls -la".into()),
            shell: Some(Shell {
                background: false,
                background_after: None,
            }),
            bodies: vec![],
        }
    );
    assert_eq!(
        call_view("bash", &json!({"command": "make", "background_after": 600})).shell,
        Some(Shell {
            background: false,
            background_after: Some(600),
        })
    );
    // Only a newline makes it a block.
    assert_eq!(
        call_view("bash", &json!({"command": "a\nb"})).bodies,
        vec![CallBody {
            key: None,
            lang: Some("bash".into()),
            text: "a\nb".into(),
            prose: false,
        }]
    );
}

#[test]
fn the_edit_family_omits_its_content_and_emits_no_body() {
    let view = call_view(
        "edit",
        &json!({"file_path": "a.rs", "operation": "replace", "anchor": "fn a() {\n  b();\n}"}),
    );
    assert_eq!(
        flat_signature(&view),
        r#"edit("a.rs", operation: "replace")"#
    );
    assert!(view.bodies.is_empty());
}

#[test]
fn a_hand_off_puts_the_brief_under_whom_it_is_for_as_prose() {
    let view = call_view(
        crate::control_tools::HAND_OFF_WIRE_NAME,
        &json!({"brief": "Greet the operator.", "intent": "hand off"}),
    );
    assert_eq!(view.args, Some(vec![]));
    assert_eq!(
        view.bodies,
        vec![CallBody {
            key: Some("to the new session".into()),
            lang: None,
            text: "Greet the operator.".into(),
            prose: true,
        }]
    );
}

#[test]
fn a_call_that_fits_is_one_line() {
    let layout = at(
        Some(80),
        "read",
        &json!({"paths": ["a.rs"], "from_line": 3}),
    );
    assert!(!layout.broken);
    assert_eq!(text(&layout), r#"read("a.rs", from_line: 3)"#);
}

#[test]
fn before_the_width_is_known_the_signature_is_flat_and_nothing_is_pulled() {
    let layout = at(None, "mystery", &json!({"note": "x".repeat(500)}));
    assert!(!layout.broken);
    assert!(layout.pulled.is_empty());
}

#[test]
fn a_call_that_does_not_fit_breaks_after_the_paren_one_argument_per_line() {
    let layout = at(
        Some(40),
        "read",
        &json!({"paths": ["src/api/provider/listing.rs", "src/boot.rs"], "from_line": 3}),
    );
    assert!(layout.broken);
    assert_eq!(
        text(&layout),
        [
            "read(",
            r#"  "src/api/provider/listing.rs","#,
            r#"  "src/boot.rs","#,
            "  from_line: 3",
            ")",
        ]
        .join("\n")
    );
}

#[test]
fn a_lone_scalar_argument_is_not_broken_around() {
    let path = "../codex/codex-rs/model-provider/src/models_cache_ttl.rs";
    let layout = at(Some(30), "read", &json!({"paths": [path]}));
    assert!(!layout.broken);
    assert_eq!(text(&layout), format!(r#"read("{path}")"#));
}

#[test]
fn a_collection_that_does_not_fit_on_its_row_breaks_inside_the_same_way() {
    let layout = at(
        Some(30),
        "mystery",
        &json!({"globs": ["src/**/*.rs", "web/src/**/*.ts"], "n": 1}),
    );
    assert_eq!(
        text(&layout),
        [
            "mystery(",
            "  globs: [",
            r#"    "src/**/*.rs","#,
            r#"    "web/src/**/*.ts""#,
            "  ],",
            "  n: 1",
            ")",
        ]
        .join("\n")
    );
    // One that fits on its row stays flat there.
    let other = "x".repeat(20);
    assert_eq!(
        text(&at(
            Some(40),
            "mystery",
            &json!({"globs": ["a", "b"], "other": other})
        )),
        [
            "mystery(".to_string(),
            r#"  globs: ["a", "b"],"#.to_string(),
            format!(r#"  other: "{other}""#),
            ")".to_string(),
        ]
        .join("\n")
    );
}

#[test]
fn a_value_longer_than_three_lines_at_this_width_leaves_the_signature() {
    let pattern = "a|".repeat(60);
    let wide = at(
        Some(100),
        "search",
        &json!({"pattern": pattern, "path": "src"}),
    );
    assert!(wide.pulled.is_empty());
    let narrow = at(
        Some(40),
        "search",
        &json!({"pattern": pattern, "path": "src"}),
    );
    assert_eq!(
        narrow
            .pulled
            .iter()
            .map(|a| a.key.as_str())
            .collect::<Vec<_>>(),
        ["pattern"]
    );
    assert_eq!(text(&narrow), r#"search(path: "src")"#);
}

#[test]
fn bashs_command_is_one_row_whatever_the_width() {
    let command = "cargo test --workspace -- --nocapture projection::stable";
    let layout = at(Some(20), "bash", &json!({"command": command}));
    assert_eq!(layout.rows.len(), 1);
    assert_eq!(text(&layout), command);
    assert!(layout.shell);
    assert_eq!(
        text(&at(
            Some(80),
            "bash",
            &json!({"command": "make", "background_after": 600})
        )),
        "makebackground after 600s"
    );
    assert_eq!(
        text(&at(
            Some(80),
            "bash",
            &json!({"command": "cargo watch", "background": true, "background_after": 5})
        )),
        "cargo watchbackground"
    );
}
