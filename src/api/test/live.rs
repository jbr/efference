//! Tests for over-the-wire async behaviour: the stream ceiling, interruption,
//! an empty stream's captured response headers, OpenRouter attribution, and
//! the refusal of a foreign-dialect turn list.

use super::*;
use crate::record::log::RequestOrigin;
use crate::testing::upstream::{Canned, ScriptedUpstream};

/// A Messages stream's first event, and nothing after it.
const FIRST_EVENT: &str = "event: message_start\ndata: {\"message\":{\"model\":\"m\",\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n";

/// The ceiling the tests below set, taken on a paused clock: the read ends when it
/// fires, and a ceiling that fired early or late would end it somewhere else.
const CEILING: std::time::Duration = std::time::Duration::from_millis(200);

fn assert_ended_at_the_ceiling(started: tokio::time::Instant) {
    let elapsed = started.elapsed();
    assert!(
        elapsed >= CEILING && elapsed < CEILING * 2,
        "the read ended at the ceiling, not before or well after it: {elapsed:?}"
    );
}

/// **The duration ceiling** (docs/transport.md): a stream that opens, trickles,
/// and then never finalizes is given up on rather than blocking its turn forever,
/// and the completion comes back marked exactly the way a mid-stream abort is —
/// record-only, retryable, with the reason on it. The upstream here holds the
/// response open with an unterminated SSE body, which is the shape nothing
/// bounded before this: the retry policy applies only while *opening* a response.
///
/// One read loop serves both stream surfaces, so a turn and a triage are
/// bounded by the same knob.
#[tokio::test(start_paused = true)]
async fn a_stream_open_past_the_ceiling_is_marked_retryable() {
    // The upstream holds the response open after its first event, so the *client's*
    // ceiling is what ends the read.
    let upstream = ScriptedUpstream::answering([Canned::stream(FIRST_EVENT).left_open()]);
    let endpoint = upstream.endpoint(Provider::Anthropic, "/v1/messages");
    let client = Client::new(endpoint, "m", "sys").with_stream_ceiling(CEILING);
    let started = tokio::time::Instant::now();
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the ceiling is not an open failure — it assembles a partial");
    assert_ended_at_the_ceiling(started);
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("ceiling"), "{why}");
    assert!(why.contains("stream-ceiling"), "names the knob: {why}");
    let diagnostic = client.upstream_failure().lock().unwrap().clone().unwrap();
    assert_eq!(diagnostic.status, None);
    assert_eq!(diagnostic.detail, why);
}

/// The operator's interrupt ends a read in flight (docs/control-channel.md): the
/// upstream holds an unfinalized stream open, the notifier fires, and `complete`
/// returns at once with the partial marked `Aborted` — record-only like any
/// aborted attempt, but a class the retry loops do not retry. The notifier is
/// registered before the first read, so a press that lands between events is
/// not lost to `notify_waiters`' no-permit semantics.
#[tokio::test(start_paused = true)]
async fn an_interrupt_ends_the_read_and_marks_the_partial_aborted() {
    let upstream = ScriptedUpstream::answering([Canned::stream(FIRST_EVENT).left_open()]);
    let endpoint = upstream.endpoint(Provider::Anthropic, "/v1/messages");
    let interrupt = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut client =
        Client::new(endpoint, "m", "sys").with_stream_ceiling(std::time::Duration::from_secs(30));
    client.set_abort(std::sync::Arc::clone(&interrupt));
    let presser = std::sync::Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        presser.notify_waiters();
    });
    let started = tokio::time::Instant::now();
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an interrupt is not an open failure — it assembles a partial");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the read ended on the interrupt, not the ceiling"
    );
    assert!(completion.operator_abort());
    assert!(client.upstream_failure().lock().unwrap().is_none());
    assert_eq!(completion.failure_class, FailureClass::Aborted);
    assert!(completion.terminal_failure().is_none());
    let why = completion.upstream_failure().unwrap();
    assert!(why.contains("interrupt"), "{why}");
    assert!(
        matches!(
            &completion.assistant,
            Payload::Assistant {
                excluded: Some(_),
                ..
            }
        ),
        "the partial is record-only"
    );
}

