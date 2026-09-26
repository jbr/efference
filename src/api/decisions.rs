//! The Decisions route — the wire for a structured-decision model
//! (docs/transport.md, docs/permissions.md).
//!
//! A decision model is an LLM with generation replaced by a classifier head: it
//! reads a `state` and answers named `questions` with a calibrated probability
//! each. There is no system prompt, no tool call, no token stream, and no
//! response text — so none of [`Client`](super::Client) applies. One POST of
//! `{model, state, questions}`, one JSON body back:
//!
//! ```text
//! { "id": …, "answers": { "in_scope": { "type": "noul", "noul": 0.91 } }, "usage": … }
//! ```
//!
//! `noul` is the API's name for the boolean question type, on both the asking
//! and the answering side; it is spelled as the wire spells it rather than
//! renamed, so a request built here reads the same as the provider's docs.
//!
//! Nothing on this route caches: the model's endpoint metadata reports
//! `supports_implicit_caching: false` and there are no cache-control markers to
//! send, so the byte-stability rule that governs the Messages prefix
//! (docs/log-and-projection.md) has nothing to hold here. Input is billed at a
//! flat rate and output is free.

use std::collections::BTreeMap;
use std::time::Duration;

use trillium::Status;
use trillium_client_retry::RetryHandler;

use super::*;

/// How long a decisions request may spend retrying before the spotter gives up
/// and the gate falls to the human (docs/permissions.md). Far shorter than the
/// inference client's budget: a turn waiting on a model has nothing better to do,
/// but a gate has a fallback that is always available and often faster — an
/// operator at the keyboard answers a prompt in less time than a long backoff.
/// The budget exists to absorb a brief rate-limit burst, which otherwise turns
/// every gated call in it into a human gate.
const DECIDE_MAX_ATTEMPTS: u32 = 3;
const DECIDE_MAX_ELAPSED: Duration = Duration::from_secs(5);

/// The decisions route's retry policy: the route's docs prescribe backoff on
/// 429 and on server overload. The POST is replay-safe — a decision that did not
/// answer billed nothing and changed nothing. A `Retry-After` longer than the
/// whole budget is not waited out; the failure surfaces and the gate falls back.
pub(super) fn decisions_retry() -> RetryHandler {
    RetryHandler::default()
        .with_all_methods()
        .with_statuses([
            Status::TooManyRequests,
            Status::InternalServerError,
            Status::BadGateway,
            Status::ServiceUnavailable,
            Status::GatewayTimeout,
        ])
        .with_max_attempts(DECIDE_MAX_ATTEMPTS)
        .with_max_elapsed(DECIDE_MAX_ELAPSED)
        .with_max_retry_after(DECIDE_MAX_ELAPSED)
}

/// One question in a decisions request. Boolean-only today because that is what
/// the spotter asks (docs/permissions.md); the route also defines `choice` and
/// `score` question types, which land here when something asks one.
#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum Question {
    /// The route's boolean question. `criteria` names what makes the answer true
    /// and what makes it false — both are required by the API, and the pair is
    /// where the calibration comes from, so neither side is optional here either.
    #[serde(rename = "noul")]
    Boolean {
        instructions: String,
        criteria: BooleanCriteria,
    },
}

