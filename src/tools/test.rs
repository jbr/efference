use super::*;
use serde_json::json;
use std::future::{pending, ready};

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall::new(name, "toolu_1", input)
}

#[test]
fn bash_only_advertises_exactly_the_bash_tool() {
    let defs = Toolbox::bash_only().definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0]["name"], json!(bash::NAME));
}

/// `tool_entry` has no hole for `Bash`, unlike `brief`: its description and
/// `intent`-injected `input_schema` are present even though the spotter's
/// brief skips it.
#[test]
fn tool_entry_answers_for_bash_even_though_brief_does_not() {
    let box_ = Toolbox::bash_only();
    assert!(box_.brief(bash::NAME).is_none(), "brief still has the hole");
    let entry = box_.tool_entry(bash::NAME).expect("bash has a definition");
    assert_eq!(entry.name, bash::NAME);
    assert!(entry.server.is_none(), "a built-in has no mounting server");
    assert!(entry.description.is_some());
    assert_eq!(
        entry.input_schema["properties"]["intent"]["type"],
        json!("string"),
        "the schema served is what the model was sent, intent included"
    );
    assert!(
        entry.input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "intent")
    );
}

/// An unmounted name has no entry — the `404` case `GET /v1/tools/:name` maps
/// onto.
#[test]
fn tool_entry_is_none_for_an_unmounted_name() {
    let box_ = Toolbox::bash_only();
    assert!(box_.tool_entry("nope").is_none());
    assert!(box_.tool_entry(read_image::NAME).is_none());
}

fn done(outcome: Outcome) -> RawResult {
    match outcome {
        Outcome::Done(result) => result,
        Outcome::Detached(_) => panic!("expected a completed result, got a detach"),
    }
}

#[tokio::test]
async fn routes_a_bash_call_to_the_bash_backend() {
    let box_ = Toolbox::bash_only();
    let result = done(
        box_.run(
            &call(bash::NAME, json!({"command": "echo routed"})),
            pending(),
            |_| {},
            |_| {},
        )
        .await,
    );
    assert!(!result.is_error);
    assert!(
        result.content.contains("routed"),
        "got {:?}",
        result.content
    );
}

#[tokio::test]
async fn unknown_tool_is_an_error_result_not_a_panic() {
    let box_ = Toolbox::bash_only();
    // An already-resolved abort proves the unknown path never touches it.
    let result = done(
        box_.run(
            &call("nope", json!({})),
            ready(Interrupt::Kill),
            |_| {},
            |_| {},
        )
        .await,
    );
    assert!(result.is_error);
    assert!(
        result.content.contains("unknown tool: nope"),
        "got {:?}",
        result.content
    );
    // The correction rides the result text: naming what failed without naming
    // what would work is what let a hallucinated name become a dead end
    // (docs/tools.md).
    assert!(
        result.content.contains("Mounted tools: bash"),
        "names what is mounted: {:?}",
        result.content
    );
    assert!(
        result.content.contains("run it through `bash`"),
        "points a program-shaped name at bash: {:?}",
        result.content
    );
}

/// A name can be an allowed bash *prefix* advertised as a skill (`ferritin`), which a
/// model then builds a structured tool call out of. The result text carries the route
/// back — bash — instead of a bare denial.
#[tokio::test]
async fn an_unknown_name_is_pointed_at_bash() {
    let toolbox = Toolbox::bash_only();
    let result = done(
        toolbox
            .run(
                &call("ferritin", json!({})),
                ready(Interrupt::Kill),
                |_| {},
                |_| {},
            )
            .await,
    );
    assert!(result.is_error);
    for expected in ["unknown tool: ferritin", "Mounted tools: bash", "`bash`"] {
        assert!(
            result.content.contains(expected),
            "{expected:?} in {:?}",
            result.content
        );
    }
}