/// End to end: the diagnostics come off the *response*, so the hook is in `complete` —
/// the status handler cannot serve this path, an empty stream being a 200 it
/// early-returns on. The upstream here answers exactly as a gateway fronting a slow
/// upstream does: 200, an `x-generation-id`, a `message_start` with all-zero usage,
/// `[DONE]`, nothing else.
#[tokio::test]
async fn an_empty_stream_over_the_wire_captures_the_response_headers() {
    let upstream = ScriptedUpstream::answering([Canned::stream(
        "event: message_start\ndata: {\"message\":{\"id\":\"gen-abc123\",\
         \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
         event: data\ndata: [DONE]\n\n",
    )
    .with_header("x-generation-id", "gen-abc123")
    .with_header("set-cookie", "session=nope")]);
    let endpoint = upstream.endpoint(Provider::OpenRouter, "/api/v1/messages");
    let client = Client::new(endpoint, "m", "sys");
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an empty stream assembles a record-only entry, not an error");

    let Payload::Assistant {
        excluded: Some(Exclusion::Upstream(failure)),
        ..
    } = &completion.assistant
    else {
        panic!("an empty stream is a record-only entry")
    };
    let stream = failure.stream.as_ref().expect("diagnostics are attached");
    assert_eq!(stream.status, 200);
    assert_eq!(
        stream.headers.get("x-generation-id").map(String::as_str),
        Some("gen-abc123")
    );
    // Off the allowlist, so it never reaches the log (docs/env.md).
    assert!(!stream.headers.contains_key("set-cookie"));
    assert!(stream.frames.contains("data: [DONE]"), "{stream:?}");
}

/// The ceiling is a `[transport]` knob with a generous default, not a hardcoded
/// number: the right value is workload-dependent (docs/transport.md).
#[test]
fn the_stream_ceiling_defaults_generously_and_is_operator_settable() {
    assert_eq!(
        crate::config::TransportLimits::default().stream_ceiling,
        crate::config::DEFAULT_STREAM_CEILING
    );
    // Comfortably past the widest model call in this repo's corpus (363s).
    assert!(crate::config::DEFAULT_STREAM_CEILING >= std::time::Duration::from_secs(3600));
}

/// The attribution headers ride an OpenRouter inference POST and no other
/// (docs/transport.md). Asserted on the request as sent rather than off
/// `Provider::attribution` alone, because the thing that can break is the wiring:
/// `with_attribution` is applied on two conn-building paths and would be silently
/// absent from either.
///
/// The negative half is the point. `HTTP-Referer` is a header hosts act on with
/// its RFC 9110 meaning, so an Anthropic turn must carry none of these — while
/// still carrying the `User-Agent`, which every host is meant to see.
#[tokio::test]
async fn attribution_rides_openrouter_turns_and_nothing_else() {
    use crate::testing::upstream::Request;

    /// The request one round trip sends against a canned answer. `sse` picks the
    /// path: an empty stream for `complete`, a usage-only body for `keepalive`.
    async fn request(provider: Provider, sse: bool) -> Request {
        let upstream = ScriptedUpstream::answering([if sse {
            Canned::stream(
                "event: message_start\ndata: {\"message\":{\"id\":\"gen-1\",\
                 \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                 event: data\ndata: [DONE]\n\n",
            )
        } else {
            Canned::json(
                200,
                "{\"id\":\"msg-1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\
                 \"output_tokens\":0}}",
            )
        }]);
        let client = Client::new(upstream.endpoint(provider, "/v1/messages"), "m", "sys");
        if sse {
            client
                .complete(&[], &[], ToolChoice::auto(), |_| {})
                .await
                .expect("the canned stream assembles");
        } else {
            client
                .keepalive(
                    RequestOrigin::Keepalive,
                    &SentRequest::new(Vec::new(), Vec::new(), ToolChoice::auto()),
                )
                .await
                .expect("the canned body parses");
        }
        upstream.requests().remove(0)
    }

    /// The names of the headers `request` carried, lowercased.
    fn names(request: &Request) -> Vec<String> {
        request
            .headers
            .iter()
            .map(|(name, _)| name.to_string().to_ascii_lowercase())
            .collect()
    }

    // The `User-Agent` is the one header every request carries, on every path.
    let user_agent = Some(crate::platform::http::user_agent());

    for sse in [true, false] {
        let openrouter = request(Provider::OpenRouter, sse).await;
        for (name, value) in [
            ("http-referer", "https://efference.net"),
            ("x-openrouter-title", "efference"),
            ("x-openrouter-categories", "cli-agent,programming-app"),
        ] {
            assert_eq!(openrouter.header(name), Some(value), "{openrouter:?}");
        }
        assert_eq!(openrouter.header("user-agent"), user_agent);

        let anthropic = request(Provider::Anthropic, sse).await;
        let names = names(&anthropic);
        assert!(
            !names
                .iter()
                .any(|name| name.contains("referer") || name.starts_with("x-openrouter")),
            "{names:?}"
        );
        assert_eq!(anthropic.header("user-agent"), user_agent);
    }
}

