//! Opportunistic consolidation at complete conversational junctures (docs/consolidate.md).
use super::*;
use crate::api::{FinishReason, Streamed};
use crate::daemon::StreamDelta;
use crate::log::Telemetry;

/// The owned request surface a detached consolidation check carries, like the
/// title ask's (docs/session-title.md): a clone of the agent's client that does
/// not answer to the operator's interrupt, and of its tools. Clones share one
/// [`Flight`], which keeps a session to one check in flight.
#[derive(Clone)]
pub(super) struct ConsolidationCheck {
    client: Client,
    tools: Vec<Value>,
    flight: Arc<std::sync::Mutex<Flight>>,
}

/// The single-flight state of a session's consolidation checks (docs/consolidate.md).
#[derive(Default)]
struct Flight {
    /// A check's request is running; a juncture reached meanwhile is not checked.
    running: bool,
    /// The projected context of the last examination, so an unchanged
    /// opportunity is not asked about twice in this process.
    examined: Option<Vec<WireTurn>>,
}

impl ConsolidationCheck {
    pub(super) fn new(client: &Client, tools: &[Value]) -> Self {
        Self {
            client: client.clone().without_interrupt(),
            tools: tools.to_vec(),
            flight: Arc::default(),
        }
    }

    /// Read a candidate off the log at this juncture and, when it qualifies, ask
    /// about it on a detached task (docs/consolidate.md). The loop does not wait:
    /// the trajectory continues past the frozen range, and an adopted account is
    /// appended whenever it lands, which the next main request projects.
    /// Returns the task, which only tests await.
    pub(super) fn check(&self, shared: &Arc<Session>) -> Option<tokio::task::JoinHandle<()>> {
        let entries = shared.snapshot();
        let span = consolidate::candidate(&entries)?;
        let bytes = consolidate::tool_bytes(&entries, span);
        let threshold = shared
            .effective_config()
            .consolidation_bytes
            .unwrap_or(65_536);
        if bytes == 0 || bytes < threshold {
            return None;
        }
        let prefix = crate::project::project_semantic(&entries);
        if examined_durably(&entries, span, &prefix) {
            return None;
        }
        {
            let mut flight = self.flight.lock().unwrap();
            if flight.running || flight.examined.as_ref() == Some(&prefix) {
                return None;
            }
            flight.running = true;
            flight.examined = Some(prefix.clone());
        }
        let context = entries.last().map_or(0, |e| e.seq);
        if let Err(e) = shared.append(
            shared.latest_turn(),
            event(span, context, bytes, "checking", 0),
        ) {
            log::error!("could not record a consolidation check: {e}");
        }
        let shared = shared.clone();
        let this = self.clone();
        Some(tokio::spawn(async move {
            let outcome = this.ask(&shared, &entries, span, prefix).await;
            let size = outcome.as_ref().map_or(0, String::len);
            let label = match outcome {
                Ok(note) => match shared.adopt_consolidation(&entries, span, note) {
                    Ok(true) => {
                        this.flight.lock().unwrap().examined = None;
                        "adopted".to_string()
                    }
                    Ok(false) => "invalidated".to_string(),
                    Err(e) => format!("could not adopt: {e}"),
                },
                Err(reason) => reason,
            };
            let size = if label == "adopted" { size } else { 0 };
            if let Err(e) = shared.append(
                shared.latest_turn(),
                event(span, context, bytes, &label, size),
            ) {
                log::error!("could not record a consolidation outcome: {e}");
            }
            this.flight.lock().unwrap().running = false;
        }))
    }

    async fn ask(
        &self,
        shared: &Session,
        entries: &[crate::log::LogEntry],
        span: consolidate::Span,
        mut prefix: Vec<WireTurn>,
    ) -> Result<String, String> {
        prefix.push(WireTurn::Breakpoint);
        prefix.push(WireTurn::text(instruction(entries, span)));
        let completion = self
            .client
            .with_origin(RequestOrigin::Consolidation)
            .complete(
                &prefix,
                &self.tools,
                ToolChoice::auto(),
                |streamed| match streamed {
                    Streamed::Text { text, .. } => {
                        shared.publish_delta(StreamDelta::ConsolidationText { text });
                    }
                    Streamed::Thinking { text, .. } => {
                        shared.publish_delta(StreamDelta::ConsolidationThinking { text });
                    }
                    Streamed::ToolStart { .. } | Streamed::ToolInput { .. } => {}
                },
            )
            .await
            .map_err(|error| format!("request error: {error}"))?;
        shared.record_request(RequestOrigin::Consolidation, completion.request_usage());
        account(
            completion.assistant_output(),
            completion.finish.as_ref(),
            completion.upstream_failure(),
        )
    }
}

