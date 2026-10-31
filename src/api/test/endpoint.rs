//! Tests for the derived endpoint: URL/auth/header carriage, the closed
//! provider vocabulary, credential redaction and expiry, and the
//! sticky/session/spotter/triage routing keys.

use super::*;

/// One declared provider derives the whole endpoint: URL, auth header name, and
/// scheme prefix. The secret arrives alone and stays a *whole* value.
#[test]
fn endpoint_is_derived_from_the_declared_provider() {
    let anthropic =
        Endpoint::from_credentials(Provider::Anthropic, "sk-ant-xyz", None, None).unwrap();
    assert_eq!(
        anthropic.url.as_deref(),
        Some("https://api.anthropic.com/v1/messages")
    );
    assert_eq!(anthropic.auth_header, "x-api-key");
    assert_eq!(anthropic.auth_value, "sk-ant-xyz");
    let openrouter =
        Endpoint::from_credentials(Provider::OpenRouter, "sk-or-v1-abc", None, None).unwrap();
    assert_eq!(
        openrouter.url.as_deref(),
        Some("https://openrouter.ai/api/v1/messages")
    );
    assert_eq!(openrouter.auth_header, "authorization");
    assert_eq!(openrouter.auth_value, "Bearer sk-or-v1-abc");
    // Base64 padding rides through verbatim.
    let padded = Endpoint::from_credentials(Provider::Anthropic, "abc==", None, None).unwrap();
    assert_eq!(padded.auth_value, "abc==");
}

/// The Codex backend needs its account id and is the only provider that takes one.
#[test]
fn codex_credentials_carry_the_account_header() {
    let codex = Endpoint::from_credentials(Provider::OpenAICodex, "tok", Some("acct".into()), None)
        .unwrap();
    assert_eq!(
        codex.extra_headers,
        vec![("ChatGPT-Account-ID".to_string(), "acct".to_string())]
    );
    assert!(Endpoint::from_credentials(Provider::OpenAICodex, "tok", None, None).is_err());
    assert!(
        Endpoint::from_credentials(Provider::OpenAI, "tok", Some("acct".into()), None).is_err()
    );
}

/// An unrecognized provider name is a hard error naming the valid values — never
/// a silent fallback to a host the key doesn't belong to. Names are trimmed and
/// `codex` is an alias.
#[test]
fn provider_names_are_a_closed_vocabulary() {
    let err = Provider::from_name("cohere").unwrap_err().to_string();
    assert!(err.contains("anthropic"), "{err}");
    assert!(err.contains("openrouter"), "{err}");
    assert_eq!(Provider::from_name(" openai ").unwrap(), Provider::OpenAI);
    assert_eq!(Provider::from_name("codex").unwrap(), Provider::OpenAICodex);
    assert_eq!(
        serde_json::to_value(Provider::OpenAICodex).unwrap(),
        serde_json::json!("openai-codex")
    );
}

/// The endpoint's Debug redacts the credential and the extra headers.
#[test]
fn endpoint_debug_redacts_credentials() {
    let codex = Endpoint::from_credentials(
        Provider::OpenAICodex,
        "tok-secret",
        Some("acct".into()),
        None,
    )
    .unwrap();
    let debug = format!("{codex:?}");
    assert!(!debug.contains("tok-secret"), "{debug}");
    assert!(!debug.contains("acct"), "{debug}");
    assert!(
        debug.contains("openai-codex") || debug.contains("OpenAICodex"),
        "{debug}"
    );
}

// ---- the sticky-routing session key (docs/transport.md) ----

/// The key is derived from the session dir, which is fixed for the session's life,
/// so re-deriving it yields the same bytes — the property the whole mechanism rests
/// on. A key that moved with the projection would re-route exactly when consolidation
/// rewrites the opening messages, which is the failure it exists to prevent.
#[test]
fn session_key_is_stable_across_derivations() {
    let dir = std::path::Path::new(".efference/work");
    assert_eq!(sticky_session_id(dir), sticky_session_id(dir));
}