/// The duration ceiling on the Responses dialect: a stream that delivered partial
/// text and then stayed open is cut at the ceiling, the partial kept record-only
/// with a transient class, so the abort schedule retries it (docs/transport.md).
#[tokio::test(start_paused = true)]
async fn a_responses_stream_past_the_ceiling_keeps_its_partial_as_transient() {
    let upstream = ScriptedUpstream::answering([Canned::stream(
        "event: response.created\ndata: {\"type\":\"response.created\",\
         \"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\",\"output\":[]}}\n\n\
         event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\
         \"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\
         \"status\":\"in_progress\",\"content\":[]}}\n\n\
         event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\
         \"output_index\":0,\"content_index\":0,\"delta\":\"half an\"}\n\n",
    )
    .left_open()]);
    let endpoint = upstream.endpoint(Provider::OpenAI, "/v1/responses");
    let client = Client::new(endpoint, "gpt-5.6-luna", "sys").with_stream_ceiling(CEILING);
    let mut streamed = Vec::new();
    let started = tokio::time::Instant::now();
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |s| streamed.push(s))
        .await
        .expect("the ceiling assembles a partial");
    assert_ended_at_the_ceiling(started);
    let why = completion.upstream_failure().expect("marked");
    assert!(why.contains("stream-ceiling"), "{why}");
    assert_eq!(completion.failure_class, FailureClass::Transient);
    assert!(
        streamed.iter().any(|s| matches!(s, Streamed::Text { .. })),
        "the partial text streamed before the cut"
    );
}

/// The dialect guard sits before the encoder in `complete` (docs/transport.md): a
/// turn list recorded in the other dialect is refused with the conflict named, and
/// no request leaves — the endpoint here has nothing listening, so a request would
/// fail with a connection error instead of this message.
#[tokio::test]
async fn a_foreign_dialect_turn_list_never_becomes_a_request() {
    use crate::record::project::WireTurn;
    let endpoint = Endpoint::at(Provider::Anthropic, "http://127.0.0.1:1/v1/messages");
    let client = Client::new(endpoint, "m", "sys");
    let turns = vec![
        WireTurn::text("hello"),
        WireTurn::Assistant(AssistantOutput::Responses(vec![
            crate::record::log::ResponsesOutputItem::FunctionCall {
                id: None,
                call_id: CallId("call_1".into()),
                name: "bash".into(),
                arguments: "{}".into(),
                status: None,
            },
        ])),
    ];
    let err = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .expect_err("a Responses history must not encode onto the Messages wire");
    let text = err.to_string();
    assert!(text.contains("recorded in the Responses dialect"), "{text}");
    assert!(text.contains("speaks Messages"), "{text}");
}
