//! One streamed request, from the encoded turns to the folded [`Completion`]
//! (docs/transport.md): encode once, open the stream (retrying a reservation or
//! credential refusal), read it to its end, then settle the completion.

use super::*;

use super::upstream_status::record_upstream_failure;
use std::pin::Pin;
use tokio::sync::futures::Notified;

impl Client {
    /// [`Self::complete`], with the output cap fixed at `cap` instead of reserved
    /// against the window when one is given. A fixed cap is sent as-is, also on the
    /// Codex profile that otherwise omits it, and a refusal of it is not retried at
    /// another value: the caller chose it (the Responses keepalive, docs/transport.md).
    /// Reached through [`Self::send`], which handles an image refusal.
    pub(super) async fn send_request(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        cap: Option<u32>,
        on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        let trace = Trace::begin(self.stages.as_ref(), self.origin, &self.active_trace);
        let result = self
            .send_traced(trace.as_ref(), turns, tools, tool_choice, cap, on_delta)
            .await;
        if let Err(error) = &result
            && error_status(error).is_none_or(|status| (200..300).contains(&status))
        {
            record_upstream_failure(&self.upstream_failure, None, error.to_string());
        }
        if let Some(trace) = trace {
            match &result {
                Err(e) => trace.failed(&e.to_string()),
                Ok(completion) if completion.operator_abort() => trace.interrupted(),
                Ok(completion) => match completion.upstream_failure() {
                    Some(_) if self.primer_planted(completion) => {
                        trace.ended(completion.finish.as_ref().map(|f| f.raw.as_str()));
                    }
                    Some(why) => trace.failed(why),
                    None => trace.ended(completion.finish.as_ref().map(|f| f.raw.as_str())),
                },
            }
        }
        result
    }

    /// Whether `completion` is a triage primer whose capped output was cut mid-call:
    /// the failure mark is the expected end, not a failed request, because the
    /// provider accounted the input and so wrote the replayed prefix
    /// ([`Self::prime`], docs/transport.md). A primer with no accounted input is
    /// still a failure.
    fn primer_planted(&self, completion: &Completion) -> bool {
        self.origin == RequestOrigin::TriagePrimer
            && !completion.operator_abort()
            && completion.request_usage().usage.input_tokens > 0
    }

    /// [`Self::send`]'s request itself, reporting the stages it passes to `trace`.
    async fn send_traced(
        &self,
        trace: Option<&Trace>,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        cap: Option<u32>,
        on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        // A turn list recorded in the other dialect never becomes a request: the
        // encoder would skip every assistant turn and send a history with the
        // model's replies missing (docs/transport.md). Live sessions are pinned to
        // one provider, so this answers only a recorded session continued or
        // rendered under another.
        crate::project::check_dialect(turns, self.dialect())?;
        let encoded = self.encode(turns);
        // The prompt's variable parts, sized before `max_tokens` is chosen: the
        // reservation is a function of how much of the window the prompt takes
        // (`api/reservation.rs`), estimated from these bytes against the last
        // response's exact count.
        let body_bytes = encoded.body_bytes(&self.system, tools)?;
        let mut max_tokens = cap.unwrap_or_else(|| self.reserve(body_bytes));
        // The operator's interrupt (docs/control-channel.md) is armed before the
        // request is sent, not once its stream opens: `notify_waiters` stores no
        // permit, so a press that lands after the request reaches the upstream but
        // before the read loop registers would otherwise be lost, leaving the read
        // on a stream that may never finish. Armed here, it is ready the first time
        // the read loop polls it.
        let abort_signal = self
            .abort
            .clone()
            .unwrap_or_else(|| Arc::new(tokio::sync::Notify::new()));
        let abort = abort_signal.notified();
        tokio::pin!(abort);
        abort.as_mut().enable();
        let open = Open {
            trace,
            encoded: &encoded,
            turn_count: turns.len(),
            tools,
            tool_choice,
            cap,
        };
        // `Sent` is reported once per request: a reservation retry, at open or
        // in-stream, goes round this loop again.
        let mut sent_reported = false;
        let mut on_delta = on_delta;
        loop {
            let events = self
                .open_stream(&open, max_tokens, &mut sent_reported)
                .await?;
            let completion = self
                .read_stream(trace, events, abort.as_mut(), &mut on_delta)
                .await?;
            if let Some(retry) = self.in_stream_reservation_retry(cap, &completion, max_tokens) {
                max_tokens = retry;
                continue;
            }
            return Ok(self.settle(completion, turns, tools, body_bytes));
        }
    }

