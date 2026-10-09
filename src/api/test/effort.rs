//! Tests for declared effort levels and thinking types, the effort/thinking
//! coupling checks, the thinking matrix, the thinking-off switch, and that the
//! keepalive sends the turn's thinking and effort.

use super::*;

/// Anthropic's `GET /v1/models/{id}` declares the `thinking` types and the
/// effort levels a model takes (docs/transport.md). Live API bodies:
/// Haiku 5.5 takes adaptive and `disabled` and every level; Sonnet 5.5 refuses
/// `disabled` (its 400 names `between_tools`); Haiku 4.5 takes no adaptive and no
/// effort at all.
#[test]
fn anthropic_metadata_declares_thinking_and_effort() {
    use super::thinking::ThinkingOff;
    let haiku_5_5 = r#"{"type":"model","id":"claude-haiku-5-5","display_name":"Claude Haiku 5.5","created_at":"2026-10-07T18:00:00Z","line":"haiku","max_input_tokens":1000000,"max_tokens":128000,"capabilities":{"batch":{"supported":true},"citations":{"supported":true},"code_execution":{"supported":true},"context_management":{"supported":true,"clear_tool_uses_20250919":{"supported":true},"clear_thinking_20251015":{"supported":true},"compact_20260112":{"supported":true}},"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},"high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":true}},"image_input":{"supported":true},"pdf_input":{"supported":true},"server_tools":{"supported":true,"code_execution":{"supported":true},"web_search":{"supported":true}},"structured_outputs":{"supported":true},"thinking":{"supported":true,"types":{"enabled":{"supported":false},"adaptive":{"supported":true},"disabled":{"supported":true}}}},"lifecycle":"active","deprecated_at":null,"retires_at":null}"#;
    let caps = Provider::Anthropic
        .parse_capabilities(haiku_5_5, "claude-haiku-5-5", &[])
        .unwrap();
    assert_eq!(
        caps.thinking,
        ThinkingSupport {
            adaptive: true,
            off: ThinkingOff::Disabled
        }
    );
    // In the request's own order, whatever order the keys arrived in.
    assert_eq!(
        caps.supported_efforts.as_deref(),
        Some(&["low", "medium", "high", "xhigh", "max"].map(String::from)[..])
    );
    assert!(caps.validates_efforts);
    assert!(caps.check_effort("xhigh", "claude-haiku-5-5").is_ok());
    assert!(caps.check_effort("ultra", "claude-haiku-5-5").is_err());

    let sonnet_5_5 = r#"{"id":"claude-sonnet-5-5","max_input_tokens":1000000,"max_tokens":128000,
        "capabilities":{"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},
        "high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":true}},
        "thinking":{"supported":true,"types":{"enabled":{"supported":false},
        "adaptive":{"supported":true},"disabled":{"supported":false}}}}}"#;
    let caps = Provider::Anthropic
        .parse_capabilities(sonnet_5_5, "claude-sonnet-5-5", &[])
        .unwrap();
    assert_eq!(
        caps.thinking,
        ThinkingSupport {
            adaptive: true,
            off: ThinkingOff::BetweenTools
        }
    );

    let haiku_4_5 = r#"{"id":"claude-haiku-4-5-20251001","max_input_tokens":200000,"max_tokens":64000,
        "capabilities":{"effort":{"supported":false,"low":{"supported":false},"medium":{"supported":false},
        "high":{"supported":false},"xhigh":null,"max":{"supported":false}},
        "thinking":{"supported":true,"types":{"enabled":{"supported":true},
        "adaptive":{"supported":false},"disabled":{"supported":true}}}}}"#;
    let caps = Provider::Anthropic
        .parse_capabilities(haiku_4_5, "claude-haiku-4-5", &[])
        .unwrap();
    assert_eq!(
        caps.thinking,
        ThinkingSupport {
            adaptive: false,
            off: ThinkingOff::Disabled
        }
    );
    // Declared, and empty: every configured level is refused at boot.
    assert_eq!(caps.supported_efforts, Some(vec![]));
    let why = caps.check_effort("high", "claude-haiku-4-5").unwrap_err();
    assert!(why.contains("takes no effort level"), "{why}");

    // No `capabilities` object declares nothing on either axis.
    let bare = r#"{"id":"m","max_input_tokens":200000,"max_tokens":64000}"#;
    let caps = Provider::Anthropic
        .parse_capabilities(bare, "m", &[])
        .unwrap();
    assert_eq!(caps.thinking, ThinkingSupport::UNKNOWN);
    assert_eq!(caps.supported_efforts, None);
    assert!(!caps.validates_efforts);
    assert_eq!(
        Provider::Anthropic
            .effort_levels_url("claude-haiku-4-5")
            .unwrap(),
        None
    );
}