/// Session ids are unique per `.efference/` dir, not per account: two repos both
/// served `--name work` must not pin each other's routing. The name stays readable
/// in front (the Sessions view is the other thing the key buys) and the path hash
/// separates them.
#[test]
fn session_key_separates_same_name_in_different_dirs() {
    let a = sticky_session_id(std::path::Path::new("alpha/.efference/work"));
    let b = sticky_session_id(std::path::Path::new("beta/.efference/work"));
    assert!(a.starts_with("work-"), "{a}");
    assert!(b.starts_with("work-"), "{b}");
    assert_ne!(a, b);
}

/// `--name` is arbitrary user input, so the 256-char cap has to hold without
/// panicking and without eating the hash. Truncating the tail would drop the
/// disambiguating half — the one thing the cap must not do.
#[test]
fn session_key_caps_by_truncating_the_name_not_the_hash() {
    let long = "x".repeat(400);
    let dir = std::path::PathBuf::from(".efference").join(&long);
    let key = sticky_session_id(&dir);
    assert!(key.len() <= 256, "{}", key.len());
    // the hash survives: the same dir under a shorter name shares no suffix by
    // accident, and this key still ends in one full hash's worth of hex
    let (name, hash) = key.rsplit_once('-').unwrap();
    assert_eq!(hash.len(), 16, "{key}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{key}");
    assert!(name.chars().all(|c| c == 'x'), "{key}");
}

/// A multibyte `--name` truncates on a character boundary. A plain byte slice here
/// panics at boot, which is a crash on the operator's first non-ASCII session name.
#[test]
fn session_key_truncates_a_multibyte_name_without_panicking() {
    let long = "é".repeat(400);
    let key = sticky_session_id(&std::path::PathBuf::from(".efference").join(&long));
    assert!(key.len() <= 256, "{}", key.len());
    assert!(key.contains('é'));
}

/// The spotter's key is distinct from the agent's. Its prefix shares nothing with the
/// agent turn, and an explicit key overrides the message hashing that would
/// otherwise separate them for free — so pointing both clients at one model would
/// pin two unrelated prefixes to one route.
#[test]
fn spotter_endpoint_takes_a_distinct_key_that_still_fits_the_cap() {
    let mut e = Endpoint::from_credentials(Provider::Anthropic, "sk-ant-xyz", None, None).unwrap();
    let long = "x".repeat(400);
    e.session_id = Some(sticky_session_id(
        &std::path::PathBuf::from(".efference").join(&long),
    ));
    let spot = e.for_spotter();
    assert_ne!(spot.session_id, e.session_id);
    assert!(spot.session_id.unwrap().len() <= 256);
}

/// An endpoint with no key derived — every offline verb — yields no spotter key
/// either, rather than a bare suffix.
#[test]
fn spotter_endpoint_without_a_key_stays_keyless() {
    let e = Endpoint::from_credentials(Provider::Anthropic, "sk-ant-xyz", None, None).unwrap();
    assert_eq!(e.session_id, None);
    assert_eq!(e.for_spotter().session_id, None);
}

/// Only providers that read a sticky key get the header, and each gets the
/// spelling it reads. Anthropic has no sticky-routing concept, so gating here is
/// what keeps the design off the assumption that an unrecognized header is ignored
/// rather than rejected. The Codex arm is the case that gating got wrong once: it
/// does read a key, on `session_id`, and sending only the body copy left the
/// backend minting a fresh `prompt_cache_key` per request (docs/transport.md).
#[test]
fn each_provider_declares_the_session_header_it_reads() {
    assert_eq!(Provider::Anthropic.session_header(), None);
    assert_eq!(Provider::OpenAI.session_header(), None);
    assert_eq!(Provider::OpenAICodex.session_header(), Some("session-id"));
    assert_eq!(Provider::OpenRouter.session_header(), Some("x-session-id"));
}

