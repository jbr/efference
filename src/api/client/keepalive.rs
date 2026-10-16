//! The cache keepalive ping and the triage-lineage primer (docs/transport.md).

use super::*;

/// Why a keepalive ping failed (docs/transport.md). A distinct type from the
/// transport's boxed [`Error`] because the keepalive loop's retry decision is a
/// function of the HTTP status: a request the upstream rejects as malformed will
/// be rejected identically on the next tick, so the loop must be able to read the
/// status structurally rather than parse an error string.
#[derive(Debug)]
pub enum KeepaliveError {
    /// The upstream answered with a non-2xx. `status` is `None` when the response
    /// carried no status line at all; `body` is the upstream's text.
    Status { status: Option<u16>, body: String },
    /// Anything before or after the status: connection failure, unreadable body,
    /// a response that did not parse.
    Other(Error),
}

impl KeepaliveError {
    pub fn other(e: impl Into<Error>) -> Self {
        Self::Other(e.into())
    }

    /// Whether this failure will recur identically on the next ping — the upstream
    /// rejected the request itself, not the moment. True for the 4xx range except
    /// 408 (request timeout) and 429 (rate limit), which describe the moment. 401
    /// and 403 count as permanent here because the client's credential is fixed at
    /// boot, so a rejected credential stays rejected for the life of the session.
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Status {
                status: Some(s), ..
            } => (400..500).contains(s) && *s != 408 && *s != 429,
            _ => false,
        }
    }

    /// Whether the body names a condition no retry changes before it resets — the
    /// Codex backend's `usage_limit_reached` arrives as a 429, which the status alone
    /// reads as a moment (`api/failure.rs`). The same request would succeed after a
    /// reset, so this is not [`is_permanent`](Self::is_permanent).
    pub fn is_exhausted(&self) -> bool {
        match self {
            Self::Status { body, .. } => classify_body(body) == FailureClass::Terminal,
            Self::Other(_) => false,
        }
    }

    /// When an exhausted limit resets, in epoch milliseconds, where the body says
    /// (`error.resets_at`, in seconds, on the Codex backend).
    pub fn resets_at(&self) -> Option<u64> {
        let Self::Status { body, .. } = self else {
            return None;
        };
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        let secs = value.get("error")?.get("resets_at")?.as_u64()?;
        Some(secs.saturating_mul(1000))
    }
}

impl std::fmt::Display for KeepaliveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status {
                status: Some(status),
                body,
            } => write!(f, "keepalive returned {status}: {body}"),
            Self::Status { status: None, body } => {
                write!(f, "keepalive returned no status: {body}")
            }
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for KeepaliveError {}

impl Client {
    /// A cache-keepalive ping (docs/transport.md): a **non-streaming**
    /// round-trip over the same prefix a real turn would send, capped at
    /// [`Provider::keepalive_output_cap`] (`max_tokens: 0` on Anthropic, 16 on
    /// OpenRouter, which reads 0 as unset). Returns only the `usage`; the log is never touched.
    ///
    /// Non-streaming because `max_tokens: 0` is rejected with `stream: true`. The
    /// prefix bytes must match a real request's **exactly** or the ping writes a
    /// *new* cache entry instead of refreshing the live one — which is why it
    /// takes the caller's request — turns, `tools`, and `tool_choice` — and reuses the
    /// client's own `system`, rather than reconstructing any of them. A request the
    /// forced-choice fallback sent ([`SentRequest::unforced`]) goes out as the fallback
    /// did, thinking off.
    ///
    /// On the Responses and Completions dialects the ping is an ordinary streamed
    /// request with the output capped at [`Provider::keepalive_output_cap`]
    /// ([`Self::streamed_keepalive`]). `origin` tags the ping's stages: `Keepalive` for
    /// the main loop, `TriageKeepalive` for the triage lineage (docs/transport.md).
    pub async fn keepalive(
        &self,
        origin: RequestOrigin,
        request: &SentRequest,
    ) -> Result<RequestUsage, KeepaliveError> {
        let unforced;
        let client = if request.unforced {
            unforced = self.unforced();
            &unforced
        } else {
            self
        };
        client
            .ping(origin, &request.turns, &request.tools, request.choice)
            .await
    }