    /// Encode the turns once per dialect (docs/transport.md). The encoded form is
    /// what the reservation sizes and what each retry re-sends verbatim.
    fn encode(&self, turns: &[WireTurn]) -> Encoded {
        match self.dialect() {
            WireDialect::Messages => Encoded::Messages(encode_messages(
                turns,
                self.image_placement(),
                self.breakpoint_placement(),
            )),
            WireDialect::Responses => {
                let preamble = match self.endpoint.provider.responses_profile() {
                    ResponsesProfile::Platform => self.system.as_str(),
                    // Rides `instructions` instead (docs/transport.md).
                    ResponsesProfile::Codex => "",
                };
                Encoded::Responses(encode_responses(turns, preamble, self.prompt_cache))
            }
            WireDialect::Completions => {
                Encoded::Completions(encode_completions(turns, &self.system))
            }
            WireDialect::SystemOne => {
                unreachable!("a client is only built on a provider that serves turns")
            }
        }
    }

    /// The Messages envelope for one request (docs/transport.md). The turn and the
    /// keepalive ping both send it, and must send the same bytes but for
    /// `max_tokens` and `stream`: a ping whose prefix differs writes a new cache
    /// entry instead of refreshing the live one.
    pub(super) fn messages_request<'a>(
        &'a self,
        messages: &'a [ApiMessage],
        tools: &'a [Value],
        tool_choice: ToolChoice,
        max_tokens: u32,
        stream: bool,
    ) -> Request<'a> {
        let thinking = Thinking::for_request(&tool_choice, self.thinking, self.thinking_off);
        Request {
            model: &self.model,
            max_tokens,
            system: &self.system,
            tools,
            tool_choice,
            thinking,
            reasoning: self.reasoning_off(thinking),
            output_config: self.effort.clone().map(|effort| OutputConfig { effort }),
            temperature: self.wire_temperature(),
            upstream: self.upstream.as_ref(),
            prompt_cache_key: self.prompt_cache_key(),
            messages,
            cache_diagnostics: self.cache_diagnostics(),
            stream,
        }
    }

    /// One attempt's request body, at `max_tokens`.
    fn request_body(&self, open: &Open<'_>, max_tokens: u32) -> Result<String, Error> {
        Ok(match open.encoded {
            Encoded::Messages(messages) => serde_json::to_string(&self.messages_request(
                messages,
                open.tools,
                open.tool_choice,
                max_tokens,
                true,
            ))?,
            Encoded::Responses(input) => serde_json::to_string(&self.responses_request(
                input.clone(),
                open.tools,
                &open.tool_choice,
                max_tokens,
                open.cap.is_some(),
            ))?,
            Encoded::Completions(messages) => {
                serde_json::to_string(&completions::completions_request(
                    &self.model,
                    messages.clone(),
                    open.tools,
                    open.tool_choice,
                    &completions::RequestParams {
                        max_tokens,
                        temperature: self.wire_temperature(),
                        effort: self.effort.as_deref(),
                        options: &self.endpoint.request,
                    },
                ))?
            }
        })
    }

    /// POST the request and open its event stream. A refused reservation or a
    /// rejected credential is repaired and sent again; any other open failure is the
    /// `Err` this returns.
    async fn open_stream(
        &self,
        open: &Open<'_>,
        mut max_tokens: u32,
        sent_reported: &mut bool,
    ) -> Result<EventStream, Error> {
        // The credential re-read below happens at most once per call: a source
        // that yields a working token yields it the first time, and a second
        // pass would only re-read the same bytes.
        let mut reloaded_auth = false;
        loop {
            let body = self.request_body(open, max_tokens)?;
            log::debug!(
                "POST {}: {} turns, {} request bytes, max_tokens {max_tokens}",
                self.url(),
                open.turn_count,
                body.len()
            );
            // What this attempt authenticates with, kept so a rejection can say
            // which value was rejected rather than re-reading one that may
            // already have been repaired by another session (`AuthValue`).
            // `current` renews a value near its stated expiry before it is
            // sent, so an OAuth grant is refreshed here rather than on a 401.
            let sent_auth = self.endpoint.auth_value.current().await;
            let conn = self.post();
            if let Some(trace) = open.trace
                && !*sent_reported
            {
                *sent_reported = true;
                trace.sent();
            }
            let (status, detail) = match conn.with_body(body).into_sse().await {
                Ok(events) => return Ok(events),
                // The Codex backend streams SSE with no `content-type` at all
                // (docs/transport.md). `into_sse` refuses that like a browser
                // EventSource would, but the failed negotiation still owns the
                // executed conn, so the Responses dialect recovers the stream
                // from it. A *present* wrong content-type, and every other
                // dialect, stay on the failure arm.
                Err(e)
                    if self.dialect() == WireDialect::Responses
                        && matches!(e.kind, SseErrorKind::UnexpectedContentType(None)) =>
                {
                    // `new` fails only on an unexecuted conn (`NoBody`), which a
                    // content-type refusal has already ruled out.
                    match EventStream::new(e.into()) {
                        Ok(events) => return Ok(events),
                        Err(e) => {
                            let (status, detail) = Self::sse_open_failure(e).await;
                            return Err(Self::open_error(status, &detail));
                        }
                    }
                }
                Err(e) => Self::sse_open_failure(e).await,
            };
            if let Some(retry) = self.reservation_retry(open.cap, status, &detail, max_tokens) {
                max_tokens = retry;
                continue;
            }
            // The credential was refused under a running host
            // (docs/transport.md). The value is shared by every session
            // on this connection, so re-reading its source here repairs
            // all of them: the operator runs `codex login` or
            // `efference setup` in another terminal and the next attempt
            // carries the new token. An expired token, a revoked or
            // replaced key, and a rejection naming nothing known all read
            // the source once; a source that still holds the dead value
            // costs that one read.
            if !reloaded_auth
                && self.endpoint.sends_credential()
                && matches!(status, Some(Status::Unauthorized | Status::Forbidden))
                && rejects_credential(&detail)
            {
                reloaded_auth = true;
                match self.endpoint.auth_value.reload(&sent_auth).await {
                    Reloaded::Replaced => {
                        log::warn!(
                            "{} rejected its credential; re-read its source \
                             and retrying",
                            self.endpoint.provider
                        );
                        continue;
                    }
                    // Every other outcome leaves the same value on the
                    // wire, so the retry would fail identically. The
                    // reason joins the error the caller stalls with.
                    outcome => {
                        return Err(Self::open_error(
                            status,
                            &format!("{detail} (credential re-read: {outcome})"),
                        ));
                    }
                }
            }
            // A connection with no `auth` line sends nothing, which is
            // right for a local server and a 401 anywhere else: say so,
            // since the provider's words cannot.
            let detail = if !self.endpoint.sends_credential()
                && matches!(status, Some(Status::Unauthorized | Status::Forbidden))
            {
                format!(
                    "{detail} (this connection sends no credential: its config \
                     block has no `auth` line)"
                )
            } else {
                detail
            };
            return Err(Self::open_error(status, &detail));
        }
    }

    /// The `max_tokens` to retry an open failure at, when the failure is the
    /// reservation's to answer; `None` leaves it terminal. Only a reserved cap is
    /// retried — a fixed `cap` was the caller's choice.
    ///
    /// The window backstop (`api/reservation.rs`): the estimate undershot and the
    /// provider named the overrun, so halve the reservation and go again — once per
    /// halving, down to the floor. The other reservation 400 is the cap itself above
    /// what the model accepts, which is what an *unreported* cap
    /// (`UNKNOWN_OUTPUT_CAP`) risks. Where the provider names its ceiling the retry
    /// goes straight there; where it does not, the same halving applies.
    fn reservation_retry(
        &self,
        cap: Option<u32>,
        status: Option<Status>,
        detail: &str,
        max_tokens: u32,
    ) -> Option<u32> {
        if cap.is_some() || status != Some(Status::BadRequest) {
            return None;
        }
        let overran = is_window_overflow(detail);
        let retry = match (!overran).then(|| output_cap_rejection(detail)).flatten() {
            Some(OutputCapRejection::Max(stated)) => {
                let mut reservation = self.reservation.lock().unwrap();
                reservation.clamp_to(stated).or_else(|| reservation.halve())
            }
            Some(OutputCapRejection::Unstated) => self.reservation.lock().unwrap().halve(),
            None => overran
                .then(|| self.reservation.lock().unwrap().halve())
                .flatten(),
        }?;
        if overran {
            log::warn!(
                "prompt plus max_tokens {max_tokens} overran the window; \
                 retrying with max_tokens {retry}"
            );
        } else {
            log::warn!(
                "{} refused max_tokens {max_tokens}; retrying with max_tokens {retry}",
                self.endpoint.provider
            );
        }
        Some(u32::try_from(retry).unwrap_or(u32::MAX))
    }

    /// The `max_tokens` to resend at when a stream that opened 200 carried the
    /// window-overflow refusal as an `error` event (DeepInfra through OpenRouter);
    /// `None` leaves the completion as it is. The same halving as the open-time
    /// 400 in [`Self::reservation_retry`], once per halving down to the floor, and
    /// the failed attempt's partial is dropped: nothing generated before the
    /// provider refused the request. An operator's interrupt is not a refusal, and
    /// a fixed `cap` was the caller's choice.
    ///
    /// Matched on the failure's text whatever its class: the provider may name the
    /// refusal as a type the classifier treats as transient, and a resend at the
    /// same size would meet the same refusal.
    fn in_stream_reservation_retry(
        &self,
        cap: Option<u32>,
        completion: &Completion,
        max_tokens: u32,
    ) -> Option<u32> {
        if cap.is_some() || completion.operator_abort() {
            return None;
        }
        let why = completion.upstream_failure()?;
        if !is_window_overflow(why) {
            return None;
        }
        let retry = self.reservation.lock().unwrap().halve()?;
        log::warn!(
            "prompt plus max_tokens {max_tokens} overran the window (refused in-stream); \
             retrying with max_tokens {retry}"
        );
        Some(u32::try_from(retry).unwrap_or(u32::MAX))
    }

    /// Read the stream to its end and fold it into a [`Completion`].
    ///
    /// A mid-stream death — a transport error, an in-stream `error` event, an
    /// unparseable event — stops the read but never discards the partial: the
    /// failure is recorded on the assembler and `finish` folds it into the
    /// completion's `upstream_failure`, alongside whatever content accumulated
    /// (docs/transport.md). Only a failure to *open* the stream is an `Err` from
    /// [`Self::send_traced`].
    ///
    /// The duration ceiling (docs/transport.md). A stream that stays open and
    /// never finalizes is bounded by nothing else — the retry policy applies only
    /// while opening a response — so the read loop carries an absolute deadline.
    /// Exceeding it is marked exactly like a mid-stream death: the partial is kept,
    /// `finish` folds the reason into the completion's `upstream_failure`, and the
    /// caller's abort-retry budget decides what happens next.
    ///
    /// The operator's interrupt, armed before the request went out, is the other
    /// way out: it ends the read the same way, and `Aborted` tells the caller not
    /// to retry.
    async fn read_stream(
        &self,
        trace: Option<&Trace>,
        mut events: EventStream,
        mut abort: Pin<&mut Notified<'_>>,
        mut on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        let mut assembler = match self.dialect() {
            WireDialect::Messages => AnyAssembler::Messages(Assembler::new()),
            WireDialect::Responses => AnyAssembler::Responses(ResponsesAssembler::new()),
            WireDialect::Completions => AnyAssembler::Completions(CompletionsAssembler::new()),
            WireDialect::SystemOne => {
                unreachable!("a client is only built on a provider that serves turns")
            }
        };
        // The response the stream opened on, read off the executed conn before the
        // first event. Only the empty-finalized path keeps it (docs/transport.md) —
        // that path's whole record is otherwise one prose string, and the edge's
        // `x-generation-id` is the join key every after-the-fact query needs.
        // The `UpstreamStatusHandler` cannot do this: an empty stream is a 200, so
        // the handler takes its success early-return, and at that point nothing
        // yet knows the stream will arrive empty.
        assembler.observed_response(response_meta(events.conn()));
        let mut operator_abort = false;
        let mut first_byte_reported = false;
        let deadline = tokio::time::Instant::now() + self.stream_ceiling;
        while let Some(event) = tokio::select! {
            next = tokio::time::timeout_at(deadline, events.next()) => match next {
                Ok(event) => event,
                Err(_) => {
                    let why = format!(
                        "upstream SSE stream stayed open past the {}s ceiling without \
                         finalizing ([transport] stream-ceiling)",
                        self.stream_ceiling.as_secs()
                    );
                    log::error!("{why}");
                    assembler.fail(why);
                    None
                }
            },
            _ = abort.as_mut() => {
                let why = "generation interrupted".to_string();
                log::debug!("{why}");
                assembler.fail(why);
                operator_abort = true;
                None
            }
        } {
            let event = match event {
                Ok(event) => event,
                Err(e) => {
                    let why = format!("upstream SSE stream failed mid-stream: {e}");
                    log::error!("{why}");
                    assembler.fail(why);
                    break;
                }
            };
            if let Some(trace) = trace
                && !first_byte_reported
            {
                first_byte_reported = true;
                trace.first_byte();
            }
            match assembler.apply(event.event_type(), event.data()) {
                Ok(Some(streamed)) => on_delta(streamed),
                Ok(None) => {}
                Err(e) => {
                    let why = e.to_string();
                    log::error!("{why}");
                    assembler.fail(why);
                    break;
                }
            }
        }
        // Dropping the stream closes its connection: a hung socket is freed here,
        // not when the provider decides.
        drop(events);
        let mut completion = assembler.finish();
        if operator_abort {
            completion.failure_class = FailureClass::Aborted;
        }
        // The response id a turn leaves on the chain is the one the next turn names as
        // `previous_message_id` (docs/transport.md). Recorded here, at the one site that
        // finishes a stream, so every dialect's turn extends it and no caller has to
        // remember to. Attempts that never finalized carry no id, and the call is a
        // no-op for them.
        self.remember_response_id(self.origin, completion.id.as_deref());
        Ok(completion)
    }

    /// The checks a read completion still owes once its stream is done, and the
    /// reservation's calibration from it.
    fn settle(
        &self,
        mut completion: Completion,
        turns: &[WireTurn],
        tools: &[Value],
        body_bytes: usize,
    ) -> Completion {
        // A server that read far fewer prompt tokens than the request's text could
        // encode to cut the prompt and said nothing (`api/truncation.rs`). Its
        // answer is to a conversation the model did not see, so the turn is
        // record-only, and a retry would be cut the same way.
        if completion.upstream_failure().is_none()
            && let Some(why) = truncation::truncated(
                completion.usage.prompt_tokens(),
                truncation::text_bytes(&self.system, tools, turns),
            )
        {
            log::warn!("{why}");
            completion.fail_terminal(why);
        }
        // Anchor the next estimate to this response's exact prompt count
        // (`api/reservation.rs`). An attempt that died with zero usage
        // counted nothing and teaches nothing, and a truncated one counted the
        // wrong thing.
        let prompt_tokens = completion.usage.prompt_tokens();
        if completion.terminal_failure().is_none() && prompt_tokens > 0 {
            self.reservation
                .lock()
                .unwrap()
                .calibrate(prompt_tokens, body_bytes);
        }
        if !completion.operator_abort()
            && let Some(why) = completion.upstream_failure()
        {
            record_upstream_failure(&self.upstream_failure, None, why);
        }
        completion
    }

    /// The Responses envelope for one turn (docs/transport.md).
    fn responses_request<'a>(
        &'a self,
        input: Vec<responses::wire::InputItem>,
        tools: &[Value],
        tool_choice: &ToolChoice,
        max_output_tokens: u32,
        capped: bool,
    ) -> responses::ResponsesRequest<'a> {
        let reasoning =
            (self.thinking.adaptive && !self.thinking_off).then(|| responses::Reasoning {
                effort: self.effort.clone(),
                summary: "auto",
            });
        match self.endpoint.provider.responses_profile() {
            ResponsesProfile::Platform => responses::ResponsesRequest {
                model: &self.model,
                instructions: None,
                input,
                tools: responses::tool_definitions(tools),
                tool_choice: responses::tool_choice_value(tool_choice),
                parallel_tool_calls: tool_choice.forced().is_none(),
                store: false,
                include: None,
                truncation: Some("disabled"),
                max_output_tokens: Some(max_output_tokens),
                reasoning,
                temperature: self.wire_temperature(),
                prompt_cache_key: self.prompt_cache_key(),
                prompt_cache_options: (self.prompt_cache == PromptCachePolicy::Explicit)
                    .then_some(responses::PromptCacheOptions { mode: "explicit" }),
                stream: true,
            },
            // What Codex CLI sends, and nothing it does not: the backend is
            // undocumented, so an extra field is a guess at its tolerance.
            ResponsesProfile::Codex => responses::ResponsesRequest {
                model: &self.model,
                instructions: Some(&self.system),
                input,
                tools: responses::tool_definitions(tools),
                tool_choice: responses::tool_choice_value(tool_choice),
                parallel_tool_calls: tool_choice.forced().is_none(),
                store: false,
                include: Some(vec!["reasoning.encrypted_content"]),
                truncation: None,
                // Omitted as Codex CLI omits it, except where a caller fixed the cap:
                // the backend enforces one with a minimum of 16 (docs/transport.md).
                max_output_tokens: capped.then_some(max_output_tokens),
                reasoning,
                temperature: None,
                prompt_cache_key: self.prompt_cache_key(),
                prompt_cache_options: None,
                stream: true,
            },
        }
    }

    /// Turn a failed `into_sse` into a status and a legible detail, reading the
    /// response body on a non-2xx. The `SseError` still owns the `Conn` (it `Deref`s
    /// to it and converts back via `Into<Conn>`), so the body is read off it —
    /// Anthropic's error responses carry a JSON `error.message` that names the
    /// actual problem.
    ///
    /// The failure *propagates*: the agent loop treats an open failure as terminal
    /// for the trajectory, recording the message durably as an empty record-only
    /// assistant entry. Mid-stream deaths are the different case — they assemble
    /// as record-only partials and the loop retries them (docs/transport.md).
    async fn sse_open_failure(e: SseError) -> (Option<Status>, String) {
        // `Status` is `Copy`, so binding it doesn't move `e.kind` — `e` stays whole and
        // convertible into its `Conn`.
        if let SseErrorKind::Status(status) = e.kind {
            let mut conn: Conn = e.into();
            let body = read_error_body(&mut conn)
                .await
                .unwrap_or_else(|read_err| format!("(could not read error body: {read_err})"));
            return (Some(status), body);
        }
        // Any other open failure still owns the conn: an unexpected content-type (a
        // JSON error body on a 4xx the retry handler let through, or a 200 that is
        // not a stream) is read the same way a status error is, so the upstream's
        // words reach the record instead of "content-type: None".
        let kind = e.kind.to_string();
        let mut conn: Conn = e.into();
        let status = conn.status();
        // No status means no response arrived — the connect itself failed — so
        // there is no body and no transport to read one from; the client panics on
        // the read rather than erroring.
        let body = if status.is_none() {
            String::new()
        } else {
            read_error_body(&mut conn)
                .await
                .unwrap_or_else(|read_err| format!("(could not read body: {read_err})"))
        };
        let body = body.trim();
        let detail = if body.is_empty() {
            kind
        } else {
            format!("{kind} — {body}")
        };
        (status, detail)
    }

    /// The [`Error`] for an open failure [`Self::sse_open_failure`] took apart:
    /// the status and body on a non-2xx, the transport's own text otherwise. Logged
    /// at `error` so the daemon's log preserves it even though it also propagates.
    fn open_error(status: Option<Status>, detail: &str) -> Error {
        let message = match status {
            Some(status) => format!("upstream API returned {status}: {detail}"),
            None => format!("upstream SSE request failed: {detail}"),
        };
        log::error!("{message}");
        Box::new(OpenError::new(status.map(|s| s as u16), message, detail))
    }
}