/// Every mounted tool advertises `intent`, and advertises it as required — bash and
/// both in-process MCP servers alike, since injection happens once over the whole
/// mounted set rather than per backend (docs/tools.md).
#[test]
fn every_mounted_tool_advertises_a_required_intent() {
    let dir = std::env::temp_dir().join(format!("eff-intent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let defs = Toolbox::bash_and_mcps(dir.clone(), None)
        .unwrap()
        .definitions();
    assert!(defs.len() > 1, "expected a populated toolbox");
    for def in &defs {
        let schema = &def["input_schema"];
        assert_eq!(
            schema["properties"][INTENT]["type"],
            json!("string"),
            "{} is missing the intent property",
            def["name"]
        );
        let required = schema["required"].as_array().expect("required array");
        assert!(
            required.contains(&json!(INTENT)),
            "{} does not require intent: {required:?}",
            def["name"]
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The `tools[]` array lives in the cached prefix, so injection must be a pure
/// function of the mounted set: same toolbox, byte-identical definitions. A
/// non-deterministic key order here would silently cold-start the cache on every
/// request (docs/log-and-projection.md).
#[test]
fn intent_injection_is_byte_stable() {
    let dir = std::env::temp_dir().join(format!("eff-intent-stable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let toolbox = Toolbox::bash_and_mcps(dir.clone(), None).unwrap();
    let once = serde_json::to_string(&toolbox.definitions()).unwrap();
    let twice = serde_json::to_string(&toolbox.definitions()).unwrap();
    assert_eq!(once, twice);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A schema that has no `properties` or `required` of its own still gets a
/// well-formed injection — the keys are created rather than assumed.
#[test]
fn intent_injects_into_a_bare_object_schema() {
    let injected = with_intent(json!({
        "name": "bare",
        "input_schema": { "type": "object" }
    }));
    assert_eq!(
        injected["input_schema"]["properties"][INTENT]["type"],
        "string"
    );
    assert_eq!(injected["input_schema"]["required"], json!([INTENT]));
}

/// A tool that declares its own `intent` is left alone: the harness must not
/// overwrite the tool's description of its own field with the harness's.
#[test]
fn a_tool_declaring_its_own_intent_is_not_injected() {
    let own = json!({
        "name": "declares",
        "input_schema": {
            "type": "object",
            "properties": { INTENT: { "type": "number", "description": "the tool's own" } },
            "required": []
        }
    });
    let injected = with_intent(own.clone());
    assert_eq!(injected, own, "an existing intent must survive untouched");
}

/// The injected `intent` never reaches the tool. Proven through the dispatch path
/// rather than by inspecting `without_intent`: bash echoes its own environment, so a
/// tool that received an unexpected argument would surface as a real error result.
#[tokio::test]
async fn intent_is_stripped_before_the_tool_runs() {
    let box_ = Toolbox::bash_only();
    let result = done(
        box_.run(
            &call(
                bash::NAME,
                json!({"command": "echo ran", INTENT: "prove the strip"}),
            ),
            pending(),
            |_| {},
            |_| {},
        )
        .await,
    );
    assert!(!result.is_error, "got {:?}", result.content);
    assert!(result.content.contains("ran"), "got {:?}", result.content);
}

/// The strip is dispatch-only: the logged call keeps its `intent`, which is what
/// puts it on the wire and in reach of the harness's readers.
#[test]
fn the_logged_call_keeps_its_intent() {
    let with = call(bash::NAME, json!({"command": "true", INTENT: "  why  "}));
    assert_eq!(with.intent(), Some("why"), "trimmed, and still on the call");
    assert_eq!(call(bash::NAME, json!({"command": "true"})).intent(), None);
    // An empty or whitespace-only intent reads as absent rather than as a blank row.
    assert_eq!(
        call(bash::NAME, json!({"command": "true", INTENT: "   "})).intent(),
        None
    );
    // A non-string intent (a model emitting the wrong type) is absent, not a panic.
    assert_eq!(
        call(bash::NAME, json!({"command": "true", INTENT: 7})).intent(),
        None
    );
}

fn fs_server(cwd: PathBuf) -> McpServer<FsTools, fs_mcp::Tools> {
    McpServer::from_state(FsTools::embedded(cwd).unwrap(), &["set_working_directory"])
}

/// fs-mcp's schemas translate to the Anthropic `tools[]` shape. Uses
/// `tools_list()`, which is pure (schema generation) — hermetic, no store.
#[test]
fn fs_schemas_translate_to_anthropic_tool_defs() {
    let schemas = <fs_mcp::Tools as AsToolsList>::tools_list();
    let defs: Vec<Value> = schemas.iter().map(anthropic_def).collect();

    let names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    for expected in ["read", "search", "write", "list"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    for def in &defs {
        // Anthropic shape: snake_case `input_schema`, an object schema, and no
        // leftover MCP camelCase `inputSchema`.
        assert!(def.get("inputSchema").is_none(), "camelCase leaked: {def}");
        assert_eq!(def["input_schema"]["type"], json!("object"), "{def}");
    }
}

/// End-to-end execute path: read and list real files through the fs server.
/// Hermetic — `FsTools::embedded` uses an **in-memory** session store, so this
/// never touches the shared `~/.ai-tools` store; only the temp file is on disk.
#[test]
fn fs_read_and_list_roundtrip() {
    let dir = std::env::temp_dir().join(format!("eff-fsrt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hello.txt"), "fs-mcp lives").unwrap();

    let server = fs_server(dir.clone());

    // `read` resolves the relative path against the seeded session cwd.
    let read = server.run_call("read", &json!({ "paths": ["hello.txt"] }));
    assert!(!read.is_error, "read errored: {:?}", read.content);
    assert!(
        read.content.contains("fs-mcp lives"),
        "got {:?}",
        read.content
    );

    // `list` finds the file in the seeded directory.
    let list = server.run_call("list", &json!({ "path": "." }));
    assert!(!list.is_error, "list errored: {:?}", list.content);
    assert!(list.content.contains("hello.txt"), "got {:?}", list.content);

    // An unknown tool name routed here is a clean error, not a panic.
    let bad = server.run_call("read", &json!({ "wrong": true }));
    assert!(bad.is_error, "bad input should error: {:?}", bad.content);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A refused write reaches the model as an **error** result carrying the refusal
/// verbatim (docs/tools.md). The two halves are separable and both matter: the flag
/// says a write did not happen — a success-shaped refusal read as a success, and
/// distillation keeping only the first line sharpened that — and the body still
/// carries the retry token, which is the only way to confirm the same bytes without
/// re-sending them.
#[test]
fn a_refused_write_is_an_error_result_with_its_body_intact() {
    let dir = std::env::temp_dir().join(format!("eff-refuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("taken.txt"), "already here").unwrap();

    let server = fs_server(dir.clone());
    let refused = server.run_call(
        "write",
        &json!({ "path": "taken.txt", "contents": "new body" }),
    );
    assert!(refused.is_error, "got {:?}", refused.content);
    assert!(
        refused.content.contains("already exists"),
        "got {:?}",
        refused.content
    );
    assert!(
        refused.content.contains("preserved under `"),
        "the retry token survives the flip: {:?}",
        refused.content
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("taken.txt")).unwrap(),
        "already here"
    );

    // The zero-match case is the other side of the line: a search that found
    // nothing answered the question it was asked, so it stays a success.
    let empty = server.run_call("search", &json!({ "pattern": "no-such-string-here" }));
    assert!(!empty.is_error, "got {:?}", empty.content);
    assert!(
        empty.content.contains("No matches found"),
        "got {:?}",
        empty.content
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A rejected edit is an error result too, and the rejection report — the diff and
/// the syntax errors that explain what blocked it — is the error's body
/// (docs/tools.md).
#[test]
fn a_rejected_edit_is_an_error_result_with_its_report_intact() {
    let dir = std::env::temp_dir().join(format!("eff-reject-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = "fn main() {\n    println!(\"hi\");\n}\n";
    std::fs::write(dir.join("main.rs"), source).unwrap();

    let server: McpServer<SemanticEditTools, semantic_edit_mcp::Tools> = McpServer::from_state(
        SemanticEditTools::embedded(dir.clone()).unwrap(),
        &["set_working_directory"],
    );
    let rejected = server.run_call(
        "edit",
        &json!({
            "file_path": "main.rs",
            "operation": "replace",
            "anchor": "nothing in this file matches this anchor",
            "content": "fn other() {}",
        }),
    );
    assert!(rejected.is_error, "got {:?}", rejected.content);
    assert!(
        !rejected.content.is_empty() && !rejected.content.contains("Applied"),
        "got {:?}",
        rejected.content
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("main.rs")).unwrap(),
        source
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// semantic-edit-mcp wires up through the same generic `McpServer`: its
/// schemas translate, an embedded instance (in-memory stores + tree-sitter
/// language registry) advertises the edit tools, and bad input is a clean
/// error. Hermetic — no shared store, no real edit performed.
#[test]
fn semantic_edit_server_advertises_its_edit_tools() {
    let dir = std::env::temp_dir().join(format!("eff-se-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let server: McpServer<SemanticEditTools, semantic_edit_mcp::Tools> = McpServer::from_state(
        SemanticEditTools::embedded(dir.clone()).unwrap(),
        &["set_working_directory"],
    );

    for expected in ["edit", "retarget_edit", "undo_edit"] {
        assert!(
            server.handles(expected),
            "missing {expected} in {:?}",
            server.schemas.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }
    let bad = server.run_call("edit", &json!({ "nonsense": true }));
    assert!(bad.is_error, "bad input should error: {:?}", bad.content);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The fully-mounted toolbox must advertise **unique** tool names — both MCP
/// servers expose `set_working_directory`, which would collide on the wire (the
/// API rejects duplicate names). It's filtered; the real tools survive.
#[test]
fn mounted_toolbox_advertises_unique_tool_names() {
    let dir = std::env::temp_dir().join(format!("eff-uniq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let defs = Toolbox::bash_and_mcps(dir.clone(), None)
        .unwrap()
        .definitions();
    let mut names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(names, unique, "duplicate tool name on the wire");

    // `set_working_directory` collides; `move`/`delete` are curated out (bash
    // does them as well — docs/tools.md). The fork-friendly fs tools survive.
    for hidden in ["set_working_directory", "move", "delete"] {
        assert!(!names.contains(&hidden), "should be filtered: {names:?}");
    }
    for tool in [
        "bash",
        "read",
        "search",
        "write",
        "list",
        "edit",
        "undo_edit",
    ] {
        assert!(names.contains(&tool), "missing {tool} in {names:?}");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A model that reports no image input gets a refusal *before* the read
/// (docs/tools.md): `read_image` exists only to produce an image, so there is no
/// degraded form worth decoding a file to arrive at. The message names what would
/// work instead, because the sentence after a refusal is what decides whether the
/// model retries the same call.
#[tokio::test]
async fn read_image_refuses_when_the_model_takes_no_images() {
    let dir = std::env::temp_dir().join("efference-test-image-gate");
    std::fs::create_dir_all(&dir).unwrap();
    let box_ = Toolbox::new(vec![Provider::ReadImage(read_image::ReadImage {
        cwd: dir.clone(),
    })])
    .with_image_input(false);
    let result = done(
        box_.run(
            // A path that does not exist: reaching the filesystem at all would be
            // the failure this test is about.
            &call(read_image::NAME, json!({"path": "nope.png"})),
            pending(),
            |_| {},
            |_| {},
        )
        .await,
    );
    assert!(result.is_error);
    assert!(result.images.is_empty());
    assert!(
        result.content.contains("no image input"),
        "got {:?}",
        result.content
    );
    assert!(
        !result.content.contains("nope.png"),
        "the file was never read: {:?}",
        result.content
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The same gate applied to a result that merely *carries* images: an MCP tool
/// call is not about its image the way `read_image` is, so the call still
/// succeeds and the text half survives — what changes is that the bytes are
/// dropped instead of riding a wire nothing will look at, and the model is told
/// rather than left to infer it from silence (docs/tools.md).
#[test]
fn a_result_that_carries_images_keeps_its_text_and_sheds_the_bytes() {
    let carrying = || {
        Outcome::Done(RawResult::text("the page rendered").with_images(vec![
            crate::log::ResultImage {
                media_type: "image/webp".into(),
                data: "UklGRg==".into(),
                width: 10,
                height: 10,
            },
        ]))
    };
    let done_result = |o: Outcome| match o {
        Outcome::Done(r) => r,
        _ => panic!("a synchronous result"),
    };

    let kept = done_result(strip_unreadable_images(carrying(), true));
    assert_eq!(kept.images.len(), 1);
    assert_eq!(kept.content, "the page rendered");

    let stripped = done_result(strip_unreadable_images(carrying(), false));
    assert!(stripped.images.is_empty());
    assert!(!stripped.is_error, "the call still succeeded");
    assert!(stripped.content.starts_with("the page rendered"));
    assert!(
        stripped.content.contains("1 image block(s) withheld"),
        "got {:?}",
        stripped.content
    );
}

/// The name rule per dialect (docs/tools.md): one alphabet, two ceilings. The
/// namespaced wire form is what is judged, and the display form is what is reported.
#[test]
fn tool_names_are_judged_on_the_wire_form_per_dialect() {
    use crate::api::WireDialect;
    for dialect in [WireDialect::Messages, WireDialect::Responses] {
        assert_eq!(dialect.tool_name_problem("ferritin__get"), None);
        assert!(dialect.tool_name_problem("").is_some());
        let dotted = dialect
            .tool_name_problem("ferritin__admin.tools.list")
            .expect("a dot is outside the alphabet on both dialects");
        assert!(dotted.contains("'.'"), "{dotted}");
    }
    let long = format!("server__{}", "t".repeat(60));
    assert_eq!(long.len(), 68);
    assert_eq!(WireDialect::Messages.tool_name_problem(&long), None);
    let why = WireDialect::Responses
        .tool_name_problem(&long)
        .expect("68 characters exceeds the Responses ceiling of 64");
    assert!(why.contains("68") && why.contains("64"), "{why}");
}

/// Every embedded tool the harness mounts is legal on both dialects, and the report
/// names an offender in its display form with the reason beside it.
#[test]
fn the_embedded_toolbox_is_legal_on_both_dialects() {
    use crate::api::WireDialect;
    let cwd = std::env::temp_dir();
    let toolbox = Toolbox::bash_and_mcps(cwd, None).unwrap();
    assert_eq!(
        toolbox.illegal_names(WireDialect::Messages),
        Vec::<String>::new()
    );
    assert_eq!(
        toolbox.illegal_names(WireDialect::Responses),
        Vec::<String>::new()
    );
    let harness: Vec<String> = crate::agent::tool_definitions(&toolbox)
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_string))
        .collect();
    for name in harness {
        assert_eq!(
            WireDialect::Responses.tool_name_problem(&name),
            None,
            "{name}"
        );
    }
}
