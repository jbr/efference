//! The replay sweep (docs/triage.md): every selected triage, `repeat` draws each, and the
//! figures they add up to.

use super::*;
use efference::record::log::LogEntry;

/// What is replayed: the recorded session's entries, the triages chosen from them, and how
/// each is shown and drawn.
pub(super) struct Replay<'a> {
    pub(super) entries: &'a [LogEntry],
    pub(super) selected: &'a [&'a Recorded],
    pub(super) reader: Reader,
    pub(super) view: TriageView,
    pub(super) repeat: usize,
}

/// What a sweep adds up to.
#[derive(Default)]
struct Tally {
    draws: usize,
    compliant: usize,
    cache_read: u64,
    /// The fork tail's size, summed over triages.
    uncached: u64,
    /// Output includes reasoning tokens, which bill as output and vary enormously by model
    /// — the term most likely to make a cost estimate wrong.
    out_tokens: u64,
    prompt_tokens: u64,
    /// Draws that never produced a parseable response at all — counted as failures, and
    /// reported separately because "the model refused the ask" and "the response didn't
    /// decode" are different defects.
    transport_errors: usize,
    seconds_total: f64,
    /// Triages whose recorded first attempt failed, and whose replays now all pass (and the
    /// reverse) — the discordant pairs, which are what a paired comparison actually rests on.
    fixed: usize,
    broke: usize,
}

/// One replay run: what it replays against, and what it has added up so far.
pub(super) struct Sweep<'a> {
    replay: Replay<'a>,
    triage: &'a Triager,
    client: &'a Client,
    tools: &'a [serde_json::Value],
    sink: Option<std::io::BufWriter<std::fs::File>>,
    tally: Tally,
    compared: Vec<Compared>,
}

impl<'a> Sweep<'a> {
    pub(super) fn new(
        replay: Replay<'a>,
        triage: &'a Triager,
        client: &'a Client,
        tools: &'a [serde_json::Value],
        sink: Option<std::io::BufWriter<std::fs::File>>,
    ) -> Self {
        Self {
            replay,
            triage,
            client,
            tools,
            sink,
            tally: Tally::default(),
            compared: Vec::new(),
        }
    }

    /// Replay every selected triage.
    pub(super) async fn run(&mut self) -> CliResult {
        for g in self.replay.selected {
            let pending = g.pending();
            self.note_request(g, &pending);
            if self.replay.view == TriageView::Text {
                self.describe(g).await;
            }
            let mut passes = 0usize;
            for draw in 1..=self.replay.repeat {
                if self.draw(g, &pending, draw).await? {
                    passes += 1;
                }
            }
            match (
                &g.recorded_first_failure,
                passes == self.replay.repeat,
                passes == 0,
            ) {
                (Some(_), true, _) => self.tally.fixed += 1,
                (None, _, true) => self.tally.broke += 1,
                _ => {}
            }
        }
        Ok(())
    }

