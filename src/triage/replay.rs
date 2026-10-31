//! Dev-tool inspection and single-shot replay without retries (docs/triage.md).

use super::*;

/// What one un-retried triage came back with — the `triage` dev tool's measurement
/// primitive (docs/triage.md).
///
/// The triage's own machinery retries, so a session's `Triage.retries` records *that* a
/// first attempt failed but never lets you compare two asks: a change that halves
/// first-attempt non-compliance and one that changes nothing both end in a usable
/// verdict. This runs exactly one round trip and reports the first attempt's verdict,
/// which is the number an instruction change actually moves.
pub struct TriageAttempt {
    /// The tools the triage called, in order — empty for a prose-only answer.
    pub called: Vec<String>,
    /// Each call's arguments, alongside `called`. A tool name says the triage reached for
    /// the wrong thing; the arguments say *what it was reaching for* — whether it was
    /// continuing the task, re-reading what it had just been shown, or something else
    /// entirely. That distinction is the whole diagnosis and the name alone erases it.
    pub inputs: Vec<Value>,
    /// The prose and thinking the triage emitted, joined. A non-compliant attempt that
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

impl TriageAttempt {
    /// A usable, covering verdict on the first try — the thing being measured.
    pub fn compliant(&self) -> bool {
        self.why.is_none()
    }
}

/// Rebuild the exact request a recorded triage sent — the same `triage_messages` the
/// live triage calls, so a replay can't drift from production (docs/triage.md).
/// Split out for the `triage` dev tool's dry run, which reviews an ask without spending a
/// request on it.
///
/// `view` picks the rendering of the history: the transcript the shared triage reads, the
/// text view a separate triage reads, or an arm under evaluation (`digest_view.rs`).
pub fn triage_request(
    prefix: &[LogEntry],
    reader: Reader,
    view: TriageView,
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
) -> Vec<WireTurn> {
    let roster = surfaceable_roster(prefix);
    match view {
        TriageView::Transcript => {
            triage_messages(prefix, reader, pending, inbox, background, &roster)
        }
        view => digest_view::text_only_messages(
            prefix, reader, view, pending, inbox, background, &roster,
        ),
    }
}

impl Triager {
    /// One triage round trip against a recorded triage, with **no retries**
    /// (docs/triage.md). Same request, same client, tools and `tool_choice`
    /// as the live triage under this configuration (shared or own), same validity rule
    /// (`parse_retain` plus [`require_coverage`] and [`require_in_bounds`]) — only the
    /// retry loop is absent, because the first attempt is the measurement.
    // The replay builds its request through `triage_request`, from the arguments the live
    // triage passes it, so its inputs are `triage_results`' less the sink plus the view under
    // test; a bundle of them would belong to `triage_request` and its callees as well.
    #[allow(clippy::too_many_arguments)]
    pub async fn attempt(
        &self,
        agent: &Client,
        agent_tools: &[Value],
        view: TriageView,
        prefix: &[LogEntry],
        pending: &[Pending<'_>],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> Result<TriageAttempt, String> {
        let messages = triage_request(prefix, self.reader(), view, pending, inbox, background);
        triage_attempt(
            &self.client(agent, RequestOrigin::Triage),
            self.tools(agent_tools),
            self.choice(RETAIN),
            &messages,
            pending,
        )
        .await
    }
}

async fn triage_attempt(
    client: &Client,
    tools: &[Value],
    choice: ToolChoice,
    messages: &[WireTurn],
    pending: &[Pending<'_>],
) -> Result<TriageAttempt, String> {
    let labels = labels(pending);
    let offered = offered_labels(pending);
    let shown_lines = shown_lines(pending, &labels);
    let completion = client
        .complete(messages, tools, choice, |_| {})
        .await
        .map_err(|e| format!("triage request failed: {e}"))?;
    if let Some(why) = completion.upstream_failure() {
        return Err(format!("triage stream aborted upstream: {why}"));
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
                    .filter_map(|r| {
                        // Back from the ask's label to the call it stands for; a label
                        // never offered has no call and no verdict to compare.
                        let (p, _) = pending
                            .iter()
                            .zip(&labels)
                            .find(|(_, l)| l.as_ref() == Some(&r.label))?;
                        Some(ReplayVerdict {
                            call_id: p.call.id.0.clone(),
                            keep: r.keep.clone(),
                            note: r.note.clone(),
                        })
                    })
                    .collect(),
            )
        }
        Err(why) => (Some(why), None, None, Vec::new()),
    };
    Ok(TriageAttempt {
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