/// Which providers read the body field, and which read the header. OpenRouter
/// takes both (the header wins its own precedence order; the body copy is what an
/// OpenAI-dialect upstream reads for its own cache). Anthropic takes neither —
/// there is one upstream to route to, and an unrecognized top-level key is a 400
/// there, so this cannot be sent unconditionally (docs/transport.md).
#[test]
fn only_openrouter_carries_the_sticky_key() {
    assert!(!Provider::Anthropic.prompt_cache_key_field());
    assert!(Provider::OpenRouter.prompt_cache_key_field());
    assert_eq!(Provider::Anthropic.session_header(), None);
    assert_eq!(Provider::OpenRouter.session_header(), Some("x-session-id"));
    // The Codex arm sends both spellings and only the header carries: the
    // backend accepts the body field and discards it (`api/provider.rs`).
    assert!(Provider::OpenAICodex.prompt_cache_key_field());
    assert_eq!(Provider::OpenAICodex.session_header(), Some("session-id"));
}

/// The separate-model triage's key is its own: distinct from the agent's and the
/// spotter's, under the same cap, and absent when the agent has none
/// (docs/triage.md, docs/transport.md).
#[test]
fn triage_endpoint_takes_a_distinct_key_under_the_cap() {
    let mut e =
        Endpoint::from_credentials(Provider::OpenRouter, "sk-or-v1-abc", None, None).unwrap();
    assert_eq!(e.for_triage().session_id, None);
    let long = "x".repeat(400);
    e.session_id = Some(sticky_session_id(
        &std::path::PathBuf::from(".efference").join(&long),
    ));
    let triage = e.for_triage().session_id.unwrap();
    assert_ne!(Some(&triage), e.session_id.as_ref());
    assert_ne!(Some(&triage), e.for_spotter().session_id.as_ref());
    assert!(triage.ends_with("-triage"));
    assert!(triage.len() <= 256);
}

/// **A credential that expired under a running host** (docs/transport.md): the
/// Codex backend answers an aged-out ChatGPT token with a 401 naming
/// `token_expired`, which is the one rejected-credential case with a remedy —
/// the value is re-read from its source and the attempt goes again. Before this,
/// the token was copied into the endpoint at host launch, so every session held
/// its own dead copy and `codex login` in another terminal changed nothing.
///
/// The second request must carry the *new* token, so the test fails if the retry
/// re-sends the rejected one.
#[tokio::test]
async fn an_expired_credential_is_re_read_and_the_attempt_retried() {
    use crate::testing::upstream::{Canned, ScriptedUpstream};

    let expired = "{\"error\":{\"message\":\"Provided authentication token is expired. \
                   Please try signing in again.\",\"type\":null,\"code\":\"token_expired\"},\
                   \"status\":401,\"detail\":{\"code\":\"token_expired\"}}";
    let upstream = ScriptedUpstream::answering([
        Canned::json(401, expired),
        Canned::stream(
            "event: message_start\ndata: {\"type\":\"message_start\",\
             \"message\":{\"id\":\"gen-1\",\"model\":\"m\",\
             \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n\
             event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\
             \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n\
             event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
    ]);

    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&reads);
    let endpoint = upstream
        .endpoint(Provider::Anthropic, "/v1/messages")
        .with_auth_value(AuthValue::reloadable(
            Provider::Anthropic,
            "stale".into(),
            sync_reload(move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("refreshed".into())
            }),
        ));
    let client = Client::new(endpoint, "m", "sys", None);
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the re-read credential completes the turn");

    assert!(
        completion.upstream_failure().is_none(),
        "the retry succeeded"
    );
    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the source is read once, not once per attempt"
    );
    let requests = upstream.requests();
    assert_eq!(requests[0].header("x-api-key"), Some("stale"));
    assert_eq!(
        requests[1].header("x-api-key"),
        Some("refreshed"),
        "the retry carries the re-read value"
    );
}

