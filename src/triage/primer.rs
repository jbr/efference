//! The triage-lineage primer (docs/transport.md): the own triage's text-view history
//! planted on its client at the batch's first execution start, so the triage's request
//! extends one that ended where its history does.

use super::*;
use crate::api::SentRequest;

/// The tool choice the primer sends (`Triager::prime`), which its keepalive re-sends:
/// forced `retain`, the triage's own head (docs/transport.md).
fn primer_choice() -> ToolChoice {
    ToolChoice::force(RETAIN)
}

/// Everything the triage-lineage primer sends (docs/transport.md): the text view's
/// history through the completion whose calls the batch runs, which the triage's
/// request then extends.
pub struct TriagePrimerInput<'a> {
    /// The triage whose own client carries the triage lineage.
    pub triager: &'a Triager,
    /// The text view's history through the completion whose calls the batch runs
    /// ([`Triager::primer_turns`]): the turns the triage's request opens with.
    pub turns: &'a [WireTurn],
}

/// What the triage-lineage primer did before its triage (docs/transport.md).
pub struct TriagePrimerOutcome {
    /// The primer request's usage, when the backend answered one. `None` when the
    /// lineage needed no planting or the send failed (warned at the send site) —
    /// either way the triage goes out unprimed, the failure mode being fixed.
    pub usage: Option<RequestUsage>,
    /// Milliseconds the primer request spent in flight.
    pub send_ms: u64,
    /// When the primer resolved, for the serial residue the triage path paid.
    pub done_at: std::time::Instant,
    /// Milliseconds of the primer's flight still unfinished when the batch's tools
    /// were all done — the floor the triage path paid. Zero when tool runtime covered
    /// it; the distribution of this number says whether a speculative (pre-execution)
    /// primer is ever worth building.
    pub residue_ms: u64,
    /// The send failure, when the primer recorded no usage because the send failed
    /// (as opposed to a lineage that needed no planting, which resolves to no
    /// outcome at all). Text, so offline callers without a logger still see it.
    pub error: Option<String>,
    /// The primer request as sent, which the triage keepalive re-sends once it planted.
    pub request: SentRequest,
}

impl Triager {
    /// The history half of the text view through the turn that made `calls`, as the
    /// primer plants it (docs/transport.md): the own triage's next request opens with
    /// exactly these turns. `None` where no primer is sent: on the shared triage, which
    /// reads the transcript, and on an own client that plants none.
    pub fn primer_turns(&self, entries: &[LogEntry], calls: &[ToolCall]) -> Option<Vec<WireTurn>> {
        let first = calls.first()?;
        let plants = self.own.as_ref().is_some_and(|o| o.client.plants_primer());
        plants.then(|| {
            let end = text_view::history_end(entries, Some(first));
            vec![text_view::history(entries, end, self.reader()).turn()]
        })
    }

    /// Send the triage-lineage primer over the own client
    /// (docs/transport.md): the text view's history ([`Self::primer_turns`]), capped,
    /// so the triage extends a request that ended where its history does. `None`
    /// on the shared triage — it rides the agent lineage, which the main request
    /// already warmed — or where the own client has no implicit lineage to plant in;
    /// a failed send warns and resolves unprimed rather than failing the batch.
    ///
    /// The primer forces `retain`, matching the triage's head rather than the main
    /// request it replays: the backend keys the resume on `tool_choice`, so an
    /// `auto` primer plants points no forced triage resumes (the title-0 row shows
    /// the head itself is choice-keyed). Forcing costs nothing here — the primer's
    /// output is discarded — and the triage keeps its own forced choice, so the
    /// anti-invented-call guard stays where it belongs.
    ///
    /// It also sends the triage's own `tools[]`, not the agent's: the ladder showed
    /// an identical-input primer planting nothing for the triage while same-envelope
    /// primers chain, so the plant must match the triage's full envelope (tools and
    /// choice) and share only the input prefix. The output is discarded either way.
    pub async fn prime(&self, turns: &[WireTurn]) -> Option<TriagePrimerOutcome> {
        let own = self.own.as_ref()?;
        self.prime_with(turns, &own.tools, primer_choice()).await
    }

    /// `prime` with an explicit choice: the ladder verb's `auto` arm replays v1
    /// as it shipped, against the forced choice live triages send now
    /// (docs/transport.md). Live code always uses `prime`.
    pub async fn prime_with(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        choice: ToolChoice,
    ) -> Option<TriagePrimerOutcome> {
        let own = self.own.as_ref()?;
        let started = std::time::Instant::now();
        let result = own.client.prime(turns, tools, choice).await?;
        let send_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let mut error = None;
        let usage = match result {
            Ok(usage) => Some(usage),
            Err(e) => {
                log::warn!("triage primer failed, the triage goes out unprimed: {e}");
                error = Some(e.to_string());
                None
            }
        };
        Some(TriagePrimerOutcome {
            usage,
            send_ms,
            done_at: std::time::Instant::now(),
            residue_ms: 0,
            error,
            request: SentRequest::new(turns.to_vec(), tools.to_vec(), choice),
        })
    }
}