/// The thinking a request carries comes from the model's metadata alone
/// (docs/transport.md): an `auto` turn thinks adaptively where the model takes
/// it; a forced choice, a model without adaptive, and a client built without
/// thinking all take the model's spelling of off. No model id is consulted.
#[test]
fn thinking_follows_the_models_metadata() {
    use super::thinking::{Thinking, ThinkingOff};
    let auto = ToolChoice::auto();
    let forced = ToolChoice::force("retain");
    let support = |adaptive, off| ThinkingSupport { adaptive, off };
    for off in [
        ThinkingOff::Disabled,
        ThinkingOff::BetweenTools,
        ThinkingOff::Omit,
    ] {
        let spelled = match off {
            ThinkingOff::Disabled => Thinking::Disabled,
            ThinkingOff::BetweenTools => Thinking::BetweenTools,
            ThinkingOff::Omit => Thinking::Omit,
        };
        assert!(matches!(
            Thinking::for_request(&auto, support(true, off), false),
            Thinking::Adaptive { .. }
        ));
        assert_eq!(
            Thinking::for_request(&forced, support(true, off), false),
            spelled
        );
        assert_eq!(
            Thinking::for_request(&auto, support(true, off), true),
            spelled
        );
        assert_eq!(
            Thinking::for_request(&auto, support(false, off), false),
            spelled
        );
    }
    assert_eq!(
        Thinking::for_request(&auto, ThinkingSupport::UNKNOWN, false),
        Thinking::Omit
    );
}

/// The wire spellings: `between_tools` is its own type, and an `Omit` never
/// reaches the wire — the offline dev view, which has no metadata to consult,
/// renders the request with no `thinking` key at all.
#[test]
fn thinking_spellings_on_the_wire() {
    use super::thinking::Thinking;
    assert_eq!(
        serde_json::to_string(&Thinking::BetweenTools).unwrap(),
        r#"{"type":"between_tools"}"#
    );
    assert_eq!(
        serde_json::to_string(&Thinking::Disabled).unwrap(),
        r#"{"type":"disabled"}"#
    );
    let offline =
        render_request_pretty("claude-fable-5", "", &[], ToolChoice::auto(), &[]).unwrap();
    assert!(!offline.contains("thinking"), "{offline}");
}

/// The OpenRouter model entry's `reasoning` block is the effort metadata.
/// Live shape (2026-09-04): GLM 5.3 flash names its levels and its default;
/// `default_enabled` is ignored; `mandatory` is read (qwen/qwen3.8-flash reports
/// `false`, glm-5.3-flash `true`).
#[test]
fn openrouter_model_entry_yields_effort_levels() {
    let body = r#"{"data": {"id": "z-ai/glm-5.3-flash", "reasoning": {
        "mandatory": true, "default_enabled": true,
        "supported_efforts": ["max", "high", "low"], "default_effort": "max"
    }}}"#;
    let efforts = Provider::OpenRouter.parse_model_entry(body).unwrap();
    assert_eq!(
        efforts.supported,
        Some(vec!["max".into(), "high".into(), "low".into()])
    );
    assert_eq!(efforts.default.as_deref(), Some("max"));
    // `mandatory` decides how a client says thinking-off (docs/transport.md).
    assert!(efforts.mandatory);
}

/// An absent block — a non-reasoning model (live: `openai/gpt-4.1-mini`,
/// 2026-09-04) — declares nothing, and an empty list is the same state:
/// neither affirms a level. `default_effort` is read independently, so
/// incoherent metadata (a default with no list) still reaches the boot line
/// rather than disappearing.
#[test]
fn openrouter_model_entry_declaring_nothing_reads_as_none() {
    let absent = Provider::OpenRouter
        .parse_model_entry(r#"{"data": {"id": "openai/gpt-4.1-mini"}}"#)
        .unwrap();
    assert_eq!(absent.supported, None);
    assert_eq!(absent.default, None);
    assert!(!absent.mandatory);

    let empty = Provider::OpenRouter
        .parse_model_entry(
            r#"{"data": {"id": "x",
                "reasoning": {"supported_efforts": [], "default_effort": "high"}}}"#,
        )
        .unwrap();
    assert_eq!(empty.supported, None);
    assert_eq!(empty.default.as_deref(), Some("high"));
}