    /// [`Self::keepalive`] on the client the request goes out on.
    async fn ping(
        &self,
        origin: RequestOrigin,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Result<RequestUsage, KeepaliveError> {
        if matches!(
            self.dialect(),
            WireDialect::Responses | WireDialect::Completions
        ) {
            return self
                .with_origin(origin)
                .streamed_keepalive(turns, tools, tool_choice)
                .await;
        }
        let messages = encode_messages(turns, self.image_placement(), self.breakpoint_placement());
        // The turn's own envelope, thinking and effort included (docs/transport.md): on
        // Anthropic a ping whose thinking differs from the turn's misses the
        // message-level cache and writes a fresh entry instead of refreshing the live
        // one. `max_tokens: 0` takes thinking on and any effort.
        let request = self.messages_request(
            &messages,
            tools,
            tool_choice,
            self.endpoint.provider.keepalive_output_cap(),
            false,
        );
        let body = serde_json::to_string(&request).map_err(KeepaliveError::other)?;
        // Non-streaming, so the ping has no first byte: it is sent, then it ends or fails.
        let trace = Trace::begin(self.stages.as_ref(), origin, &self.active_trace);
        if let Some(trace) = &trace {
            trace.sent();
        }
        let result = self.keepalive_exchange(body).await;
        if let Some(trace) = trace {
            match &result {
                Ok(_) => trace.ended(None),
                Err(e) => trace.failed(&e.to_string()),
            }
        }
        result
    }

    /// The Messages keepalive's POST and read, split from [`Self::keepalive`] so the
    /// trace closes on every exit.
    async fn keepalive_exchange(&self, body: String) -> Result<RequestUsage, KeepaliveError> {
        let conn = self.post();
        let mut conn = conn.with_body(body).await.map_err(KeepaliveError::other)?;
        let status = conn.status().map(|s| s as u16);
        if !status.is_some_and(|s| (200..300).contains(&s)) {
            let body = read_error_body(&mut conn)
                .await
                .map_err(KeepaliveError::other)?;
            return Err(KeepaliveError::Status { status, body });
        }
        let text = conn
            .response_body()
            .read_string()
            .await
            .map_err(KeepaliveError::other)?;
        serde_json::from_str::<KeepaliveResponse>(&text)
            .map(KeepaliveResponse::into_request_usage)
            .map_err(KeepaliveError::other)
    }

    /// The streamed keepalive (docs/transport.md): the turn's own envelope, streamed,
    /// with the output capped at [`Provider::keepalive_output_cap`]. Neither Responses
    /// nor Completions has a `max_tokens: 0`, and the Codex backend resumes a cached prefix only where an
    /// earlier request ended, so the ping re-sends the last request's input whole —
    /// what the keepalive loop's lopped projection is — and pays a cached read plus
    /// the capped output.
    async fn streamed_keepalive(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Result<RequestUsage, KeepaliveError> {
        let completion = self
            .send(
                turns,
                tools,
                tool_choice,
                Some(self.endpoint.provider.keepalive_output_cap()),
                |_| {},
            )
            .await
            .map_err(|e| match e.downcast_ref::<OpenError>() {
                Some(open) => KeepaliveError::Status {
                    status: open.status(),
                    body: open.to_string(),
                },
                None => KeepaliveError::Other(e),
            })?;
        if let Some(why) = completion.upstream_failure() {
            return Err(KeepaliveError::other(why.to_string()));
        }
        Ok(completion.request_usage())
    }

    /// Whether this client can plant a triage-lineage primer (docs/transport.md): a
    /// Responses lineage with an implicit cache. [`Self::prime`] keys on it, and so does
    /// what the triage keepalive re-sends: the primer where it plants, the last result
    /// triage's request elsewhere.
    pub(crate) fn plants_primer(&self) -> bool {
        self.dialect() == WireDialect::Responses && self.prompt_cache == PromptCachePolicy::Implicit
    }

    /// The triage-lineage primer (docs/transport.md): the turn's
    /// own input replayed so a later triage extends a request that ended where the main
    /// one did. Shaped exactly like the keepalive's Responses ping — the turn envelope
    /// with the backend-minimum output cap — because it is the same write: a cached
    /// read plus the capped output. `None` where no implicit lineage exists to plant
    /// in (anything but Responses-with-implicit-policy), so the caller sends nothing
    /// speculative there; the triage then goes out unprimed, the failure mode being fixed.
    ///
    /// The primer wants the write, not the read, so a marked output does not fail it:
    /// a 16-token budget at a call-eliciting position truncates the model's function
    /// call mid-arguments, which marks the completion without unmaking its input
    /// prefix — the provider still processed (and wrote) the whole replayed prefix.
    /// Only a response with no accounted input is evidence nothing was planted.
    pub(crate) async fn prime(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Option<Result<RequestUsage, Error>> {
        if !self.plants_primer() {
            return None;
        }
        let completion = match self
            .with_origin(RequestOrigin::TriagePrimer)
            .send(
                turns,
                tools,
                tool_choice,
                Some(self.endpoint.provider.keepalive_output_cap()),
                |_| {},
            )
            .await
        {
            Ok(completion) => completion,
            Err(e) => return Some(Err(e)),
        };
        let usage = completion.request_usage();
        if usage.usage.input_tokens == 0 {
            let why = completion
                .upstream_failure()
                .unwrap_or("primer response carried no usage")
                .to_string();
            return Some(Err(Box::new(std::io::Error::other(why))));
        }
        Some(Ok(usage))
    }
}

/// The non-streamed Messages body a keepalive ping returns: the id, model and serving
/// upstream, the `usage` with its inline cost, and — since a ping sends the cache
/// diagnostics opt-in like any other request on the main prefix — the provider's own
/// attribution of a cache miss (docs/transport.md).
#[derive(Deserialize)]
pub(crate) struct KeepaliveResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    usage: UsageWithCost,
    #[serde(default)]
    diagnostics: Option<CacheDiagnosticsResult>,
}

impl KeepaliveResponse {
    pub(crate) fn into_request_usage(self) -> RequestUsage {
        RequestUsage {
            id: self.id,
            model: self.model,
            usage: self.usage.usage,
            cache_miss: self.diagnostics.and_then(|d| d.cache_miss_reason),
            receipt: Receipt {
                served_by: self.provider,
                cost: self.usage.cost.into_inline(),
            },
        }
    }
}