/// What every attempt at opening one request's stream sends.
struct Open<'a> {
    trace: Option<&'a Trace>,
    encoded: &'a Encoded,
    turn_count: usize,
    tools: &'a [Value],
    tool_choice: ToolChoice,
    cap: Option<u32>,
}

/// One request's encoded conversation, per dialect.
enum Encoded {
    Messages(Vec<ApiMessage>),
    Responses(Vec<responses::wire::InputItem>),
    Completions(Vec<completions::Message>),
}

impl Encoded {
    /// The bytes the reservation sizes: the system prompt, the encoded
    /// conversation, and the tool definitions.
    fn body_bytes(&self, system: &str, tools: &[Value]) -> Result<usize, Error> {
        let conversation = match self {
            Self::Messages(messages) => system.len() + serde_json::to_vec(messages)?.len(),
            Self::Responses(input) => system.len() + serde_json::to_vec(input)?.len(),
            // The preamble is the first encoded message.
            Self::Completions(messages) => serde_json::to_vec(messages)?.len(),
        };
        Ok(conversation + serde_json::to_vec(tools)?.len())
    }
}

/// The per-dialect stream folder behind one read loop.
enum AnyAssembler {
    Messages(Assembler),
    Responses(ResponsesAssembler),
    Completions(CompletionsAssembler),
}

impl AnyAssembler {
    fn observed_response(&mut self, response: ResponseMeta) {
        match self {
            Self::Messages(a) => a.observed_response(response),
            Self::Responses(a) => a.observed_response(response),
            Self::Completions(a) => a.observed_response(response),
        }
    }

    fn fail(&mut self, why: String) {
        match self {
            Self::Messages(a) => a.fail(why),
            Self::Responses(a) => a.fail(why),
            Self::Completions(a) => a.fail(why),
        }
    }

    fn apply(&mut self, event_type: Option<&str>, data: &str) -> Result<Option<Streamed>, Error> {
        match self {
            Self::Messages(a) => a.apply(event_type, data),
            Self::Responses(a) => a.apply(event_type, data),
            Self::Completions(a) => a.apply(event_type, data),
        }
    }

    fn finish(self) -> Completion {
        match self {
            Self::Messages(a) => a.finish(),
            Self::Responses(a) => a.finish(),
            Self::Completions(a) => a.finish(),
        }
    }
}
