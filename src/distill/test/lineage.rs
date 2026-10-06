//! Tests for which client and lineage a peek runs on.
use super::*;

/// What the gate records as the peek's reasoning (docs/redaction-fork.md): every thinking
/// block's text, joined — prose and redacted blocks excluded (a redacted block carries no
/// text), and `None` when the attempt thought nothing, so a gate that didn't think records
/// nothing rather than an empty entry. Signatures are dropped: this is a record, not a
/// replay input.
#[test]
fn peek_thinking_joins_thinking_text_only() {
    use crate::log::ContentBlock;

    let assistant = |thinking: Vec<ContentBlock>| Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            thinking,
            vec![ContentBlock::Text("prose the gate never reads".into())],
            Vec::new(),
        ),
        upstream_failure: None,
    };

    assert_eq!(peek_thinking(&assistant(Vec::new())), None);
    assert_eq!(
        peek_thinking(&assistant(vec![
            ContentBlock::Thinking {
                text: "the head is boilerplate".into(),
                signature: "sig-a".into(),
            },
            ContentBlock::RedactedThinking {
                data: "opaque".into(),
            },
            ContentBlock::Thinking {
                text: "keep 40-52".into(),
                signature: "sig-b".into(),
            },
        ])),
        Some("the head is boilerplate\nkeep 40-52".to_string())
    );
    assert_eq!(
        peek_thinking(&assistant(vec![ContentBlock::RedactedThinking {
            data: "opaque".into(),
        }])),
        None,
        "a redacted-only attempt has no text to record"
    );
}

