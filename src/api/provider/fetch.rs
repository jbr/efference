//! The provider lookups an [`Endpoint`] makes over HTTP: probes, the boot
//! capability fetch, the key check, and the generation-cost lookup
//! (docs/transport.md).

use super::*;

impl Endpoint {
    /// The route listing the models this endpoint's provider serves, for a
    /// discovery menu and for `efference dev models` (docs/transport.md).
    pub fn models_url(&self) -> Option<String> {
        self.provider.models_url(self.base.as_deref())
    }

    /// An authenticated GET reported whole: the status, the `ETag` a catalog can
    /// be revalidated against, and the body as the provider sent it. A non-2xx
    /// answer is a result here rather than a failure — whether a response is
    /// usable is the question a listing probe asks (`efference dev models`,
    /// docs/transport.md).
    ///
    /// Carries the identity a turn carries, because a listing scoped to an
    /// account answers only to it: the auth header, the provider's extra
    /// headers, and its attribution. Nothing per-turn rides along — there is no
    /// conversation to route or cache-key on a listing read.
    pub async fn probe(&self, url: &str) -> Result<ProbeResponse, Error> {
        let http = crate::http::client();
        let auth = self.auth_value.current().await;
        // Every header is set before the request is sent: a header added to an
        // in-flight connection is not part of the request that went out.
        let mut conn = self.authorize(http.get(url), auth);
        // OpenRouter answers a read carrying `anthropic-version` in Anthropic's
        // paginated listing shape (20 rows, ids re-prefixed `anthropic/…[1m]`)
        // rather than its own catalog, so its reads go without it.
        if self.provider.inference_dialect() == Some(WireDialect::Messages)
            && self.provider != Provider::OpenRouter
        {
            conn = conn.with_request_header("anthropic-version", ANTHROPIC_VERSION);
        }
        let conn = self
            .endpoint_extra_headers()
            .into_iter()
            .fold(conn, |conn, (name, value)| {
                conn.with_request_header(name, value)
            });
        let mut conn = conn
            .await
            .map_err(|e| -> Error { format!("GET {url} failed: {e}").into() })?;
        let status = conn.status().map(|status| status as u16);
        let etag = conn.response_headers().get_str("etag").map(str::to_string);
        let body = conn.response_body().read_string().await?;
        Ok(ProbeResponse { status, etag, body })
    }