impl Question {
    /// A boolean question from its three strings, in the order they read:
    /// what is being asked, what makes it true, what makes it false.
    pub fn boolean(
        instructions: impl Into<String>,
        when_true: impl Into<String>,
        when_false: impl Into<String>,
    ) -> Self {
        Question::Boolean {
            instructions: instructions.into(),
            criteria: BooleanCriteria {
                r#true: when_true.into(),
                r#false: when_false.into(),
            },
        }
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct BooleanCriteria {
    pub r#true: String,
    pub r#false: String,
}

/// One question's answer, read leniently. Every field beyond `type` is optional
/// because which one carries the answer depends on the question type, and a
/// consumer that asked a boolean reads `noul` and ignores the rest. Lenient
/// rather than a tagged enum so an answer shape this harness doesn't model yet
/// doesn't fail the whole response away — the caller's `None` handling is the
/// same fail-closed path a transport error takes.
#[derive(Deserialize, Debug, Clone)]
pub struct Answer {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// The boolean answer: P(true), in `[0, 1]`. Not thresholded upstream — the
    /// model reports its calibration and the caller decides what to do with it.
    pub noul: Option<f64>,
    pub choice: Option<String>,
    pub score: Option<f64>,
    pub confidence: Option<f64>,
}

/// What the route bills for one request. Output is free on this model family and
/// reported as 0; `cost` is the provider's own figure, carried through rather
/// than modeled (docs/transport.md).
#[derive(Deserialize, Debug, Clone, Default)]
pub struct DecisionsUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost: Option<f64>,
}

/// One decisions response.
#[derive(Deserialize, Debug, Clone)]
pub struct Decisions {
    /// The generation id — the same key `usage.jsonl` joins a request's cost on
    /// (docs/log-and-projection.md). Optional defensively, as elsewhere.
    pub id: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: DecisionsUsage,
}

impl Decisions {
    /// P(true) for a boolean question by name, or `None` when the question went
    /// unanswered or came back as some other shape.
    pub fn probability(&self, question: &str) -> Option<f64> {
        self.answers.get(question)?.noul
    }

    /// This request's spend, in the shape every other request reports
    /// (docs/log-and-projection.md). The cache counts stay 0 because the route
    /// has no cache — a truthful zero, not a missing reading, which is what keeps
    /// a decisions request out of the cold-streak classification.
    pub fn request_usage(&self) -> RequestUsage {
        RequestUsage {
            id: self.id.clone(),
            model: self.model.clone(),
            usage: Usage {
                input_tokens: self.usage.input_tokens,
                output_tokens: self.usage.output_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        }
    }
}

#[derive(Serialize)]
struct DecisionsRequest<'a> {
    model: &'a str,
    state: &'a Value,
    questions: &'a BTreeMap<String, Question>,
    /// The sticky key this endpoint already carries, for grouping in the
    /// provider's own accounting. Omitted where there is no session, and where
    /// the provider does not document the field: the first-party API answers an
    /// unrecognized top-level key with a 422, so an undocumented extra is a
    /// failed permission check rather than a harmless decoration.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

impl Endpoint {
    /// Ask `model` the `questions` about `state`.
    ///
    /// Unlike the streaming routes this is one request and one body, so there is
    /// no assembler and no partial: it either answers or errors. The caller's
    /// error path is the spotter's fail-closed gate (docs/permissions.md); a
    /// short [`decisions_retry`] budget absorbs a rate-limit burst before that
    /// path is taken.
    pub async fn decide(
        &self,
        model: &str,
        state: &Value,
        questions: &BTreeMap<String, Question>,
    ) -> Result<Decisions, Error> {
        let url = self.provider.decisions_url().ok_or_else(|| -> Error {
            format!(
                "provider {} serves no decisions route, so it cannot run a decision model \
                 (docs/permissions.md)",
                self.provider
            )
            .into()
        })?;
        let body = serde_json::to_string(&DecisionsRequest {
            model,
            state,
            questions,
            session_id: self
                .provider
                .decisions_takes_session_id()
                .then_some(self.session_id.as_deref())
                .flatten(),
        })?;
        let http = crate::http::client().with_handler(decisions_retry());
        let conn = http
            .post(url)
            .with_request_header(self.auth_header.clone(), self.auth_value.get())
            .with_request_header("content-type", "application/json")
            .with_body(body);
        // Billed inference, so it carries the same attribution the inference
        // POSTs do (`Provider::attribution`) — the metadata GETs deliberately
        // don't, and this is not one of those.
        let conn = self
            .provider
            .attribution()
            .iter()
            .fold(conn, |conn, (name, value)| {
                conn.with_request_header(*name, *value)
            });
        let mut conn = conn
            .await
            .map_err(|e| -> Error { format!("POST {url} failed: {e}").into() })?;
        let status = conn.status();
        let response = conn.response_body().read_string().await?;
        if !status.is_some_and(|s| s.is_success()) {
            let status = status.map_or_else(|| "no status".to_string(), |s| s.to_string());
            return Err(format!("POST {url} returned {status}: {response}").into());
        }
        serde_json::from_str(&response)
            .map_err(|e| -> Error { format!("could not parse decisions response: {e}").into() })
    }
}

#[cfg(test)]
mod test;