    /// Count the size of the request's fork tail — result blocks, roster, ask — which is
    /// uncached by construction, so its size is what a triage costs beyond re-reading the
    /// prefix, on every tool result.
    fn note_request(&mut self, g: &Recorded, pending: &[Pending<'_>]) {
        self.tally.uncached += tail_chars(&triage_request(
            &self.replay.entries[..g.prefix_end],
            self.triage.reader(),
            self.replay.view,
            pending,
            &g.inbox,
            &[],
        )) as u64;
    }

    /// The text view's line for a triage, and the primer the live batch sends first, where
    /// the own client plants one (docs/transport.md): without it a Codex triage model never
    /// resumes.
    async fn describe(&self, g: &Recorded) {
        println!(
            "seq {:>4} · {}",
            g.seq,
            layout::describe(self.replay.entries, g, self.replay.reader)
        );
        if let Some(turns) = self
            .triage
            .primer_turns(&self.replay.entries[..g.prefix_end], &g.calls)
            && let Some(primed) = self.triage.prime(&turns).await
        {
            match (&primed.usage, &primed.error) {
                (Some(u), _) => println!(
                    "seq {:>4} · primer · {} prompt · {} cached",
                    g.seq,
                    u.usage.input_tokens
                        + u.usage.cache_read_input_tokens
                        + u.usage.cache_creation_input_tokens,
                    u.usage.cache_read_input_tokens
                ),
                (None, error) => println!(
                    "seq {:>4} · primer failed: {}",
                    g.seq,
                    error.as_deref().unwrap_or("no usage")
                ),
            }
        }
    }

    /// One draw of a triage. Returns whether its first attempt answered.
    async fn draw(
        &mut self,
        g: &Recorded,
        pending: &[Pending<'_>],
        draw: usize,
    ) -> Result<bool, CliError> {
        let repeat = self.replay.repeat;
        // A transport error is this draw's problem, not the sweep's. Propagating it
        // threw away 160-odd completed draws when one response came back with
        // malformed JSON — and a model that sometimes fails to produce a parseable
        // response *is* part of what a cross-model comparison is measuring, so it is
        // recorded as a failed draw rather than hidden by aborting.
        let started = std::time::Instant::now();
        let attempt = match self
            .triage
            .attempt(
                self.client,
                self.tools,
                self.replay.view,
                &self.replay.entries[..g.prefix_end],
                pending,
                &g.inbox,
                &[],
            )
            .await
        {
            Ok(attempt) => attempt,
            Err(e) => {
                self.tally.draws += 1;
                self.tally.transport_errors += 1;
                println!(
                    "seq {:>4} · draw {draw}/{repeat} · {:<24} {e}",
                    g.seq, "(error)"
                );
                return Ok(false);
            }
        };
        self.tally.draws += 1;
        let seconds = started.elapsed().as_secs_f64();
        self.tally.seconds_total += seconds;
        self.tally.cache_read += attempt.usage.usage.cache_read_input_tokens;
        self.tally.out_tokens += attempt.usage.usage.output_tokens;
        self.tally.prompt_tokens += attempt.usage.usage.input_tokens
            + attempt.usage.usage.cache_read_input_tokens
            + attempt.usage.usage.cache_creation_input_tokens;
        // NB: not `usage.input_tokens`. That counts everything neither read from nor
        // written to cache, so a prefix segment that missed cache lands in it too —
        // it moved the wrong way between two runs whose tails differed by 1 KB. Tail
        // size is measured off the request we build instead: deterministic, free, and
        // exactly the thing a cheaper roster is supposed to move.
        if attempt.compliant() {
            self.tally.compliant += 1;
        }
        let called = match attempt.called.join(", ").as_str() {
            "" => "(no tool)".to_string(),
            s => s.to_string(),
        };
        let rows = compare(g, draw, &attempt.verdicts, seconds);
        let against = rows
            .iter()
            .map(|c| {
                format!(
                    " · vs {} (over {}, under {}{})",
                    c.reference_keep,
                    pct(c.over, c.shown),
                    pct(c.under, c.shown),
                    if c.phantom_none { ", phantom none" } else { "" }
                )
            })
            .collect::<String>();
        let usage = &attempt.usage.usage;
        println!(
            "seq {:>4} · draw {draw}/{repeat} · {:<24} {}{against} · {} prompt · {} cached · {seconds:.1}s",
            g.seq,
            called,
            outcome(&attempt),
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
        );
        if let Some(sink) = &mut self.sink {
            for c in &rows {
                writeln!(sink, "{}", serde_json::to_string(c)?)?;
            }
        }
        self.compared.extend(rows);
        // A failing draw prints what it reached for and what it said. The tool name
        // alone says the triage answered with the wrong thing; the arguments and the
        // prose say whether it was continuing the task, re-reading what it had just
        // been shown, or never registered the ask at all — which is the diagnosis.
        if !attempt.compliant() {
            for (name, input) in attempt.called.iter().zip(&attempt.inputs) {
                println!("           ↳ {name}({})", one_line(&input.to_string(), 160));
            }
            if !attempt.said.is_empty() {
                println!("           ↳ said: {}", one_line(&attempt.said, 300));
            }
        }
        Ok(attempt.compliant())
    }

    /// The sweep's totals, and — when there are draws to compare — how they sit against the
    /// recorded verdicts. `out` names the file the compared rows were written to.
    pub(super) fn print_summary(&self, model: &str, table: usize, out: Option<&Path>) {
        self.print_totals(model);
        if !self.compared.is_empty() {
            self.print_against_recorded();
            print_table(table, &self.compared);
            if let Some(path) = out {
                println!(
                    "\n{} rows written to {}",
                    self.compared.len(),
                    path.display()
                );
            }
        }
    }

    fn print_totals(&self, model: &str) {
        let Tally {
            draws,
            compliant,
            cache_read,
            uncached,
            out_tokens,
            prompt_tokens,
            transport_errors,
            seconds_total,
            fixed,
            broke,
        } = &self.tally;
        let selected = self.replay.selected;
        let recorded_failures = selected
            .iter()
            .filter(|g| g.recorded_first_failure.is_some())
            .count();
        println!("\nTRIAGE REPLAY · {model}");
        // The fork tail is uncached by construction, so this is what a triage costs beyond
        // re-reading the prefix — paid on every tool result, for the whole session. It is the
        // number a cheaper roster has to move, and it belongs next to the failure rate because
        // the two are the trade.
        println!(
            "  tail      {} chars per triage, uncached (result + roster + ask)",
            uncached / selected.len().max(1) as u64
        );
        println!(
            "  recorded  {recorded_failures}/{} triages failed their first attempt  ({:.0}%)",
            selected.len(),
            100.0 * recorded_failures as f64 / selected.len() as f64
        );
        println!(
            "  replayed  {}/{draws} draws failed  ({:.0}%)",
            draws - compliant,
            100.0 * (draws - compliant) as f64 / *draws as f64
        );
        // The paired view: an aggregate rate can move without any gate changing behaviour,
        // so the triages that flipped are reported separately from the totals.
        println!(
            "  flipped   {fixed} recorded-failure triages now pass every draw · {broke} recorded-pass triages now fail every draw"
        );
        // Output carries reasoning tokens, which differ by an order of magnitude between
        // models and are the term that makes a borrowed cost estimate wrong. A short
        // `--limit` probe reports this on the model itself.
        if *transport_errors > 0 {
            println!(
                "  errors    {transport_errors} draw(s) returned nothing parseable (counted as failures)"
            );
        }
        println!(
            "  tokens    {}k prompt · {} output per draw (output includes reasoning)",
            prompt_tokens / (*draws).max(1) as u64 / 1000,
            out_tokens / (*draws).max(1) as u64
        );
        println!(
            "  cache     {cache_read} tokens read (a cold total means the replay isn't reading the session's prefix)"
        );
        // On the critical path of every tool call: a verdict's latency is paid by the
        // agent's step, so it is reported beside tokens rather than left to the clock.
        println!(
            "  latency   {:.1}s per draw, request to verdict",
            seconds_total / (*draws).max(1) as f64
        );
    }

    fn print_against_recorded(&self) {
        let compared = &self.compared;
        let n = compared.len();
        let shown: usize = compared.iter().map(|c| c.shown).sum();
        let r: usize = compared.iter().map(|c| c.reference_kept).sum();
        let c: usize = compared.iter().map(|c| c.candidate_kept).sum();
        let over: usize = compared.iter().map(|c| c.over).sum();
        let under: usize = compared.iter().map(|c| c.under).sum();
        let exact = compared
            .iter()
            .filter(|c| c.over == 0 && c.under == 0)
            .count();
        let phantom = compared.iter().filter(|c| c.phantom_none).count();
        println!("\nAGAINST THE RECORDED VERDICTS · {n} result-draws");
        println!(
            "  kept      reference {} of shown lines · candidate {}",
            pct(r, shown),
            pct(c, shown)
        );
        println!(
            "  over      {} of shown lines kept that the reference dropped",
            pct(over, shown)
        );
        println!(
            "  under     {} of shown lines dropped that the reference kept",
            pct(under, shown)
        );
        println!("  exact     {exact}/{n} result-draws kept the same lines");
        if phantom > 0 {
            println!(
                "  phantom   {phantom} `none` verdict(s) against a reference that kept something"
            );
        }
    }
}