    /// The provider's identity headers for a non-turn request: its extra
    /// headers (the Codex account id) and its attribution, in the order a turn
    /// sends them.
    fn endpoint_extra_headers(&self) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = self.extra_headers.clone();
        headers.extend(
            self.provider
                .attribution()
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
        );
        headers
    }

    /// The required boot-time metadata lookup (docs/transport.md): GET the
    /// provider's model-metadata route(s) for `model` and parse them into
    /// [`ModelCapabilities`]. Authenticated with the same header as the
    /// messages POSTs (Anthropic's Models API requires the key; OpenRouter
    /// tolerates it). Any failure — network, non-2xx, missing fields — is an
    /// error the caller treats as fatal to startup.
    ///
    /// Anthropic asks one route ([`Provider::capabilities_url`]). OpenRouter
    /// asks two: the endpoints listing for the ceilings, then the singular
    /// model entry ([`Provider::effort_levels_url`]) for the effort levels,
    /// merged in here. Both are fatal at boot — the boot rule validates a
    /// configured effort against this metadata, so "the entry route didn't
    /// answer" must not silently degrade into no validation.
    ///
    /// `upstream` is the session's `--upstream` order (empty when unpinned): a
    /// pinned session's ceilings come from the endpoints it will actually reach,
    /// not from every endpoint serving the slug (docs/transport.md).
    pub async fn fetch_capabilities(
        &self,
        model: &str,
        upstream: &[String],
    ) -> Result<ModelCapabilities, Error> {
        // The two OpenAI arms resolve capabilities from the provider's own model
        // catalog rather than from a table in this build, so a model released
        // after the build is as usable as one released before it.
        match self.provider {
            Provider::OpenAICodex | Provider::OpenAI => {
                return codex::capabilities(self, model).await;
            }
            Provider::Anthropic
            | Provider::OpenRouter
            | Provider::TypeSafe
            | Provider::Custom(_) => {}
        }
        let url = self
            .provider
            .capabilities_url(self.base.as_deref(), model)?;
        let body = self.authed_get(&url).await?;
        // OpenRouter lists decision models beside text ones. One answers typed
        // questions about a state and produces no turn, so a role that runs turns
        // refuses it here, before a Messages request would 4xx on it.
        if self.provider == Provider::OpenRouter
            && matches!(
                openrouter::kind_of_endpoints(&body),
                Ok(ModelKind::Decisions)
            )
        {
            return Err(format!(
                "{model} is a decision model: it answers typed questions about a state and \
                 produces no turn, so it can back the spotter only (docs/transport.md)"
            )
            .into());
        }
        let mut capabilities = self
            .provider
            .parse_capabilities(&body, model, upstream)
            .map_err(|e| -> Error {
                format!("could not parse model metadata from {url}: {e}").into()
            })?;
        if let Some(effort_url) = self.provider.effort_levels_url(model)? {
            let effort_body = self.authed_get(&effort_url).await?;
            let efforts = self
                .provider
                .parse_model_entry(&effort_body)
                .map_err(|e| -> Error {
                    format!("could not parse model metadata from {effort_url}: {e}").into()
                })?;
            // A list this route names is checked against: OpenRouter maps an
            // unsupported level to the nearest supported one silently, so a
            // session could otherwise believe it set a knob that did nothing.
            // An absent list is not a refusal. `openai/o4-mini` names none and
            // honors effort all the same (2,752 reasoning tokens at `low`, the
            // 6,000 cap at `high`, 2026-09-25), so the level passes unchecked
            // and the boot line says so.
            capabilities.validates_efforts = efforts.supported.is_some();
            capabilities.supported_efforts = efforts.supported;
            capabilities.default_effort = efforts.default;
            // Optional reasoning takes the explicit `disabled`; mandatory reasoning
            // refuses it, so off is said by omission there (docs/transport.md).
            capabilities.thinking.off = if efforts.mandatory {
                ThinkingOff::Omit
            } else {
                ThinkingOff::Disabled
            };
        }
        Ok(capabilities)
    }

    /// One request's billed cost in USD, from the provider's own accounting —
    /// authoritative, never modeled from a rate card (cache read/write/TTL
    /// multipliers make modeling brittle; docs/transport.md). OpenRouter keys
    /// it by the generation id every response carries — the same id
    /// `usage.jsonl` already records per request. Anthropic has no
    /// per-request cost surface, so the daemon never calls this there.
    pub async fn fetch_generation_cost(&self, id: &str) -> Result<f64, Error> {
        let url = match self.provider {
            Provider::OpenRouter => {
                format!("https://openrouter.ai/api/v1/generation?id={id}")
            }
            Provider::Anthropic => {
                return Err("anthropic exposes no per-generation cost endpoint".into());
            }
            Provider::OpenAI | Provider::OpenAICodex => {
                return Err("openai exposes no per-generation cost endpoint".into());
            }
            Provider::Custom(_) => {
                return Err("a custom endpoint exposes no per-generation cost endpoint".into());
            }
            Provider::TypeSafe => {
                return Err("typesafe exposes no per-generation cost endpoint".into());
            }
        };
        parse_generation_cost(&self.authed_get(&url).await?)
    }

    /// One authenticated GET against a provider metadata/accounting URL,
    /// returning the body on 2xx and a status+body error otherwise. Draws the
    /// process-wide client (`http.rs`) rather than dialing a fresh connection per
    /// call — `fetch_generation_cost` alone can run thousands of these in a
    /// spotter-eval cost pass, and a fresh `Client::new` per call holds a socket
    /// per lookup for the life of the process. Sharing it with the agent's
    /// `Client` also means these lookups multiplex onto the HTTP/2 connection the
    /// turn stream already holds open to the same host (docs/transport.md).
    /// Whether the credential is accepted, via [`Provider::key_check_url`]. A
    /// provider with no such route is `Ok(None)`: unchecked, not rejected. The
    /// returned text is the route's body for the caller to describe (OpenRouter's
    /// key label, for one).
    pub async fn check_key(&self) -> Result<Option<String>, Error> {
        match self.provider.key_check_url(self.base.as_deref()) {
            Some(url) => self.authed_get(&url).await.map(Some),
            None => Ok(None),
        }
    }

    /// A GET that must answer 2xx: every metadata and credential read on the boot
    /// path, where any other status is a failure to report. The transport is
    /// [`Self::probe`]'s, so these reads carry the same identity a turn does.
    pub(in crate::api) async fn authed_get(&self, url: &str) -> Result<String, Error> {
        let response = self.probe(url).await?;
        match response.status {
            Some(status) if (200..300).contains(&status) => Ok(response.body),
            status => {
                let status =
                    status.map_or_else(|| "no status".to_string(), |status| status.to_string());
                Err(format!("GET {url} returned {status}: {}", response.body).into())
            }
        }
    }
}

/// Parse OpenRouter's `GET /api/v1/generation` response down to `total_cost` —
/// what the account was actually billed for one request. (`upstream_inference_cost`
/// is BYOK-only and stays ignored.) A record without the field is an error the
/// caller counts as an unpriced request, never a silent zero.
pub(in crate::api) fn parse_generation_cost(body: &str) -> Result<f64, Error> {
    #[derive(Deserialize)]
    struct Body {
        data: Generation,
    }
    #[derive(Deserialize)]
    struct Generation {
        total_cost: Option<f64>,
    }
    serde_json::from_str::<Body>(body)?
        .data
        .total_cost
        .ok_or_else(|| "generation record carried no total_cost".into())
}