/// A level outside efference's five-level spelling survives as a string: the
/// metadata's vocabulary is what the boot check tests membership against, not
/// ours — a model that invents `xmax` is validated, not rejected
/// (docs/transport.md).
#[test]
fn openrouter_model_entry_efforts_are_not_vocabulary_checked() {
    let efforts = Provider::OpenRouter
        .parse_model_entry(r#"{"data": {"reasoning": {"supported_efforts": ["xmax", "low-mid"]}}}"#)
        .unwrap();
    assert_eq!(
        efforts.supported,
        Some(vec!["xmax".into(), "low-mid".into()])
    );
    assert_eq!(efforts.default, None);
}

/// OpenRouter's effort-levels route is the singular model entry; Anthropic
/// has no second route and answers `None`. Same author/slug requirement as
/// the ceilings route.
#[test]
fn effort_levels_url_per_provider() {
    assert_eq!(
        Provider::OpenRouter
            .effort_levels_url("z-ai/glm-5.3-flash")
            .unwrap()
            .as_deref(),
        Some("https://openrouter.ai/api/v1/model/z-ai/glm-5.3-flash")
    );
    assert_eq!(
        Provider::Anthropic
            .effort_levels_url("claude-haiku-4-5")
            .unwrap(),
        None
    );
    assert!(
        Provider::OpenRouter
            .effort_levels_url("claude-haiku-4-5")
            .is_err()
    );
}

/// The levels that are legal only with thinking on: exactly the two spellings
/// in [`EFFORTS_REQUIRING_THINKING`], known because the provider answers the
/// pairing with a 400. Any other spelling has no known coupling.
#[test]
fn the_two_spellings_requiring_thinking_are_those_two() {
    assert_eq!(
        super::thinking::EFFORTS_REQUIRING_THINKING,
        ["xhigh", "max"]
    );
}

/// The one coupling between the level axis and the type axis, checked at boot:
/// `xhigh`/`max` with thinking off is a 400 from the provider, so the session is
/// refused at startup with a message naming both halves (docs/transport.md).
/// Every other pairing — including a session that sets no effort at all, and a
/// level with no known coupling — passes.
#[test]
fn thinking_off_rejects_the_two_levels_that_need_thinking() {
    use crate::api::check_effort_thinking as check;
    let why = check(Some("max"), "m", true).unwrap_err();
    assert!(why.contains("max"), "names the level: {why}");
    assert!(why.contains("m's metadata"), "names the model: {why}");
    assert!(check(Some("xhigh"), "m", true).is_err());
    assert!(check(Some("high"), "m", true).is_ok());
    // A spelling outside the known vocabulary rides through — its coupling,
    // if any, is the provider's to answer, not a local list's to guess.
    assert!(check(Some("xmax"), "m", true).is_ok());
    assert!(check(None, "m", true).is_ok());
    for effort in ["low", "medium", "high"] {
        assert!(check(Some(effort), "m", false).is_ok());
    }
}

/// The boot membership check (docs/transport.md): a configured level rides the
/// wire only when the metadata names it; the declared default, when present,
/// points at the unset path in the rejection.
#[test]
fn check_effort_rejects_unnamed_levels() {
    let caps = caps_with_effort_levels(Some(&["max", "high", "low"]), Some("max"));
    assert!(caps.check_effort("high", "z-ai/glm-5.3-flash").is_ok());

    let why = caps
        .check_effort("medium", "z-ai/glm-5.3-flash")
        .unwrap_err();
    assert!(why.contains("medium"), "names the level: {why}");
    assert!(
        why.contains("max, high, low"),
        "names the levels as the metadata spells them: {why}"
    );
    assert!(why.contains("silently"), "names the mapping hazard: {why}");
    assert!(
        why.contains("provider default: max"),
        "names the unset path: {why}"
    );

    // No declared default → the message doesn't invent one.
    let caps = caps_with_effort_levels(Some(&["xhigh", "high"]), None);
    let why = caps.check_effort("low", "m").unwrap_err();
    assert!(!why.contains("default"), "{why}");
}

/// A source that declared levels refuses a level outside them, and a source
/// that declared none does not check at all: nothing affirmed a level to check
/// against, so the configured one passes through for the provider to accept or
/// refuse (docs/transport.md).
#[test]
fn a_declared_list_bounds_the_level_and_an_undeclared_source_does_not() {
    // Declared: the membership test is exact, and the refusal names the levels
    // and the provider's default.
    let caps = caps_with_effort_levels(Some(&["low", "high"]), Some("high"));
    assert!(caps.check_effort("high", "m").is_ok());
    let why = caps.check_effort("medium", "m").unwrap_err();
    assert!(why.contains("not among the levels m reports"), "{why}");
    assert!(why.contains("low, high"), "{why}");
    assert!(why.contains("provider default: high"), "{why}");

    // Undeclared: nothing is checked, whatever the configured level is.
    let undeclared = caps_with_effort_levels(None, None);
    assert!(undeclared.check_effort("ultra", "m").is_ok());
    assert!(undeclared.check_effort("high", "m").is_ok());
}

/// Test fixture: a [`ModelCapabilities`] with the effort metadata set and the
/// other fields at neutral values.
fn caps_with_effort_levels(
    levels: Option<&[&str]>,
    default: Option<&str>,
) -> crate::api::ModelCapabilities {
    crate::api::ModelCapabilities {
        context_length: Some(200_000),
        prompt_cache: PromptCachePolicy::Explicit,
        supported_efforts: levels.map(|l| l.iter().map(|s| s.to_string()).collect()),
        default_effort: default.map(String::from),
        // A fixture that declares levels is a source that declared them, which
        // is what makes the boot check apply.
        validates_efforts: levels.is_some(),
        ..crate::api::ModelCapabilities::unknown()
    }
}

/// The boot check's `thinking_off` input: an `auto` turn sends an explicit off
/// exactly when the model takes no adaptive and its off is spelled explicitly.
/// Omission leaves the endpoint to decide, so the check stays out of it.
#[test]
fn auto_turns_send_thinking_off_only_without_adaptive_and_when_spelled() {
    use super::thinking::ThinkingOff;
    let support = |adaptive, off| ThinkingSupport { adaptive, off };
    assert!(support(false, ThinkingOff::Disabled).auto_turns_send_thinking_off());
    assert!(support(false, ThinkingOff::BetweenTools).auto_turns_send_thinking_off());
    assert!(!support(false, ThinkingOff::Omit).auto_turns_send_thinking_off());
    assert!(!support(true, ThinkingOff::Disabled).auto_turns_send_thinking_off());
    assert!(!support(true, ThinkingOff::BetweenTools).auto_turns_send_thinking_off());
}

/// The keepalive sends the turn's envelope, over a real socket: the same
/// thinking and the same effort, differing only in `max_tokens` and `stream`
/// (docs/transport.md). On Anthropic a ping whose thinking differs from the
/// turn's misses the message-level cache and writes a fresh entry instead of
/// refreshing the live one.
///
/// A client that was never given an effort — the spotter's — sends the field on
/// neither path, which the second half asserts.
#[tokio::test]
async fn the_keepalive_sends_the_turns_thinking_and_effort() {
    use std::io::{Read, Write};

    /// One request/response over a fresh socket; returns the request bytes the
    /// client sent. `sse` picks the canned reply: an empty stream for `complete`,
    /// a usage-only JSON body for `keepalive`.
    async fn round_trip(provider: Provider, effort: Option<&str>, sse: bool) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            // Read until the whole POST body has arrived: one `read` can return the
            // headers alone, and the assertion below is about the body.
            let mut request = String::new();
            let mut buf = [0u8; 16384];
            loop {
                let read = sock.read(&mut buf).unwrap();
                request.push_str(&String::from_utf8_lossy(&buf[..read]));
                let complete = request.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    head.to_ascii_lowercase()
                        .split("\r\n")
                        .find_map(|line| line.strip_prefix("content-length: ").map(str::to_string))
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .is_some_and(|n| body.len() >= n)
                });
                if read == 0 || complete {
                    break;
                }
            }
            let (content_type, body) = if sse {
                (
                    "text/event-stream",
                    "event: message_start\ndata: {\"message\":{\"id\":\"gen-1\",\
                     \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                     event: data\ndata: [DONE]\n\n"
                        .to_string(),
                )
            } else {
                (
                    "application/json",
                    "{\"id\":\"msg-1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\
                     \"output_tokens\":0}}"
                        .to_string(),
                )
            };
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\n\
                     content-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
            request
        });

        let endpoint = Endpoint {
            provider,
            url: Some(format!("http://{addr}/v1/messages")),
            auth_header: "x-api-key".into(),
            auth_value: "k".into(),
            session_id: None,
            extra_headers: Vec::new(),
            base: None,
        };
        let client = Client::new(endpoint, "claude-opus-5", "sys", None)
            .with_thinking(ThinkingSupport {
                adaptive: true,
                off: super::thinking::ThinkingOff::Disabled,
            })
            .with_effort(effort.map(String::from));
        if sse {
            client
                .complete(&[], &[], ToolChoice::auto(), |_| {})
                .await
                .expect("the canned stream assembles");
        } else {
            client
                .keepalive(&[], &[], ToolChoice::auto())
                .await
                .expect("the canned body parses");
        }
        server.join().unwrap()
    }

    /// The request body as JSON, without the two keys a ping differs by.
    fn envelope(request: &str) -> Value {
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        let mut body: Value = serde_json::from_str(body).unwrap();
        let object = body.as_object_mut().unwrap();
        object.remove("max_tokens");
        object.remove("stream");
        body
    }
    let turn = round_trip(Provider::Anthropic, Some("max"), true).await;
    assert!(
        turn.contains(r#""output_config":{"effort":"max"}"#),
        "the turn envelope carries it: {turn}"
    );
    assert!(turn.contains(r#""type":"adaptive""#), "{turn}");
    let ping = round_trip(Provider::Anthropic, Some("max"), false).await;
    assert_eq!(envelope(&ping), envelope(&turn));

    // The shape of the spotter's client: never given an effort, so neither path
    // carries one.
    let turn = round_trip(Provider::Anthropic, None, true).await;
    assert!(!turn.contains("output_config"), "{turn}");
    let ping = round_trip(Provider::Anthropic, None, false).await;
    assert!(!ping.contains("output_config"), "{ping}");

    // The output cap is the provider's (docs/transport.md): Anthropic honours zero;
    // OpenRouter reads zero as unset, so its ping sends a positive cap.
    let max_tokens = |request: &str| {
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        serde_json::from_str::<Value>(body).unwrap()["max_tokens"].clone()
    };
    assert_eq!(max_tokens(&ping), 0);
    let ping = round_trip(Provider::OpenRouter, None, false).await;
    assert_eq!(max_tokens(&ping), 16);
}

/// `Client::without_thinking` takes the model's spelling of off on every turn,
/// adaptive or not (docs/redaction-fork.md, docs/transport.md), and on OpenRouter
/// an omitted `thinking` is said in OpenRouter's own word, `reasoning: {enabled:
/// false}`. A client that never asked sends nothing extra.
#[test]
fn thinking_off_takes_the_models_spelling() {
    use super::thinking::{Thinking, ThinkingOff};
    let openrouter =
        Endpoint::from_credentials(Provider::OpenRouter, "sk-or-v1-abc".into(), None, None)
            .unwrap();
    let anthropic =
        Endpoint::from_credentials(Provider::Anthropic, "sk-ant-xyz".into(), None, None).unwrap();
    let auto = ToolChoice::auto();
    let support = |off| ThinkingSupport {
        adaptive: true,
        off,
    };
    let sent = |client: &Client, off| {
        let thinking = Thinking::for_request(&auto, support(off), client.thinking_off);
        let reasoning = client
            .reasoning_off(thinking)
            .map(|r| serde_json::to_string(&r).unwrap());
        (thinking, reasoning)
    };
    let off =
        |endpoint: &Endpoint| Client::new(endpoint.clone(), "m", "sys", None).without_thinking();
    // Mandatory or undeclared reasoning on OpenRouter: omitted, and the switch rides.
    assert_eq!(
        sent(&off(&openrouter), ThinkingOff::Omit),
        (Thinking::Omit, Some(r#"{"enabled":false}"#.into()))
    );
    // An explicit spelling says it alone.
    assert_eq!(
        sent(&off(&openrouter), ThinkingOff::Disabled),
        (Thinking::Disabled, None)
    );
    assert_eq!(
        sent(&off(&anthropic), ThinkingOff::BetweenTools),
        (Thinking::BetweenTools, None)
    );
    // Anthropic has no `reasoning` field to send.
    assert_eq!(
        sent(&off(&anthropic), ThinkingOff::Omit),
        (Thinking::Omit, None)
    );
    let unasked = Client::new(openrouter, "m", "sys", None);
    assert!(matches!(
        sent(&unasked, ThinkingOff::Omit),
        (Thinking::Adaptive { .. }, None)
    ));
}
