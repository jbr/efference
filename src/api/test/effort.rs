//! Tests for declared effort levels, the effort/thinking coupling checks,
//! the thinking matrix, the thinking-off switch, and that only the shared
//! turn envelope carries the effort.

use super::*;

/// The Anthropic metadata path declares no effort levels — not on its single
/// route, and there is no second route — so a configured level passes through
/// to the provider's own validation (docs/transport.md).
#[test]
fn anthropic_declares_no_effort_levels() {
    let body = r#"{
        "id": "claude-haiku-4-5",
        "display_name": "Claude Haiku 4.5",
        "max_input_tokens": 200000,
        "max_tokens": 64000
    }"#;
    let caps = Provider::Anthropic
        .parse_capabilities(body, "m", &[])
        .unwrap();
    assert_eq!(caps.supported_efforts, None);
    assert_eq!(caps.default_effort, None);
    assert_eq!(
        Provider::Anthropic
            .effort_levels_url("claude-haiku-4-5")
            .unwrap(),
        None
    );
}

/// The thinking matrix (docs/transport.md): Claude keeps the Anthropic pairings
/// (adaptive on `auto` for recognized tiers, explicit `disabled` on forced choice
/// and Haiku); foreign models never see `disabled` — adaptive on `auto` when the
/// capability lookup grants `reasoning`, the field omitted otherwise, forced
/// choice included. A mandatory-reasoning endpoint 400s on an explicit disable
/// but thinks and then honors a forced call with the field omitted.
#[test]
fn thinking_matrix_never_disables_foreign_models() {
    use super::thinking::Thinking;
    let auto = ToolChoice::auto();
    let forced = ToolChoice::force("retain");
    let m = |model: &str, choice: &ToolChoice, reasoning: bool| {
        Thinking::for_request(model, choice, reasoning, ThinkingOff::No)
    };
    assert!(matches!(
        m("claude-fable-5", &auto, false),
        Thinking::Adaptive { .. }
    ));
    assert!(matches!(
        m("claude-fable-5", &forced, false),
        Thinking::Disabled
    ));
    assert!(matches!(
        m("claude-haiku-4-5-20251001", &auto, false),
        Thinking::Disabled
    ));
    assert!(matches!(
        m("stealth/ox-alpha", &auto, true),
        Thinking::Adaptive { .. }
    ));
    assert!(matches!(
        m("stealth/ox-alpha", &forced, true),
        Thinking::Omit
    ));
    assert!(matches!(
        m("deepseek/deepseek-chat", &auto, false),
        Thinking::Omit
    ));
    assert!(matches!(
        m("deepseek/deepseek-chat", &forced, false),
        Thinking::Omit
    ));
    assert!(matches!(
        Thinking::for_keepalive("claude-haiku-4-5-20251001"),
        Thinking::Disabled
    ));
    assert!(matches!(
        Thinking::for_keepalive("stealth/ox-alpha"),
        Thinking::Omit
    ));
}

