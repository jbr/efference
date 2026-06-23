//! The Anthropic Messages API wire — request/response shapes and the HTTP call.
//!
//! Phase 0: non-streaming. The request `messages` are produced by
//! [`crate::project::project`]; the response is parsed back into a
//! [`Payload::Assistant`] so the result folds straight into the log — this is
//! where the substrate and the transport meet. Streaming (SSE), `system`,
//! `tools`, and `cache_control` arrive in later slices.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trillium_client::Client as HttpClient;
use trillium_rustls::RustlsConfig;
use trillium_tokio::ClientConfig;

use crate::log::{CallId, ContentBlock, Payload, ToolCall};
use crate::project::ApiMessage;

const MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Boxed error for the Phase 0 transport. A typed error enum can replace this
/// once error handling becomes load-bearing; for now non-2xx responses carry
/// the raw body in the message so failures are legible.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: &'a [ApiMessage],
}

/// The Anthropic `usage` object. The two `cache_*` fields are the Phase 0 gate
/// metrics; defaulted because they're absent unless caching is in play.
#[derive(Deserialize, Debug, Clone)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Deserialize)]
struct Response {
    content: Vec<ResponseBlock>,
    model: String,
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    // Anthropic may emit other block types (e.g. `thinking` once we move to
    // Opus adaptive thinking). Those will surface as a hard parse error here
    // until a later slice models them — deliberately loud, never silently
    // dropped.
}

/// A parsed assistant turn plus the metadata we surface and log.
pub struct Completion {
    /// Always a [`Payload::Assistant`], ready to append to the log.
    pub assistant: Payload,
    pub model: String,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

impl Response {
    fn into_completion(self) -> Completion {
        let mut text = Vec::new();
        let mut calls = Vec::new();
        for block in self.content {
            match block {
                ResponseBlock::Text { text: t } => text.push(ContentBlock::Text(t)),
                ResponseBlock::ToolUse { id, name, input } => calls.push(ToolCall {
                    id: CallId(id),
                    name,
                    input,
                }),
            }
        }
        Completion {
            assistant: Payload::Assistant { text, calls },
            model: self.model,
            stop_reason: self.stop_reason,
            usage: self.usage,
        }
    }
}

/// A thin Anthropic Messages client: holds the HTTP stack, key, and model.
pub struct Client {
    http: HttpClient,
    api_key: String,
    model: String,
}

impl Client {
    pub fn new(api_key: String, model: impl Into<String>) -> Self {
        Self {
            // TLS over the tokio TCP connector; ALPN negotiates HTTP/2 (no h3
            // advertised by Anthropic/Cloudflare as of 2026-06). See main.rs.
            http: HttpClient::new(RustlsConfig::<ClientConfig>::default()),
            api_key,
            model: model.into(),
        }
    }

    /// One non-streaming round-trip: serialize `messages`, POST, and parse the
    /// response into a [`Completion`]. Non-2xx responses return an error whose
    /// message includes the raw body.
    pub async fn complete(&self, messages: &[ApiMessage]) -> Result<Completion, Error> {
        let request = Request {
            model: &self.model,
            max_tokens: 1024,
            messages,
        };
        let body = serde_json::to_string(&request)?;

        let mut conn = self
            .http
            .post(MESSAGES_ENDPOINT)
            .with_request_header("x-api-key", self.api_key.clone())
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .with_request_header("content-type", "application/json")
            .with_body(body)
            .await?;

        let status = conn.status();
        let raw = conn.response_body().read_string().await?;
        if !status.is_some_and(|s| s.is_success()) {
            return Err(format!("Anthropic API returned {status:?}: {raw}").into());
        }

        let response: Response = serde_json::from_str(&raw)?;
        Ok(response.into_completion())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses a canned response containing both a text block and a tool call —
    /// the path the bash tool will depend on — into a `Payload::Assistant`.
    #[test]
    fn parses_text_and_tool_use_into_assistant() {
        let canned = r#"{
            "content": [
                {"type": "text", "text": "let me look"},
                {"type": "tool_use", "id": "toolu_9", "name": "bash", "input": {"command": "ls"}}
            ],
            "model": "claude-haiku-4-5",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 3, "output_tokens": 4}
        }"#;

        let completion = serde_json::from_str::<Response>(canned)
            .unwrap()
            .into_completion();

        assert_eq!(completion.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(completion.usage.input_tokens, 3);
        // cache_* default to 0 when absent.
        assert_eq!(completion.usage.cache_read_input_tokens, 0);

        match completion.assistant {
            Payload::Assistant { text, calls } => {
                assert_eq!(text, vec![ContentBlock::Text("let me look".into())]);
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "bash");
                assert_eq!(calls[0].id.0, "toolu_9");
                assert_eq!(calls[0].input, serde_json::json!({ "command": "ls" }));
            }
            other => panic!("expected Payload::Assistant, got {other:?}"),
        }
    }
}
