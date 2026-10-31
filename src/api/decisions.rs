//! The Decisions route — the wire for a structured-decision model
//! (docs/transport.md, docs/permissions.md).
//!
//! A decision model is an LLM with generation replaced by a classifier head: it
//! reads a `state` and answers named `questions` with a calibrated probability
//! each. There is no system prompt, no tool call, no token stream, and no
//! response text — so none of [`Client`] applies. One POST of
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

use super::stages::{ActiveTrace, StageSink, Trace};
use super::*;
use crate::log::RequestOrigin;

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

/// One question in a decisions request (docs/permissions.md). The route defines
/// three types: a boolean (`noul`), a `score` over ordered levels, and a `choice`
/// among named options.
///
/// `instructions` is a string or an object ([`Instructions`]); an object lets a
/// question carry the text it is about (an earlier call, say) beside the question
/// itself, without a state field per candidate.
#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum Question {
    /// The route's boolean question. `criteria` names what makes the answer true
    /// and what makes it false — both are required by the API, and the pair is
    /// where the calibration comes from, so neither side is optional here either.
    #[serde(rename = "noul")]
    Boolean {
        instructions: Instructions,
        criteria: BooleanCriteria,
    },
    /// A question answered with a position on an ordered scale. `criteria` is one
    /// description per level, lowest first; the answer's `probabilities` are keyed
    /// by the level's index.
    #[serde(rename = "score")]
    Score {
        instructions: Instructions,
        criteria: Vec<String>,
    },
    /// A question answered with one of several named options. `criteria` maps each
    /// option to its description, and the order they are sent in is part of the
    /// question: the model favors the first option, so a caller that wants a
    /// position-free reading asks twice with the options reversed.
    #[serde(rename = "choice")]
    Choice {
        instructions: Instructions,
        criteria: OptionCriteria,
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
            instructions: Instructions::Text(instructions.into()),
            criteria: BooleanCriteria {
                r#true: when_true.into(),
                r#false: when_false.into(),
            },
        }
    }

    /// A score question from what is being asked and its levels, lowest first.
    pub fn score<L: Into<String>>(
        instructions: impl Into<Instructions>,
        levels: impl IntoIterator<Item = L>,
    ) -> Self {
        Question::Score {
            instructions: instructions.into(),
            criteria: levels.into_iter().map(Into::into).collect(),
        }
    }

    /// A choice question from what is being asked and its `(option, description)`
    /// pairs, in the order they go on the wire.
    pub fn choice<K: Into<String>, V: Into<String>>(
        instructions: impl Into<Instructions>,
        options: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Question::Choice {
            instructions: instructions.into(),
            criteria: OptionCriteria(
                options
                    .into_iter()
                    .map(|(option, description)| (option.into(), description.into()))
                    .collect(),
            ),
        }
    }
}

/// What a question asks: a string, or an object of named strings. The object keeps
/// the order it was built in on the wire, for the same reason [`OptionCriteria`]
/// does: the model reads the fields in sequence, so which comes first is part of
/// the prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum Instructions {
    Text(String),
    Fields(Vec<(String, String)>),
}

impl Instructions {
    /// An object of `(name, text)` fields, in the order they go on the wire.
    pub fn fields<K: Into<String>, V: Into<String>>(
        fields: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Instructions::Fields(
            fields
                .into_iter()
                .map(|(name, text)| (name.into(), text.into()))
                .collect(),
        )
    }
}

impl From<&str> for Instructions {
    fn from(text: &str) -> Self {
        Instructions::Text(text.to_string())
    }
}

impl From<String> for Instructions {
    fn from(text: String) -> Self {
        Instructions::Text(text)
    }
}

impl Serialize for Instructions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Instructions::Text(text) => serializer.serialize_str(text),
            Instructions::Fields(fields) => ordered_map(serializer, fields),
        }
    }
}

fn ordered_map<S: serde::Serializer>(
    serializer: S,
    pairs: &[(String, String)],
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(pairs.len()))?;
    for (key, value) in pairs {
        map.serialize_entry(key, value)?;
    }
    map.end()
}

#[derive(Serialize, Debug, Clone)]
pub struct BooleanCriteria {
    pub r#true: String,
    pub r#false: String,
}

/// A choice's options with their descriptions, serialized as a JSON object in the
/// order held. `serde_json::Map` sorts its keys here (the determinism rule in
/// docs/log-and-projection.md depends on that), so the order is carried as pairs
/// and written entry by entry.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionCriteria(pub Vec<(String, String)>);