/// The Responses analogue of the canonical-versus-peek prefix rule
/// (docs/redaction-fork.md): under explicit caching the peek's one marker sits at the
/// shared pre-fork frontier, nothing in the fork-local raw tail carries one, and the
/// next canonical request is that same prefix, content for content, followed by the
/// distillate — never the raw output or the peek's ask.
#[test]
fn a_responses_peek_marks_the_shared_frontier_and_the_canonical_wire_carries_only_the_distillate() {
    use crate::api::{PromptCachePolicy, encode_responses};
    use crate::log::{Distilled, Keep, ResponsesOutputItem, WireState};

    let call = ToolCall::new("bash", "call_1", json!({ "command": "ls" }));
    let prefix = vec![
        entry(
            1,
            Payload::User(vec![crate::log::ContentBlock::Text("go".into())]),
        ),
        entry(
            2,
            Payload::Assistant {
                output: AssistantOutput::Responses(vec![
                    ResponsesOutputItem::Reasoning {
                        id: "rs_1".into(),
                        status: Some("completed".into()),
                        summary: Vec::new(),
                        content: Vec::new(),
                        encrypted_content: Some("ENC==".into()),
                    },
                    ResponsesOutputItem::FunctionCall {
                        id: Some("fc_1".into()),
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: r#"{"command":"ls"}"#.into(),
                        status: Some("completed".into()),
                    },
                ]),
                upstream_failure: None,
            },
        ),
    ];
    let raw = RawResult::text("alpha\nbeta\ngamma");
    let pending = [Pending {
        call: &call,
        raw: &raw,
    }];

    // The peek: prefix, frontier, then the fork-local raw and the ask.
    let peek = peek_request(&prefix, Reader::Main, &pending, &[], &[]);
    let peek_input = encode_responses(&peek, "", PromptCachePolicy::Explicit);
    let peek_json: Vec<serde_json::Value> = peek_input
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect();
    let marked: Vec<usize> = peek_json
        .iter()
        .enumerate()
        .filter(|(_, item)| item.to_string().contains("prompt_cache_breakpoint"))
        .map(|(i, _)| i)
        .collect();
    let first_output = peek_json
        .iter()
        .position(|item| item["type"] == "function_call_output")
        .expect("the raw tail answers the call");
    assert_eq!(marked.len(), 1, "one marker: {peek_json:#?}");
    assert!(
        marked[0] < first_output,
        "the marker sits before the fork-local tail, not on it: marked {marked:?}, tail at {first_output}"
    );
    let tail = serde_json::to_string(&peek_json[first_output..]).unwrap();
    assert!(tail.contains("gamma"), "the raw rides the tail: {tail}");
    let shared = serde_json::to_string(&peek_json[..first_output]).unwrap();
    assert!(!shared.contains("gamma"), "and never the prefix: {shared}");

    // The canonical continuation: the verdict is logged, the raw is not projected.
    let mut log = prefix;
    log.push(entry(
        3,
        Payload::tool_result(
            call.id.clone(),
            raw.clone(),
            WireState::from(Distilled::new(Keep::Ranges(vec![(1, 1)])).with_note("kept the first")),
        ),
    ));
    let mut canonical = crate::project::project_semantic(&log);
    canonical.push(WireTurn::Breakpoint);
    let canonical_input = encode_responses(&canonical, "", PromptCachePolicy::Explicit);
    let canonical_json: Vec<serde_json::Value> = canonical_input
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect();
    // The pre-fork items are the peek's, content for content. The marker is a
    // write instruction, not content: the peek's sits at the frontier, the
    // canonical request's at its own tail, and the provider matches the prefix on
    // what is between them (docs/log-and-projection.md).
    fn unmarked(item: &serde_json::Value) -> serde_json::Value {
        match item {
            serde_json::Value::Object(map) => map
                .iter()
                .filter(|(key, _)| key.as_str() != "prompt_cache_breakpoint")
                .map(|(key, value)| (key.clone(), unmarked(value)))
                .collect(),
            serde_json::Value::Array(items) => items.iter().map(unmarked).collect(),
            other => other.clone(),
        }
    }
    assert_eq!(
        canonical_json[..first_output]
            .iter()
            .map(unmarked)
            .collect::<Vec<_>>(),
        peek_json[..first_output]
            .iter()
            .map(unmarked)
            .collect::<Vec<_>>(),
        "the pre-fork items are the peek's — one cache lineage"
    );
    let canonical_marked: Vec<usize> = canonical_json
        .iter()
        .enumerate()
        .filter(|(_, item)| item.to_string().contains("prompt_cache_breakpoint"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        canonical_marked,
        vec![canonical_json.len() - 1],
        "the canonical marker moved to the distillate at the tail: {canonical_json:#?}"
    );
    let text = serde_json::to_string(&canonical_json).unwrap();
    assert!(
        text.contains("alpha") && text.contains("kept the first"),
        "{text}"
    );
    assert!(
        !text.contains("gamma"),
        "dropped raw stays off the canonical wire: {text}"
    );
    let ask = peek_json.last().unwrap().to_string();
    assert!(
        ask.contains("retain"),
        "the peek's ask names the retain tool: {ask}"
    );
    let ask_text = peek_json.last().unwrap()["content"][0]["text"]
        .as_str()
        .expect("the ask is a text part");
    assert!(
        !text.contains(ask_text),
        "the peek's ask never reaches the canonical wire"
    );
}

/// The peek's request surface follows its configuration (docs/redaction-fork.md):
/// shared borrows the agent's client and `tools[]`; own sends its own client and
/// only the two peek tools.
#[test]
fn own_peek_sends_its_own_client_and_only_the_peek_tools() {
    let endpoint = crate::api::Endpoint::from_credentials(
        crate::api::Provider::Anthropic,
        "sk-ant-xyz".into(),
        None,
        None,
    )
    .unwrap();
    let agent = Client::new(endpoint.clone(), "big", "sys", None);
    let agent_tools = vec![json!({"name": "read"}), retain_tool(), triage_tool()];

    let shared = ForkPeek::shared();
    assert!(!shared.is_own());
    assert_eq!(shared.own_model(), None);
    assert_eq!(shared.client(&agent).model(), "big");
    assert_eq!(shared.tools(&agent_tools).len(), 3);

    let own = ForkPeek::own(Client::new(endpoint.for_triage(), "small", "persona", None));
    assert!(own.is_own());
    assert_eq!(own.own_model(), Some("small"));
    assert_eq!(own.client(&agent).model(), "small");
    let names: Vec<&str> = own
        .tools(&agent_tools)
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["retain", "triage"]);

    // The own peek forces the asked-for tool; the shared peek never does, and the
    // knob turns the own one back to `auto`.
    assert_eq!(shared.choice(RETAIN).forced(), None);
    assert_eq!(own.choice(RETAIN).forced(), Some("retain"));
    assert_eq!(own.choice(TRIAGE).forced(), Some("triage"));
    assert_eq!(own.with_auto_choice().choice(RETAIN).forced(), None);
}

/// The standalone idle triage rides the agent lineage even when a separate triage
/// model is configured (docs/redaction-fork.md): the agent's client, the agent's
/// full `tools[]`, and `auto`.
#[test]
fn the_idle_triage_rides_the_agent_lineage_whatever_the_triage_model() {
    let endpoint = crate::api::Endpoint::from_credentials(
        crate::api::Provider::Anthropic,
        "sk-ant-xyz".into(),
        None,
        None,
    )
    .unwrap();
    let agent = Client::new(endpoint.clone(), "big", "sys", None);
    let agent_tools = vec![json!({"name": "read"}), retain_tool(), triage_tool()];
    let own = ForkPeek::own(Client::new(endpoint.for_triage(), "small", "persona", None));

    let (client, tools, choice) = own.idle_envelope(&agent, &agent_tools);
    assert_eq!(client.model(), "big");
    assert_eq!(tools.len(), 3);
    assert_eq!(choice.forced(), None);
    // The per-result peek still runs on the own model.
    assert_eq!(own.client(&agent).model(), "small");
}

/// The shared peek rides the agent lineage the main request already warmed, so there
/// is no triage lineage to plant a primer in (backlog/separate-triage-cold-prefix.md).
#[tokio::test]
async fn the_shared_peek_takes_no_primer() {
    assert!(
        ForkPeek::shared().prime(&[], &[]).await.is_none(),
        "a shared peek never primes"
    );
}
