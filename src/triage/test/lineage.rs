//! Tests for which client and lineage a triage runs on.
use super::*;

/// What the triage records as the triage's reasoning (docs/triage.md): every thinking
/// block's text, joined — prose and redacted blocks excluded (a redacted block carries no
/// text), and `None` when the attempt thought nothing, so a triage that didn't think records
/// nothing rather than an empty entry. Signatures are dropped: this is a record, not a
/// replay input.
#[test]
fn triage_thinking_joins_thinking_text_only() {
    use crate::log::ContentBlock;

    let assistant = |thinking: Vec<ContentBlock>| {
        Payload::assistant(AssistantOutput::from_legacy_messages(
            thinking,
            vec![ContentBlock::Text("prose the triage never reads".into())],
            Vec::new(),
        ))
    };

    assert_eq!(triage_thinking(&assistant(Vec::new())), None);
    assert_eq!(
        triage_thinking(&assistant(vec![
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
        triage_thinking(&assistant(vec![ContentBlock::RedactedThinking {
            data: "opaque".into(),
        }])),
        None,
        "a redacted-only attempt has no text to record"
    );
}

/// The Responses analogue of the canonical-versus-triage prefix rule
/// (docs/triage.md): under explicit caching the triage's one marker sits at the
/// shared pre-fork frontier, nothing in the fork-local raw tail carries one, and the
/// next canonical request is that same prefix, content for content, followed by the
/// selection — never the raw output or the triage's ask.
// A scenario read top to bottom: the triage request, the canonical request that follows it, and
// the comparison between their prefixes.
#[allow(clippy::too_many_lines)]
#[test]
fn a_responses_triage_marks_the_shared_frontier_and_the_canonical_wire_carries_only_the_selection()
{
    use crate::api::{PromptCachePolicy, encode_responses};
    use crate::log::{Keep, ResponsesOutputItem, Triaged, WireState};

    let call = ToolCall::new("bash", "call_1", json!({ "command": "ls" }));
    let prefix = vec![
        entry(1, Payload::user("go")),
        entry(
            2,
            Payload::assistant(AssistantOutput::Responses(vec![
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
            ])),
        ),
    ];
    let raw = RawResult::text("alpha\nbeta\ngamma");
    let pending = [Pending {
        call: &call,
        raw: &raw,
    }];

    // The triage: prefix, frontier, then the fork-local raw and the ask.
    let triage = triage_request(
        &prefix,
        Reader::Main,
        TriageView::Transcript,
        &pending,
        &[],
        &[],
    );
    let triage_input = encode_responses(&triage, "", PromptCachePolicy::Explicit);
    let triage_json: Vec<Value> = triage_input
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect();
    let marked: Vec<usize> = triage_json
        .iter()
        .enumerate()
        .filter(|(_, item)| item.to_string().contains("prompt_cache_breakpoint"))
        .map(|(i, _)| i)
        .collect();
    let first_output = triage_json
        .iter()
        .position(|item| item["type"] == "function_call_output")
        .expect("the raw tail answers the call");
    assert_eq!(marked.len(), 1, "one marker: {triage_json:#?}");
    assert!(
        marked[0] < first_output,
        "the marker sits before the fork-local tail, not on it: marked {marked:?}, tail at {first_output}"
    );
    let tail = serde_json::to_string(&triage_json[first_output..]).unwrap();
    assert!(tail.contains("gamma"), "the raw rides the tail: {tail}");
    let shared = serde_json::to_string(&triage_json[..first_output]).unwrap();
    assert!(!shared.contains("gamma"), "and never the prefix: {shared}");

    // The canonical continuation: the verdict is logged, the raw is not projected.
    let mut log = prefix;
    log.push(entry(
        3,
        Payload::tool_result(
            call.id.clone(),
            raw.clone(),
            WireState::from(Triaged::new(Keep::Ranges(vec![(1, 1)])).with_note("kept the first")),
        ),
    ));
    let mut canonical = crate::project::project_semantic(&log);
    canonical.push(WireTurn::Breakpoint);
    let canonical_input = encode_responses(&canonical, "", PromptCachePolicy::Explicit);
    let canonical_json: Vec<Value> = canonical_input
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect();
    // The pre-fork items are the triage's, content for content. The marker is a
    // write instruction, not content: the triage's sits at the frontier, the
    // canonical request's at its own tail, and the provider matches the prefix on
    // what is between them (docs/log-and-projection.md).
    fn unmarked(item: &Value) -> Value {
        match item {
            Value::Object(map) => map
                .iter()
                .filter(|(key, _)| key.as_str() != "prompt_cache_breakpoint")
                .map(|(key, value)| (key.clone(), unmarked(value)))
                .collect(),
            Value::Array(items) => items.iter().map(unmarked).collect(),
            other => other.clone(),
        }
    }
    assert_eq!(
        canonical_json[..first_output]
            .iter()
            .map(unmarked)
            .collect::<Vec<_>>(),
        triage_json[..first_output]
            .iter()
            .map(unmarked)
            .collect::<Vec<_>>(),
        "the pre-fork items are the triage's — one cache lineage"
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
        "the canonical marker moved to the selection at the tail: {canonical_json:#?}"
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
    let ask = triage_json.last().unwrap().to_string();
    assert!(
        ask.contains("retain"),
        "the triage's ask names the retain tool: {ask}"
    );
    let ask_text = triage_json.last().unwrap()["content"][0]["text"]
        .as_str()
        .expect("the ask is a text part");
    assert!(
        !text.contains(ask_text),
        "the triage's ask never reaches the canonical wire"
    );
}

/// The triage's request surface follows its configuration (docs/triage.md):
/// shared borrows the agent's client and `tools[]`; own sends its own client and
/// only the two triage tools.
#[test]
fn own_triage_sends_its_own_client_and_only_the_triage_tools() {
    let endpoint = crate::api::Endpoint::from_credentials(
        crate::log::Provider::Anthropic,
        "sk-ant-xyz",
        None,
        None,
    )
    .unwrap();
    let agent = Client::new(endpoint.clone(), "big", "sys");
    let agent_tools = vec![json!({"name": "read"}), retain_tool(), triage_tool()];

    let shared = Triager::shared();
    assert!(!shared.is_own());
    assert_eq!(shared.own_model(), None);
    assert_eq!(shared.client(&agent, RequestOrigin::Triage).model(), "big");
    assert_eq!(shared.tools(&agent_tools).len(), 3);

    let own = Triager::own(Client::new(endpoint.for_triage(), "small", "persona"));
    assert!(own.is_own());
    assert_eq!(own.own_model(), Some("small"));
    assert_eq!(own.client(&agent, RequestOrigin::Triage).model(), "small");
    let names: Vec<&str> = own
        .tools(&agent_tools)
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["retain", "triage"]);

    // The own triage forces the asked-for tool; the shared triage never does, and the
    // knob turns the own one back to `auto`.
    assert_eq!(shared.choice(RETAIN).forced(), None);
    assert_eq!(own.choice(RETAIN).forced(), Some("retain"));
    assert_eq!(own.choice(TRIAGE).forced(), Some("triage"));
    assert_eq!(own.with_auto_choice().choice(RETAIN).forced(), None);
}

/// The standalone idle triage rides the agent lineage even when a separate triage
/// model is configured (docs/triage.md): the agent's client, the agent's
/// full `tools[]`, and `auto`.
#[test]
fn the_idle_triage_rides_the_agent_lineage_whatever_the_triage_model() {
    let endpoint = crate::api::Endpoint::from_credentials(
        crate::log::Provider::Anthropic,
        "sk-ant-xyz",
        None,
        None,
    )
    .unwrap();
    let agent = Client::new(endpoint.clone(), "big", "sys");
    let agent_tools = vec![json!({"name": "read"}), retain_tool(), triage_tool()];
    let own = Triager::own(Client::new(endpoint.for_triage(), "small", "persona"));

    let (client, tools, choice) = own.idle_envelope(&agent, &agent_tools);
    assert_eq!(client.model(), "big");
    assert_eq!(tools.len(), 3);
    assert_eq!(choice.forced(), None);
    // The per-result triage still runs on the own model.
    assert_eq!(own.client(&agent, RequestOrigin::Triage).model(), "small");
}

/// The shared triage rides the agent lineage the main request already warmed, so there
/// is no triage lineage to plant a primer in (docs/transport.md).
#[tokio::test]
async fn the_shared_triage_takes_no_primer() {
    assert!(
        Triager::shared().prime(&[]).await.is_none(),
        "a shared triage never primes"
    );
}
