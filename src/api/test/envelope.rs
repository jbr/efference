//! Tests for the request envelope's exact wire bytes, temperature mapping,
//! sorted tool-definition keys, parallel-tool-call scoping, the
//! prompt-cache key, and pinned-upstream rendering.

use super::*;

fn sample_messages() -> Vec<ApiMessage> {
    vec![
        ApiMessage {
            role: crate::project::Role::User,
            content: vec![ApiBlock::Text {
                text: "hello".into(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: crate::project::Role::Assistant,
            content: vec![ApiBlock::Text {
                text: "hi there".into(),
                cache_control: None,
            }],
        },
    ]
}

/// `render_messages_pretty` (the `wire --messages` JSON view) is exactly the
/// projected array — no `system`/`tools`/`model` envelope wrapped around it.
#[test]
fn render_messages_pretty_is_just_the_array() {
    let messages = sample_messages();
    let rendered = render_messages_pretty(&messages).unwrap();
    let value: Value = serde_json::from_str(&rendered).unwrap();
    assert!(value.is_array(), "expected a bare array, got {value}");
    assert_eq!(value, serde_json::to_value(&messages).unwrap());
    assert!(!rendered.contains("\"system\""));
    assert!(!rendered.contains("\"tools\""));
}

/// `render_messages_yaml` unfolds the same array as pseudo-YAML, with no
/// envelope keys either.
#[test]
fn render_messages_yaml_is_just_the_array() {
    let messages = sample_messages();
    let rendered = render_messages_yaml(&messages).unwrap();
    assert!(rendered.contains("hello"));
    assert!(rendered.contains("hi there"));
    assert!(!rendered.contains("system:"));
    assert!(!rendered.contains("tools:"));
}

/// The session-level 0–100 integer temperature maps to the provider's
/// native float range. 50 is mid-scale and lands at 1.0 on both providers
/// (matching both defaults), 0 is the floor, and 100 is the ceiling.
#[test]
fn map_temperature_maps_the_integer_to_provider_native_range() {
    // Anthropic: 0.0..1.0
    let a = Provider::Anthropic;
    assert_eq!(a.map_temperature(0), 0.0);
    assert_eq!(a.map_temperature(50), 0.5);
    assert_eq!(a.map_temperature(100), 1.0);

    // OpenRouter: 0.0..2.0
    let o = Provider::OpenRouter;
    assert_eq!(o.map_temperature(0), 0.0);
    assert_eq!(o.map_temperature(50), 1.0);
    assert_eq!(o.map_temperature(100), 2.0);
}

/// When `temperature` is `None`, the serialized `Request` omits the field
/// entirely — the JSON key is absent, not null. Omitting optional fields
/// vs. explicitly sending defaults produces different cache keys, so this
/// is the cache-safe path for sessions that don't set `--temperature`.
#[test]
fn request_omits_temperature_when_none() {
    let request = Request {
        model: "test",
        max_tokens: 1024,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Disabled,
        reasoning: None,
        output_config: None,
        temperature: None,
        upstream: None,
        prompt_cache_key: None,
        messages: &[],
        cache_diagnostics: None,
        stream: false,
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        !json.contains("temperature"),
        "temperature field absent when None"
    );
}

/// When `temperature` is `Some(f)`, the serialized `Request` includes the
/// float value. This is the cache-affecting path — the value is part of the
/// request body hash.
#[test]
fn request_includes_temperature_when_set() {
    let request = Request {
        model: "test",
        max_tokens: 1024,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Disabled,
        reasoning: None,
        output_config: None,
        temperature: Some(0.7),
        upstream: None,
        prompt_cache_key: None,
        messages: &[],
        cache_diagnostics: None,
        stream: false,
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"temperature\":0.7"),
        "temperature: 0.7 present"
    );
}

/// **The whole request envelope, pinned to its exact bytes.**
///
/// The `messages` pins in `src/project/test.rs` cover the projection's own
/// output; the provider matches the **whole body**, and the gap between those
/// two facts has already cost a silent cache break — a tools-array mutation
/// moved the prefix without failing either projection pin
/// (docs/log-and-projection.md). This pin closes that gap: every field ahead of
/// the tail, in serialization order, with `messages` present only far enough to
/// fix its position relative to the rest.
///
/// If this fails, the change under test moves the cached prefix in production.
/// That is the point — update the expectation deliberately, having decided the
/// move is worth a cold cache for every live session, rather than reflexively.
///
/// Adding a field is caught too: a new key lands somewhere in these bytes.
#[test]
fn request_envelope_matches_expected_wire_bytes() {
    use crate::project::{ApiBlock, ApiMessage, CacheControl, Role};

    let tools = [serde_json::json!({
        "name": "bash",
        "description": "run a command",
        "input_schema": {"type": "object"},
    })];
    let messages = [ApiMessage {
        role: Role::User,
        content: vec![ApiBlock::Text {
            text: "hello".into(),
            cache_control: Some(CacheControl::Ephemeral),
        }],
    }];
    let request = Request {
        model: "claude-opus-5",
        max_tokens: 4096,
        system: "you are a harness",
        tools: &tools,
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Disabled,
        reasoning: None,
        output_config: None,
        temperature: Some(0.7),
        upstream: None,
        prompt_cache_key: None,
        messages: &messages,
        cache_diagnostics: None,
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"claude-opus-5","max_tokens":4096,"system":"you are a harness","#,
        r#""tools":[{"description":"run a command","input_schema":{"type":"object"},"name":"bash"}],"#,
        r#""tool_choice":{"type":"auto"},"#,
        r#""thinking":{"type":"disabled"},"temperature":0.7,"#,
        r#""messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]}],"#,
        r#""stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// The same envelope with an effort set — the other side of the pin above, which
/// holds because `output_config` is absent when the session sets none. This fixes
/// the with-effort bytes: the object is `{"effort":"<level>"}` nested under
/// `output_config`, and it sits **between `thinking` and `temperature`**, so the
/// two reasoning knobs are adjacent (docs/transport.md).
///
/// Same rule as the pin above: a session's effort is boot-time and frozen, so
/// these bytes never change under a live cache — but if this test fails, the
/// change moves the prefix of every session that sets an effort, so update it
/// deliberately.
#[test]
fn with_effort_envelope_matches_expected_wire_bytes() {
    use crate::project::{ApiBlock, ApiMessage, CacheControl, Role};

    let tools = [serde_json::json!({
        "name": "bash",
        "description": "run a command",
        "input_schema": {"type": "object"},
    })];
    let messages = [ApiMessage {
        role: Role::User,
        content: vec![ApiBlock::Text {
            text: "hello".into(),
            cache_control: Some(CacheControl::Ephemeral),
        }],
    }];
    let request = Request {
        model: "claude-opus-5",
        max_tokens: 4096,
        system: "you are a harness",
        tools: &tools,
        tool_choice: ToolChoice::auto(),
        thinking: THINKING_ADAPTIVE,
        reasoning: None,
        output_config: Some(OutputConfig {
            effort: "xhigh".to_string(),
        }),
        temperature: Some(0.7),
        upstream: None,
        prompt_cache_key: None,
        messages: &messages,
        cache_diagnostics: None,
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"claude-opus-5","max_tokens":4096,"system":"you are a harness","#,
        r#""tools":[{"description":"run a command","input_schema":{"type":"object"},"name":"bash"}],"#,
        r#""tool_choice":{"type":"auto"},"#,
        r#""thinking":{"type":"adaptive","display":"summarized"},"#,
        r#""output_config":{"effort":"xhigh"},"temperature":0.7,"#,
        r#""messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]}],"#,
        r#""stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// The pinned envelope above is what an **Anthropic** session sends, and adding
/// `prompt_cache_key` did not move a byte of it — the field is `None` there and
/// skips serialization entirely. This pins the other half: an OpenRouter session
/// carries the key, and it sits between `provider` and `messages`, so the two
/// provider-conditional fields are adjacent rather than straddling the long
/// `messages` block. Both are per-session constants, so neither can toggle
/// mid-session and move a live cached prefix (docs/transport.md).
#[test]
fn openrouter_envelope_carries_the_prompt_cache_key() {
    let pin = Upstream::pinned(vec!["google-ai-studio".into()]).expect("a non-empty order");
    let request = Request {
        model: "google/gemini-3.7-flash",
        max_tokens: 1,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Omit,
        reasoning: None,
        output_config: None,
        temperature: None,
        upstream: Some(&pin),
        prompt_cache_key: Some("work-0123456789abcdef"),
        messages: &[],
        cache_diagnostics: None,
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"google/gemini-3.7-flash","max_tokens":1,"tools":[],"#,
        r#""tool_choice":{"type":"auto"},"#,
        r#""provider":{"order":["google-ai-studio"],"allow_fallbacks":false},"#,
        r#""prompt_cache_key":"work-0123456789abcdef","#,
        r#""messages":[],"stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// **The cache-diagnostics object, pinned in both of its shapes.**
///
/// Its position is the pin's point: the object sits *after* `messages`, because its
/// value changes on every request of a chain and everything ahead of the last
/// breakpoint has to stay byte-stable (docs/log-and-projection.md). It is the
/// envelope's only per-request field, and that is why it cannot sit with the
/// per-session constants.
///
/// The first request of a chain carries an explicit `null` — the opt-in with nothing
/// to compare against, which is what makes the API store a fingerprint for it — and
/// every request after it names the previous response. Measured against the live API:
/// a pair of requests differing only in this value read the whole 15k-token prefix on
/// the second, so the field is not part of what the provider's cache matches.
#[test]
fn the_cache_diagnostics_object_sits_after_messages() {
    let messages = [ApiMessage {
        role: crate::project::Role::User,
        content: vec![ApiBlock::Text {
            text: "hello".into(),
            cache_control: None,
        }],
    }];
    let render = |previous_message_id| {
        serde_json::to_string(&Request {
            model: "m",
            max_tokens: 1,
            system: "",
            tools: &[],
            tool_choice: ToolChoice::auto(),
            thinking: Thinking::Omit,
            reasoning: None,
            output_config: None,
            temperature: None,
            upstream: None,
            prompt_cache_key: None,
            messages: &messages,
            cache_diagnostics: Some(CacheDiagnostics {
                previous_message_id,
            }),
            stream: true,
        })
        .expect("serializes")
    };

    let opening = concat!(
        r#"{"model":"m","max_tokens":1,"tools":[],"#,
        r#""tool_choice":{"type":"auto"},"#,
        r#""messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"#,
    );
    // The first request of a chain: the opt-in, with an explicit null rather than an
    // absent field, which is the difference between "store a fingerprint" and "store
    // nothing".
    assert_eq!(
        render(None),
        format!(r#"{opening}"diagnostics":{{"previous_message_id":null}},"stream":true}}"#)
    );
    // Every request after it names the response it should be compared against.
    assert_eq!(
        render(Some("msg_01Xyz".to_string())),
        format!(r#"{opening}"diagnostics":{{"previous_message_id":"msg_01Xyz"}},"stream":true}}"#)
    );
}

/// The envelope's two omit-when-unset fields drop out **entirely** rather than
/// serializing null or a default: an absent key and a key with a default value
/// are different bytes, so this is the shape a session that sets neither
/// `--temperature` nor a system prompt actually sends.
#[test]
fn request_envelope_omits_unset_fields_entirely() {
    let request = Request {
        model: "m",
        max_tokens: 1,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Omit,
        reasoning: None,
        output_config: None,
        temperature: None,
        upstream: None,
        prompt_cache_key: None,
        messages: &[],
        cache_diagnostics: None,
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"m","max_tokens":1,"tools":[],"#,
        r#""tool_choice":{"type":"auto"},"#,
        r#""messages":[],"stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// Tool definitions serialize with **sorted keys**, whatever order they were
/// declared in. This is what makes `tools[]` byte-stable across processes:
/// `Value`'s map sorts, where a `HashMap` would randomize per process and move
/// the prefix on every restart (docs/log-and-projection.md).
#[test]
fn tool_definition_keys_serialize_sorted() {
    let declared = serde_json::json!({"zeta": 1, "alpha": 2, "input_schema": 3});
    assert_eq!(
        serde_json::to_string(&declared).unwrap(),
        r#"{"alpha":2,"input_schema":3,"zeta":1}"#,
    );
}

/// **Two tool calls in one response stay distinguishable on the live sink.** Blocks
/// stream sequentially, so an unscoped sink cannot tell the second call's input from the
/// first's continuing: a client holding one slot per kind resets it when the second
/// `ToolStart` arrives and the first call's arriving arguments vanish from the display.
/// Carrying `index` — and the `tool_use` id the eventual result keys on — is what lets a
/// client bind each fragment to the call it belongs to (docs/control-api.md).
#[test]
fn parallel_tool_calls_are_scoped_to_their_blocks() {
    let seen = live(&[
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_a","name":"read","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"paths\""}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "content_block_start",
            r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_b","name":"bash","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\""}}"#,
        ),
        ("content_block_stop", r#"{"index":1}"#),
    ]);
    assert_eq!(
        seen,
        vec![
            Streamed::ToolStart {
                index: 0,
                id: "toolu_a".into(),
                name: "read".into()
            },
            Streamed::ToolInput {
                index: 0,
                fragment: "{\"paths\"".into()
            },
            Streamed::ToolStart {
                index: 1,
                id: "toolu_b".into(),
                name: "bash".into()
            },
            Streamed::ToolInput {
                index: 1,
                fragment: "{\"command\"".into()
            },
        ],
        "each call's fragments must name the block they belong to"
    );
}

/// A pinned upstream serializes as OpenRouter's `provider` object, and an
/// unpinned one leaves the wire **byte-identical** to before the field existed.
/// The second half is the cache-safety claim: adding a request field that
/// serializes unconditionally would move every session's prefix
/// (docs/log-and-projection.md), so `None` must skip entirely rather than emit
/// `null`. The rename is pinned too — the type is `Upstream` because `Provider`
/// already means anthropic-vs-openrouter, and only the wire says `provider`.
#[test]
fn a_pinned_upstream_renders_as_the_provider_object() {
    let pin = Upstream::pinned(vec!["google-ai-studio".into()]).expect("non-empty order");
    let json = serde_json::to_string(&pin).expect("serializes");
    assert_eq!(
        json,
        r#"{"order":["google-ai-studio"],"allow_fallbacks":false}"#
    );

    // An empty order is not an empty object on the wire — it is no pin at all.
    assert_eq!(Upstream::pinned(Vec::new()), None);

    let tools: [Value; 0] = [];
    let messages = [ApiMessage {
        role: crate::project::Role::User,
        content: vec![ApiBlock::Text {
            text: "hi".into(),
            cache_control: None,
        }],
    }];
    let render = |upstream| {
        serde_json::to_string(&Request {
            model: "m",
            max_tokens: 1,
            system: "",
            tools: &tools,
            tool_choice: ToolChoice::auto(),
            thinking: Thinking::Omit,
            reasoning: None,
            output_config: None,
            temperature: None,
            upstream,
            prompt_cache_key: None,
            messages: &messages,
            cache_diagnostics: None,
            stream: true,
        })
        .expect("serializes")
    };
    assert!(!render(None).contains("provider"));
    assert!(render(Some(&pin)).contains(r#""provider":{"order":["google-ai-studio""#));
}