/// A listing read is a question about a route, so its whole answer survives to
/// the caller: a non-2xx comes back as a response with its status and body, not
/// as an error (docs/transport.md). And it carries the identity a turn carries
/// — a catalog scoped to an account answers only to that account's headers, and
/// the `anthropic-version` the Models API requires is part of a turn's headers
/// too, not an afterthought (an earlier draft set both after the request went
/// out, and Anthropic answered 400 "header is required").
#[tokio::test]
async fn a_probe_reports_a_refusal_and_carries_the_turns_identity() {
    use crate::testing::upstream::{Canned, ScriptedUpstream};

    const LISTING: &str = "/backend-api/codex/models";

    let refusal = r#"{"error":{"message":"Missing scopes: api.model.read"}}"#;
    let upstream = ScriptedUpstream::answering([Canned::json(403, refusal)]);
    let codex = Endpoint::from_credentials(
        Provider::OpenAICodex,
        "tok-secret",
        Some("acct-1".into()),
        None,
    )
    .unwrap()
    .with_http(upstream.http());
    let response = codex.probe(&ScriptedUpstream::url(LISTING)).await.unwrap();
    assert_eq!(response.status, Some(403));
    assert!(
        response.body.contains("api.model.read"),
        "{}",
        response.body
    );
    assert_eq!(response.etag, None);
    let request = &upstream.requests()[0];
    assert_eq!(request.header("authorization"), Some("Bearer tok-secret"));
    assert_eq!(request.header("chatgpt-account-id"), Some("acct-1"));
    assert_eq!(request.header("originator"), Some("efference"));

    let listing = r#"{"data":[{"id":"m"}]}"#;
    let upstream =
        ScriptedUpstream::answering([Canned::json(200, listing).with_header("etag", "W/\"abc\"")]);
    let anthropic = Endpoint::from_credentials(Provider::Anthropic, "sk-ant-x", None, None)
        .unwrap()
        .with_http(upstream.http());
    let response = anthropic
        .probe(&ScriptedUpstream::url(LISTING))
        .await
        .unwrap();
    assert_eq!(response.status, Some(200));
    assert_eq!(response.etag.as_deref(), Some("W/\"abc\""));
    assert_eq!(response.body, listing);
    let request = &upstream.requests()[0];
    assert_eq!(request.header("x-api-key"), Some("sk-ant-x"));
    assert!(request.header("anthropic-version").is_some(), "{request:?}");
    assert_eq!(
        request.header("originator"),
        None,
        "a provider with no attribution sends none"
    );
}

/// Every provider names the route its own listing is read from, and the two
/// OpenAI arms are the only ones whose credential and catalog do not travel
/// together: the platform listing wants a platform key's `api.model.read`
/// scope, and a ChatGPT grant answers the subscription backend's route instead
/// (docs/transport.md).
#[test]
fn each_provider_names_its_own_listing_route() {
    assert_eq!(
        Provider::Anthropic.models_url(None).as_deref(),
        Some("https://api.anthropic.com/v1/models?limit=100")
    );
    assert_eq!(
        Provider::OpenRouter.models_url(None).as_deref(),
        Some("https://openrouter.ai/api/v1/models")
    );
    assert_eq!(
        Provider::OpenAI.models_url(None).as_deref(),
        Some("https://api.openai.com/v1/models")
    );
    assert_eq!(
        Provider::OpenAICodex
            .models_url(None)
            .as_deref()
            .and_then(|url| url.split('?').next()),
        Some("https://chatgpt.com/backend-api/codex/models"),
        "the route, without the version claim this build adds to it"
    );
    // A described endpoint's listing hangs off the base the operator gave, the
    // same route its credential check and capability lookup use.
    assert_eq!(
        Provider::Custom(WireDialect::Responses)
            .models_url(Some("https://api.example.com/v1"))
            .as_deref(),
        Some("https://api.example.com/v1/models")
    );
    assert_eq!(
        Provider::Custom(WireDialect::Responses).models_url(None),
        None
    );
}

/// A 401 that is *not* expiry keeps its terminal classification: re-reading a
/// key the provider rejects as wrong produces the same rejection.
/// A 401 that is *not* expiry keeps its terminal classification: re-reading a
/// key the provider rejects as wrong produces the same rejection.
#[test]
fn a_rejected_key_is_terminal_and_an_expired_one_is_not() {
    use crate::api::failure::{FailureClass, classify_body};

    assert_eq!(
        classify_body("{\"error\":{\"code\":\"invalid_api_key\"}}"),
        FailureClass::Terminal
    );
    assert_eq!(
        classify_body("{\"error\":{\"code\":\"token_expired\"}}"),
        FailureClass::CredentialExpired
    );
    assert_eq!(
        classify_body("{\"detail\":{\"code\":\"token_expired\"}}"),
        FailureClass::Transient,
        "only the documented fields name the class; `detail` is not one of them"
    );
}