/// An `Omit` never reaches the wire: the request body carries no `thinking` key
/// at all, where a Claude request carries the adaptive block.
#[test]
fn omitted_thinking_is_absent_from_the_request_body() {
    let foreign =
        render_request_pretty("deepseek/deepseek-chat", "", &[], ToolChoice::auto(), &[]).unwrap();
    assert!(!foreign.contains("thinking"), "{foreign}");
    let claude = render_request_pretty("claude-fable-5", "", &[], ToolChoice::auto(), &[]).unwrap();
    assert!(claude.contains("\"thinking\""), "{claude}");
    assert!(claude.contains("adaptive"), "{claude}");
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

/// The `auto` thinking default is a Claude allowlist, not "everything but haiku":
/// foreign models (reached via OpenRouter) never get an Anthropic `thinking` block,
/// while direct ids and `anthropic/…` slugs do — except Haiku.
#[test]
fn adaptive_thinking_allowlist_is_claude_non_haiku() {
    assert!(is_recognized_adaptive_claude("claude-opus-4-8"));
    assert!(is_recognized_adaptive_claude("claude-sonnet-5"));
    assert!(is_recognized_adaptive_claude("anthropic/claude-opus-4.1"));
    assert!(is_recognized_adaptive_claude("claude-fable-5"));
    assert!(!is_recognized_adaptive_claude("claude-haiku-4-5-20251001"));
    assert!(!is_recognized_adaptive_claude("anthropic/claude-haiku-4.5"));
    assert!(!is_recognized_adaptive_claude("openai/gpt-5.6"));
    assert!(!is_recognized_adaptive_claude("google/gemini-2.5-pro"));
    assert!(!is_recognized_adaptive_claude("deepseek/deepseek-chat"));
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
    let why = crate::api::check_effort_thinking(Some("max"), true).unwrap_err();
    assert!(why.contains("max"), "names the level: {why}");
    assert!(
        why.contains("EFFERENCE_THINKING"),
        "names one route to thinking-off: {why}"
    );
    assert!(crate::api::check_effort_thinking(Some("xhigh"), true).is_err());
    assert!(crate::api::check_effort_thinking(Some("high"), true).is_ok());
    // A spelling outside the known vocabulary rides through — its coupling,
    // if any, is the provider's to answer, not a local list's to guess.
    assert!(crate::api::check_effort_thinking(Some("xmax"), true).is_ok());
    assert!(crate::api::check_effort_thinking(None, true).is_ok());
    for effort in ["low", "medium", "high"] {
        assert!(crate::api::check_effort_thinking(Some(effort), false).is_ok());
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
        max_output_tokens: None,
        supports_reasoning: true,
        accepts_temperature: true,
        supports_image_input: false,
        prompt_cache: PromptCachePolicy::Explicit,
        supported_efforts: levels.map(|l| l.iter().map(|s| s.to_string()).collect()),
        default_effort: default.map(String::from),
        // A fixture that declares levels is a source that declared them, which
        // is what makes the boot check apply.
        validates_efforts: levels.is_some(),
        reasoning_mandatory: None,
    }
}

/// The boot check's `thinking_off` input covers *both* routes to disabled, not
/// just the env override: a Claude model outside the adaptive rule whose
/// capability lookup denies reasoning (Haiku is the concrete case) falls through
/// to explicit `disabled` on every auto turn, so `--effort max` there must refuse
/// at boot rather than 400 on the first turn. Assumes `EFFERENCE_THINKING` is
/// unset in the test environment, like every other test of the thinking matrix.
#[test]
fn the_capability_route_to_thinking_off_is_covered() {
    use crate::api::auto_turns_disable_thinking as off;
    // Haiku: Claude family, not recognized-adaptive, no reasoning grant → disabled.
    assert!(off("claude-haiku-4-5", false));
    assert!(off("anthropic/claude-haiku-4.5", false));
    // Adaptive Claude thinks; a reasoning grant rescues an unrecognized Claude.
    assert!(!off("claude-opus-4-8", false));
    assert!(!off("claude-haiku-4-5", true));
    // A foreign model without reasoning gets `Omit`, not `disabled` — the
    // endpoint decides, so the boot check stays out of it.
    assert!(!off("deepseek/deepseek-v4-flash", false));
}

/// Which envelopes carry the effort, over a real socket: the turn (and the fork
/// peek, which is the same `complete` on the same client) sends it; the keepalive
/// on that same client does not (docs/transport.md). The keepalive sends thinking
/// disabled — `max_tokens: 0` rejects thinking-on — and `xhigh`/`max` are illegal
/// with thinking off, so the ping cannot carry the field the turns do. Its
/// envelope therefore differs from a turn's by exactly this key whenever a session
/// sets an effort; whether that splits the provider's cache key is the watch-item
/// in docs/transport.md, read off `usage.jsonl`.
///
/// A client that was never given an effort — the spotter's, built by
/// `spotter::ForkSpot` with a bare `Client::new` — sends the field on neither
/// path, which the second half asserts.
#[tokio::test]
async fn only_the_shared_turn_envelope_carries_the_effort() {
    use std::io::{Read, Write};

    /// One request/response over a fresh socket; returns the request bytes the
    /// client sent. `sse` picks the canned reply: an empty stream for `complete`,
    /// a usage-only JSON body for `keepalive`.
    async fn round_trip(effort: Option<&str>, sse: bool) -> String {
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
            provider: Provider::Anthropic,
            url: Some(format!("http://{addr}/v1/messages")),
            auth_header: "x-api-key".into(),
            auth_value: "k".into(),
            session_id: None,
            extra_headers: Vec::new(),
            base: None,
        };
        let client = Client::new(endpoint, "claude-opus-5", "sys", None)
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

    let turn = round_trip(Some("max"), true).await;
    assert!(
        turn.contains(r#""output_config":{"effort":"max"}"#),
        "the turn envelope carries it: {turn}"
    );
    let ping = round_trip(Some("max"), false).await;
    assert!(
        !ping.contains("output_config"),
        "the keepalive does not: {ping}"
    );

    // The shape of the spotter's client: never given an effort, so neither path
    // carries one — including with the forced tool choice that disables thinking.
    let turn = round_trip(None, true).await;
    assert!(!turn.contains("output_config"), "{turn}");
    let ping = round_trip(None, false).await;
    assert!(!ping.contains("output_config"), "{ping}");
}

/// `Client::without_thinking` on an `auto` turn: explicit `disabled` on Claude
/// either way; a foreign model gets the explicit form when its reasoning is
/// optional and the field omitted when it is mandatory — even one the lookup
/// granted reasoning (docs/redaction-fork.md, docs/transport.md).
#[test]
fn thinking_off_switch_disables_claude_and_optional_foreign_and_omits_mandatory() {
    use super::thinking::Thinking;
    let auto = ToolChoice::auto();
    assert!(matches!(
        Thinking::for_request("claude-fable-5", &auto, false, ThinkingOff::Omit),
        Thinking::Disabled
    ));
    assert!(matches!(
        Thinking::for_request("qwen/qwen3.8-flash", &auto, true, ThinkingOff::Disable),
        Thinking::Disabled
    ));
    assert!(matches!(
        Thinking::for_request("stealth/ox-alpha", &auto, true, ThinkingOff::Omit),
        Thinking::Omit
    ));
    assert!(matches!(
        Thinking::for_request("claude-fable-5", &auto, false, ThinkingOff::No),
        Thinking::Adaptive { .. }
    ));
}

/// `without_thinking` reaches a foreign model on OpenRouter as `reasoning.enabled:
/// false`, since `thinking` is omitted there; a Claude model gets `thinking:
/// disabled` and no `reasoning`; Anthropic never sees the field; and a client
/// that never asked sends nothing (docs/redaction-fork.md).
#[test]
fn thinking_off_reaches_a_foreign_openrouter_model_as_reasoning_disabled() {
    use super::thinking::Thinking;
    let openrouter =
        Endpoint::from_credentials(Provider::OpenRouter, "sk-or-v1-abc".into(), None, None)
            .unwrap();
    let anthropic =
        Endpoint::from_credentials(Provider::Anthropic, "sk-ant-xyz".into(), None, None).unwrap();
    let auto = ToolChoice::auto();
    let field = |client: &Client| {
        let thinking = Thinking::for_request(client.model(), &auto, false, client.thinking_off);
        client
            .reasoning_off(thinking)
            .map(|r| serde_json::to_string(&r).unwrap())
    };
    // Mandatory reasoning: `thinking` is omitted, so OpenRouter's own switch rides.
    let mandatory =
        Client::new(openrouter.clone(), "ibm/granite", "sys", None).without_thinking(Some(true));
    assert_eq!(field(&mandatory).as_deref(), Some(r#"{"enabled":false}"#));
    // Optional reasoning: explicit `thinking: disabled` says it, nothing else needed.
    let optional = Client::new(openrouter.clone(), "qwen/qwen3.8-flash", "sys", None)
        .without_thinking(Some(false));
    assert_eq!(field(&optional), None);
    let claude = Client::new(openrouter.clone(), "anthropic/claude-opus-5", "sys", None)
        .without_thinking(Some(false));
    assert_eq!(field(&claude), None);
    let direct = Client::new(anthropic, "claude-opus-5", "sys", None).without_thinking(Some(false));
    assert_eq!(field(&direct), None);
    // Unknown reasoning requirement takes the mandatory spelling: the field is
    // omitted and OpenRouter's own switch rides, because the explicit
    // `thinking: disabled` is an assertion a mandatory endpoint would reject.
    let unknown =
        Client::new(openrouter.clone(), "ibm/granite", "sys", None).without_thinking(None);
    assert_eq!(field(&unknown).as_deref(), Some(r#"{"enabled":false}"#));
    let unasked = Client::new(openrouter, "ibm/granite", "sys", None);
    assert_eq!(field(&unasked), None);
}
