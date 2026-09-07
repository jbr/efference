//! Dev-tool inspection and single-shot replay without retries (docs/redaction-fork.md).

use super::*;

/// What one un-retried fork peek came back with — the `peek` dev tool's measurement
/// primitive (docs/redaction-fork.md).
///
/// The gate's own machinery retries, so a session's `Triage.retries` records *that* a
/// first attempt failed but never lets you compare two asks: a change that halves
/// first-attempt non-compliance and one that changes nothing both end in a usable
/// verdict. This runs exactly one round trip and reports the first attempt's verdict,
/// which is the number an instruction change actually moves.
pub struct PeekAttempt {
    /// The tools the peek called, in order — empty for a prose-only answer.
    pub called: Vec<String>,
    /// Each call's arguments, alongside `called`. A tool name says the peek reached for
    /// the wrong thing; the arguments say *what it was reaching for* — whether it was
    /// continuing the task, re-reading what it had just been shown, or something else
    /// entirely. That distinction is the whole diagnosis and the name alone erases it.
    pub inputs: Vec<Value>,
    /// The prose and thinking the peek emitted, joined. A non-compliant attempt that
    /// reasoned about triage first is a different failure from one that never registered
    /// the ask, and only the text separates them.
    pub said: String,
    /// Why the verdict was unusable, in the same phrasing `Triage.retries` records.
    /// `None` means a usable, covering verdict on the first attempt.
    pub why: Option<String>,
    /// The model's exact `keep` string when it did answer, for eyeballing quality
    /// separately from compliance.
    pub raw_keep: Option<String>,
    /// The one-line `note`, which is where an invented claim would show up.
    pub note: Option<String>,
    /// Every result's verdict when the attempt was usable, in the order the model
    /// answered — what a comparison against the recorded verdicts reads, since
    /// `raw_keep`/`note` carry only the first.
    pub verdicts: Vec<ReplayVerdict>,
    pub usage: RequestUsage,
}

/// One result's verdict out of a replayed attempt.
#[derive(Debug, Clone)]
pub struct ReplayVerdict {
    pub call_id: String,
    pub keep: Keep,
    pub note: Option<String>,
}

impl PeekAttempt {
    /// A usable, covering verdict on the first try — the thing being measured.
    pub fn compliant(&self) -> bool {
        self.why.is_none()
    }
}

/// Rebuild the exact request a recorded gate's peek sent — the same `peek_messages` the
/// live gate calls, so a replay can't drift from production (docs/redaction-fork.md).
/// Split out for the `peek` dev tool's dry run, which reviews an ask without spending a
/// request on it.
pub fn peek_request(
    prefix: &[LogEntry],
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
) -> Vec<WireTurn> {
    let roster = surfaceable_roster(prefix);
    peek_messages(prefix, pending, inbox, background, &roster)
}

impl ForkPeek {
    /// One fork-peek round trip against a recorded gate, with **no retries**
    /// (docs/redaction-fork.md). Same request, same client, tools and `tool_choice`
    /// as the live gate under this configuration (shared or own), same validity rule
    /// (`parse_retain` plus [`require_coverage`] and [`require_in_bounds`]) — only the
    /// retry loop is absent, because the first attempt is the measurement.
    pub async fn attempt(
        &self,
        agent: &Client,
        agent_tools: &[Value],
        prefix: &[LogEntry],
        pending: &[Pending<'_>],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> Result<PeekAttempt, String> {
        peek_attempt(
            self.client(agent),
            self.tools(agent_tools),
            self.choice(RETAIN),
            prefix,
            pending,
            inbox,
            background,
        )
        .await
    }
}

async fn peek_attempt(
    client: &Client,
    tools: &[Value],
    choice: ToolChoice,
    prefix: &[LogEntry],
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
) -> Result<PeekAttempt, String> {
    let offered = offered_ids(pending);
    let shown_lines: Vec<(String, usize)> = pending
        .iter()
        .filter(|p| bypassed(p).is_none())
        .map(|p| (p.call.id.0.clone(), shown_line_count(p.raw)))
        .collect();
    let messages = peek_request(prefix, pending, inbox, background);
    let completion = client
        .complete(&messages, tools, choice, |_| {})
        .await
        .map_err(|e| format!("peek request failed: {e}"))?;
    if let Some(why) = completion.upstream_failure() {
        return Err(format!("peek stream aborted upstream: {why}"));
    }

    let (called, inputs, said) = match &completion.assistant {
        Payload::Assistant { output, .. } => {
            let calls = output.tool_calls().unwrap_or_default();
            let said = output
                .reasoning_text()
                .into_iter()
                .chain(output.text())
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            (
                calls.iter().map(|c| c.name.clone()).collect(),
                calls.iter().map(|c| c.input.clone()).collect(),
                said,
            )
        }
        _ => (Vec::new(), Vec::new(), String::new()),
    };
    let verdict = parse_retain(&completion, &offered)
        .ok_or_else(|| unusable(&completion, RETAIN))
        .and_then(|v| require_coverage(v, &offered))
        .and_then(|v| require_in_bounds(v, &shown_lines));
    let (why, raw_keep, note, verdicts) = match verdict {
        Ok(v) => {
            let first = v.results.first();
            (
                None,
                first.map(|r| r.raw_keep.clone()),
                first.and_then(|r| r.note.clone()),
                v.results
                    .iter()
                    .map(|r| ReplayVerdict {
                        call_id: r.call_id.clone(),
                        keep: r.keep.clone(),
                        note: r.note.clone(),
                    })
                    .collect(),
            )
        }
        Err(why) => (Some(why), None, None, Vec::new()),
    };
    Ok(PeekAttempt {
        called,
        inputs,
        said,
        why,
        raw_keep,
        note,
        verdicts,
        usage: completion.request_usage(),
    })
}