impl Serialize for OptionCriteria {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ordered_map(serializer, &self.0)
    }
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
    /// A `choice` or `score` answer's distribution: option name (or level index,
    /// as a string) to probability.
    pub probabilities: Option<BTreeMap<String, f64>>,
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

    /// The distribution a `choice` or `score` question answered with, or `None`
    /// when the question went unanswered or came back as some other shape.
    pub fn probabilities(&self, question: &str) -> Option<&BTreeMap<String, f64>> {
        self.answers.get(question)?.probabilities.as_ref()
    }

    /// This request's spend, in the shape every other request reports
    /// (docs/log-and-projection.md). The cache counts stay 0 because the route
    /// has no cache — a truthful zero, not a missing reading, which is what keeps
    /// a decisions request out of the cold-streak classification.
    pub fn request_usage(&self) -> RequestUsage {
        RequestUsage {
            receipt: Default::default(),
            id: self.id.clone(),
            model: self.model.clone(),
            usage: Usage {
                input_tokens: self.usage.input_tokens,
                output_tokens: self.usage.output_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            // The route carries no prompt cache, so it has no cache chain to draw a
            // comparison through (docs/transport.md).
            cache_miss: None,
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

/// The names a decisions provider's model listing offers. The first-party
/// listing is `{"models": [{"name": …}]}` — neither the OpenAI `data[].id` shape
/// nor anything the custom-endpoint listing parse reads — and it names aliases
/// (`jev-latest`) alongside versions, so a configured alias matches exactly.
pub(super) fn listed_decision_models(body: &str) -> Result<Vec<String>, Error> {
    #[derive(Deserialize)]
    struct Listing {
        models: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        name: String,
    }
    let listing: Listing = serde_json::from_str(body)
        .map_err(|e| -> Error { format!("could not parse the model listing: {e}").into() })?;
    Ok(listing.models.into_iter().map(|m| m.name).collect())
}

impl Endpoint {
    /// The existence check for a model on a provider that serves only decision
    /// models (docs/permissions.md). A decision model has no context window or
    /// output cap, so the capability lookup that checks an inference model cannot
    /// check it; without this a misspelled id boots clean and then fails every
    /// gated call closed. The first-party listing names every id, aliases
    /// included, and the listing GET is authenticated, so it is the credential
    /// check too. OpenRouter's check is [`Endpoint::spotter_kind`].
    pub async fn check_decision_model(&self, model: &str) -> Result<(), Error> {
        // A described System One endpoint publishes no listing and no per-model
        // route, so there is nothing to check an id against; a misspelled one
        // fails closed at the first gated call.
        if self.provider == Provider::Custom(WireDialect::SystemOne) {
            return Ok(());
        }
        let url = self.models_url().ok_or_else(|| -> Error {
            format!("provider {} publishes no model listing", self.provider).into()
        })?;
        let listed = listed_decision_models(&self.authed_get(&url).await?)?;
        if listed.iter().any(|name| name == model) {
            return Ok(());
        }
        Err(format!(
            "{} lists no model {model:?}; it lists {}",
            self.provider,
            listed.join(", ")
        )
        .into())
    }

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
        self.decide_traced(None, RequestOrigin::Spot, model, state, questions)
            .await
    }

    /// [`Self::decide`], reporting the request's stages to `stages` under `origin`
    /// (docs/log-and-projection.md): `sent` before the POST, `first_byte` when the
    /// response arrives, then `ended` once its body parsed, or `failed` with the error.
    /// The route is one request and one body, so there is no stream to open: the
    /// response's arrival is the first byte. Retries inside the transport's budget
    /// record no `attempt_failed`; the retry handler here is not wired to the trace.
    pub async fn decide_traced(
        &self,
        stages: Option<&StageSink>,
        origin: RequestOrigin,
        model: &str,
        state: &Value,
        questions: &BTreeMap<String, Question>,
    ) -> Result<Decisions, Error> {
        let trace = Trace::begin(stages, origin, &ActiveTrace::default());
        if let Some(trace) = &trace {
            trace.sent();
        }
        let result = self
            .post_decisions(trace.as_ref(), model, state, questions)
            .await;
        if let Some(trace) = trace {
            match &result {
                Ok(_) => trace.ended(None),
                Err(error) => trace.failed(&error.to_string()),
            }
        }
        result
    }

    async fn post_decisions(
        &self,
        trace: Option<&Trace>,
        model: &str,
        state: &Value,
        questions: &BTreeMap<String, Question>,
    ) -> Result<Decisions, Error> {
        let url = self
            .provider
            .decisions_url(self.base.as_deref())
            .ok_or_else(|| -> Error {
                format!(
                    "provider {} serves no decisions route, so it cannot run a decision \
                     model",
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
        let auth = self.auth_value.current().await;
        let conn = self
            .authorize(http.post(url.as_str()), auth)
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
        if let Some(trace) = trace {
            trace.first_byte();
        }
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