/// Whether a check recorded in the log (before a restart, say) already examined
/// this range against the same projected context; telemetry and title entries
/// since do not constitute new work.
fn examined_durably(
    entries: &[crate::log::LogEntry],
    span: consolidate::Span,
    prefix: &[WireTurn],
) -> bool {
    entries
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            Payload::Telemetry(Telemetry::Consolidation {
                from,
                upto,
                context,
                ..
            }) if *from == span.from && *upto == span.upto => Some(*context),
            _ => None,
        })
        .is_some_and(|context| {
            let seen: Vec<_> = entries
                .iter()
                .filter(|e| e.seq <= context)
                .cloned()
                .collect();
            crate::project::project_semantic(&seen) == prefix
        })
}

fn event(
    span: consolidate::Span,
    context: crate::log::Seq,
    tool_bytes: usize,
    outcome: &str,
    account_bytes: usize,
) -> Payload {
    Payload::Telemetry(Telemetry::Consolidation {
        from: span.from,
        upto: span.upto,
        context,
        tool_bytes,
        outcome: outcome.into(),
        account_bytes,
    })
}

fn account(
    output: &AssistantOutput,
    finish: Option<&crate::api::Finish>,
    failure: Option<&str>,
) -> Result<String, String> {
    if let Some(why) = failure {
        return Err(format!("failed: {why}"));
    }
    if let Some(finish) = finish
        && finish.reason != FinishReason::Finished
    {
        return Err(format!("finish: {:?}", finish.reason));
    }
    if !output
        .tool_calls()
        .map_err(|_| "invalid tool call")?
        .is_empty()
    {
        return Err("tool call".into());
    }
    let text = output.text().join("\n");
    let text = text.trim();
    if text.eq_ignore_ascii_case("skip") {
        return Err("skip".into());
    }
    if text.split_whitespace().count() < 100 {
        return Err("short account".into());
    }
    Ok(text.into())
}

fn endpoint(entries: &[crate::log::LogEntry], seq: crate::log::Seq) -> String {
    let Some(entry) = entries.iter().find(|e| e.seq == seq) else {
        return format!("#{seq}");
    };
    match &entry.payload {
        Payload::ToolResult { .. } => format!("tool result [{seq}]"),
        Payload::User(blocks) => format!(
            "user instruction beginning {:?}",
            blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
                .chars()
                .take(400)
                .collect::<String>()
        ),
        Payload::Assistant { output, .. } => format!(
            "assistant output beginning {:?}",
            output
                .text()
                .join("\n")
                .chars()
                .take(400)
                .collect::<String>()
        ),
        _ => format!("bookkeeping following the working entry before #{seq}"),
    }
}

/// What stays verbatim after the range: a report or question and what followed
/// it, or an operator message that arrived while the work was under way.
fn retained(entries: &[crate::log::LogEntry], span: consolidate::Span) -> &'static str {
    let next = entries.iter().find(|e| {
        e.seq > span.upto && matches!(e.payload, Payload::User(_) | Payload::Assistant { .. })
    });
    match next.map(|e| &e.payload) {
        Some(Payload::User(_)) => {
            "Everything after it, beginning with the operator message that follows it, will remain verbatim. That message arrived while this work was under way, so the work may not be finished; if it is not at a point where its outcomes can be stated, that is a reason to skip."
        }
        _ => {
            "Everything after it, including the report or question that follows it and any reply, will remain verbatim."
        }
    }
}

fn instruction(entries: &[crate::log::LogEntry], span: consolidate::Span) -> String {
    format!(
        "Consider replacing the working span #{from} through #{upto}, inclusive, with an account for continuation. It begins at {begin} and ends at {end}. Everything before that range, including earlier consolidation accounts, will remain verbatim. {after} Do not summarize those retained surroundings.\n\nPrefer completed tool-heavy investigation, reads, edits, execution and verification whose useful outcomes can now be stated directly. Prefer leaving pure planning/design sequences intact. Reply only `skip` if much of this span remains directly useful verbatim for ongoing or next work. Otherwise write the account itself, at least 100 words, as the record of this work going forward. Preserve current results, decisions and their reasons, still-operative scoped operator direction, unresolved threads, and relevant verification. Summarize operator messages by forward utility; quote where wording matters, without turning old permissions or acknowledgments into unscoped standing instructions. Omit superseded diffs and resolved compile/troubleshooting sidetracks unless they establish a continuing constraint or lesson. Exact quotations, snippets, identifiers and line ranges must be directly supported by visible evidence. Historical code may have changed after it was read: qualify or omit uncertain precision rather than reconstruct it. A small exact excerpt may belong in the account; needing to reproduce much of the span is a reason to skip. No tools: do not verify or investigate anything. Reply with the account text or `skip`.",
        from = span.from,
        upto = span.upto,
        begin = endpoint(entries, span.from),
        end = endpoint(entries, span.upto),
        after = retained(entries, span),
    )
}

#[cfg(test)]
mod test;
