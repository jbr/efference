//! `efference dev peek` — replay a recorded session's fork gates (docs/redaction-fork.md).
//!
//! A dev tool, like `wire`/`gallery`/`spotter`. The live gate retries, so a session's
//! `Triage.retries` records *that* a first attempt failed but can't compare two asks —
//! a change that halves first-attempt non-compliance and one that changes nothing both
//! end in a usable verdict. This re-sends a recorded gate's exact request, once, and
//! reports whether the first attempt answered.
//!
//! The eval set is the session itself: every gate carries its own prefix, at its own
//! session depth, and replaying one is a single warm prefix read.

use super::*;
use efference::distill::{ForkPeek, Pending, ReplayVerdict, TriageModel, TriageView, peek_request};
use efference::log::{Keep, Payload, RawResult, ToolCall, WireState};
use efference::project::{Reader, distillate};
use std::collections::BTreeSet;
use std::io::Write;

/// One recorded gate, rebuilt from the log: the prefix the peek saw, and the results it
/// was asked to triage.
///
/// The prefix ends **before** the gate's tool results, because that is where the live
/// loop snapshots it — `peek_messages` appends the results itself as fresh
/// `tool_result` blocks, so a prefix that already contained them would double them.
pub(crate) struct Gate {
    /// Index into the log of the first result this gate carries — the prefix boundary.
    pub(crate) prefix_end: usize,
    /// Index into the log of the assistant turn whose calls this gate triages. A
    /// primer replays the main request's input, which ends *before* those calls —
    /// including them without their results is an unanswered `tool_use` the API
    /// rejects — so primer turns project `entries[..calls_at]`, not the peek prefix.
    pub(crate) calls_at: usize,
    /// The `seq` of the first result, which is how `--seq` addresses a gate and how the
    /// recorded `Triage.for_result` names it.
    pub(crate) seq: u64,
    pub(crate) calls: Vec<ToolCall>,
    pub(crate) raws: Vec<RawResult>,
    /// The recorded verdict per result, alongside `raws` — what the session's own
    /// peek chose to see. The reference a candidate model is compared against
    /// (backlog/inbox/distiller-experiment.md): not an operator's idea of the right keep,
    /// but what the agent that was doing the work kept for itself.
    pub(crate) recorded: Vec<WireState>,
    /// What the recorded run's first attempt did, from `Triage.retries` — the baseline
    /// each replay is compared against. `None` means it complied first try.
    pub(crate) recorded_first_failure: Option<String>,
}

/// Rebuild every gate in a log, in order.
///
/// A gate is a completion's tool calls plus their results, which is exactly the grouping
/// the live loop opens a gate over: the peek's prefix ends with an assistant message
/// carrying one `tool_use` per call, and the API rejects a request that leaves any of
/// them unanswered (docs/redaction-fork.md).
pub(crate) fn gates(entries: &[efference::log::LogEntry]) -> Vec<Gate> {
    // Every result, by call id, and every gate's recorded first-attempt breadcrumb. A
    // result logged `Pending` takes the verdict a later `ResultVerdict` gave it, as the
    // projection resolves it (docs/log-and-projection.md).
    let verdicts = efference::log::result_verdicts(entries);
    let mut gates = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let Payload::Assistant { output, .. } = &entry.payload else {
            continue;
        };
        let Ok(calls) = output.tool_calls() else {
            continue;
        };
        if calls.is_empty() {
            continue;
        }
        let ids: Vec<&str> = calls.iter().map(|c| c.id.0.as_str()).collect();
        let mut raws = Vec::new();
        let mut recorded = Vec::new();
        let mut kept_calls = Vec::new();
        let mut prefix_end = None;
        let mut seq = 0;
        for (j, later) in entries.iter().enumerate().skip(i + 1) {
            match &later.payload {
                Payload::ToolResult { call_id, raw, wire } if ids.contains(&call_id.0.as_str()) => {
                    if prefix_end.is_none() {
                        prefix_end = Some(j);
                        seq = later.seq;
                    }
                    if let Some(call) = calls.iter().find(|c| c.id == *call_id) {
                        kept_calls.push(call.clone());
                        raws.push(raw.clone());
                        recorded.push(
                            verdicts
                                .get(&later.seq)
                                .map_or_else(|| wire.clone(), |&verdict| verdict.clone()),
                        );
                    }
                }
                // The next completion starts a new gate.
                Payload::Assistant { .. } => break,
                _ => {}
            }
        }
        let Some(prefix_end) = prefix_end else {
            continue; // a completion whose calls never ran (aborted, withheld)
        };
        // The recorded first attempt, off this gate's `Triage` breadcrumbs. A batch
        // writes one `Triage` per result but its `retries` ride the **last** of them
        // (docs/redaction-fork.md), so every `Triage` between here and the next
        // completion is searched — reading only the first undercounts multi-result gates.
        let recorded_first_failure = entries[prefix_end..]
            .iter()
            .take_while(|e| !matches!(&e.payload, Payload::Assistant { .. }))
            .find_map(|e| match &e.payload {
                Payload::Triage(t) => t.retries.first().cloned(),
                _ => None,
            });
        gates.push(Gate {
            prefix_end,
            calls_at: i,
            seq,
            calls: kept_calls,
            raws,
            recorded,
            recorded_first_failure,
        });
    }
    gates
}

/// Collapse `s` onto one line and cap it at `max` characters — a failing draw's arguments
/// and prose are for scanning a sweep, not for reading in full.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

/// The size of the peek's trailing user message — the part of the request that is fresh
/// on every gate, as against the prefix it re-reads warm.
fn tail_chars(turns: &[efference::project::WireTurn]) -> usize {
    use efference::project::{UserPart, WireTurn};
    let Some(WireTurn::User(parts)) = turns.last() else {
        return 0;
    };
    parts
        .iter()
        .map(|p| match p {
            UserPart::Text(text) => text.len(),
            UserPart::ToolResult { content, .. } => content.len(),
            UserPart::Breakpoint => 0,
        })
        .sum()
}

/// The one-line description of what a replayed attempt did.
fn outcome(attempt: &efference::distill::PeekAttempt) -> String {
    match &attempt.why {
        None => format!(
            "ok · keep {}",
            attempt.raw_keep.as_deref().unwrap_or("(none)")
        ),
        Some(why) => why.clone(),
    }
}

/// The 1-based line numbers a verdict keeps over `content`, after the cap.
fn kept_set(content: &str, keep: &Keep) -> BTreeSet<usize> {
    distillate(content, keep)
        .ranges
        .iter()
        .flat_map(|&(a, b)| a..=b)
        .collect()
}

/// One result's comparison: a candidate verdict against the recorded one.
#[derive(serde::Serialize)]
struct Compared {
    seq: u64,
    draw: usize,
    call_id: String,
    tool: String,
    intent: Option<String>,
    shown: usize,
    reference_keep: String,
    candidate_keep: String,
    reference_kept: usize,
    candidate_kept: usize,
    /// Lines the candidate kept that the reference dropped.
    over: usize,
    /// Lines the reference kept that the candidate dropped.
    under: usize,
    reference_note: Option<String>,
    candidate_note: Option<String>,
    /// Wall-clock seconds the draw took, request to verdict. The peek sits on the
    /// critical path of every tool call, so this is one of the three axes beside
    /// judgment and price.
    seconds: f64,
    /// `retain(none)` against a reference that kept something — the shape of the
    /// "no content was shown" verdicts seen on a real read.
    phantom_none: bool,
}

/// The `Keep` a recorded wire state carries, if the reference is a model verdict.
/// A bypassed (`Raw`) or withheld result has no reference to compare against.
fn recorded_keep(wire: &WireState) -> Option<(&Keep, Option<&str>)> {
    match wire {
        WireState::Distilled(v) => Some((&v.keep, v.note.as_deref())),
        _ => None,
    }
}

fn keep_label(keep: &Keep) -> String {
    match keep {
        Keep::All => "all".into(),
        Keep::None => "none".into(),
        Keep::Ranges(rs) => rs
            .iter()
            .map(|(a, b)| {
                if a == b {
                    a.to_string()
                } else {
                    format!("{a}-{b}")
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}

/// Compare one draw's verdicts against the gate's recorded ones, result by result.
fn compare(gate: &Gate, draw: usize, verdicts: &[ReplayVerdict], seconds: f64) -> Vec<Compared> {
    let mut out = Vec::new();
    for ((call, raw), wire) in gate.calls.iter().zip(&gate.raws).zip(&gate.recorded) {
        let Some((reference, reference_note)) = recorded_keep(wire) else {
            continue;
        };
        let Some(candidate) = verdicts.iter().find(|v| v.call_id == call.id.0) else {
            continue;
        };
        let shown = distillate(&raw.content, &Keep::All).shown.len();
        let r = kept_set(&raw.content, reference);
        let c = kept_set(&raw.content, &candidate.keep);
        out.push(Compared {
            seq: gate.seq,
            draw,
            call_id: call.id.0.clone(),
            tool: call.name.clone(),
            intent: call.intent().map(str::to_string),
            shown,
            reference_keep: keep_label(reference),
            candidate_keep: keep_label(&candidate.keep),
            reference_kept: r.len(),
            candidate_kept: c.len(),
            over: c.difference(&r).count(),
            under: r.difference(&c).count(),
            reference_note: reference_note.map(str::to_string),
            candidate_note: candidate.note.clone(),
            seconds,
            phantom_none: matches!(candidate.keep, Keep::None) && !r.is_empty(),
        });
    }
    out
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "–".into()
    } else {
        format!("{:.0}%", 100.0 * n as f64 / d as f64)
    }
}

/// The session's own model, off the recorded preamble's identity line. The line
/// carries the session's id in a second sentence (docs/context.md), so the model
/// is the first sentence's body, not the whole line.
pub(crate) fn session_model(system: &str) -> Option<String> {
    let line = system.lines().next()?.strip_prefix("You are ")?;
    let model = line.split_once(". ").map_or(line, |(first, _)| first);
    model.strip_suffix('.').unwrap_or(model).to_string().into()
}

/// Print the comparison table: every compared result sorted by disagreement, the
/// worst first, capped at `rows`. Where the reading happens.
fn print_table(rows_cap: usize, compared: &[Compared]) {
    let mut rows: Vec<&Compared> = compared.iter().collect();
    rows.sort_by_key(|c| std::cmp::Reverse(c.over + c.under));
    println!(
        "\n| seq | draw | tool | shown | reference | candidate | over | under | reference note | candidate note |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for c in rows.iter().take(rows_cap) {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.seq,
            c.draw,
            c.tool,
            c.shown,
            one_line(&c.reference_keep, 24),
            one_line(&c.candidate_keep, 24),
            pct(c.over, c.shown),
            pct(c.under, c.shown),
            one_line(c.reference_note.as_deref().unwrap_or(""), 120),
            one_line(c.candidate_note.as_deref().unwrap_or(""), 120),
        );
    }
    if compared.len() > rows_cap {
        println!(
            "\n({} more rows; `--table` raises the cap, `--out` writes every row)",
            compared.len() - rows_cap
        );
    }
}

/// Replay one gate or every gate in a recorded session, `repeat` draws each.
#[allow(clippy::too_many_arguments)] // one flag, one parameter; the CLI is the arity
pub(crate) async fn peek(
    path: PathBuf,
    seq: Option<u64>,
    repeat: usize,
    limit: Option<usize>,
    dry_run: bool,
    model: &str,
    triage: Option<TriageModel>,
    view: TriageView,
    out: Option<PathBuf>,
    table: usize,
) -> CliResult {
    // A `log.jsonl`, or a session dir / name that resolves to one — the same target
    // grammar the other reader verbs take.
    let (path, label) = resolve_session_log(&path.to_string_lossy())?;
    let entries = efference::persist::load(&path)?;
    // Replays use the selected connection's projection-shaping choices — image
    // placement and cache-breakpoint placement — matching what a live session on
    // that provider projects. If the current config cannot resolve the selection,
    // the boot identity supplies the historical shapes; a legacy log falls back to
    // the defaults (docs/log-and-projection.md).
    let inference = crate::inference::resolve(
        Some(model),
        None,
        triage.clone().map(|triage| {
            efference::inference::TriageChoice::separate(
                triage.model,
                triage.thinking,
                triage.auto_choice,
                triage.effort,
            )
        }),
    );
    let recorded_identity = efference::log::recorded_inference(&entries);
    let (provider, placement_model) = match inference.as_ref() {
        Ok(inference) => (Some(inference.main().1.provider), model),
        Err(_) => (
            recorded_identity.map(|identity| identity.provider),
            recorded_identity
                .map(|identity| identity.model.as_str())
                .unwrap_or(model),
        ),
    };
    let placement = provider
        .map(|p| p.image_placement(placement_model))
        .unwrap_or_default();
    let breakpoint = provider
        .map(|p| p.breakpoint_placement(placement_model))
        .unwrap_or_default();
    let all = gates(&entries);
    let mut selected: Vec<&Gate> = match seq {
        Some(s) => all.iter().filter(|g| g.seq == s).collect(),
        None => all.iter().collect(),
    };
    // Evenly spaced, not the first N. Failure rate climbs with session depth, so a prefix
    // of the gates samples the easy end and flatters the model.
    if let Some(limit) = limit.filter(|n| *n < selected.len() && *n > 0) {
        let step = selected.len() as f64 / limit as f64;
        selected = (0..limit)
            .map(|i| selected[(i as f64 * step) as usize])
            .collect();
    }
    if selected.is_empty() {
        return Err(match seq {
            Some(s) => format!("no gate triaging result seq {s} in {}", path.display()).into(),
            None => format!("no gates in {}", path.display()).into(),
        });
    }

    // The envelope a real turn sends. `system` is the recorded boot preamble, so the
    // replay reads the same cached prefix the session did; `tools[]` is rebuilt from the
    // current code and cwd, exactly as `wire` does and with the same caveat.
    let (system, context_block) = entries
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            Payload::Preamble {
                system, context, ..
            } => Some((system.clone(), context.render())),
            _ => None,
        })
        .ok_or("log predates the boot-preamble snapshot; no recorded system prompt to replay")?;
    let toolbox = Toolbox::bash_and_mcps(std::env::current_dir()?, None)?;
    let tools = efference::agent::tool_definitions(&toolbox);

    // A dry run over the whole session costs nothing and answers the other half of the
    // question a variant raises: what it costs. The tail is the fresh part of every gate's
    // request, so its mean size is what a cheaper roster is trying to move — and reading
    // it here rather than off `usage` keeps it free of cache jitter.
    if dry_run && seq.is_none() {
        let total: usize = selected
            .iter()
            .map(|g| {
                let pending: Vec<Pending<'_>> = g
                    .calls
                    .iter()
                    .zip(&g.raws)
                    .map(|(call, raw)| Pending { call, raw })
                    .collect();
                tail_chars(&peek_request(
                    &entries[..g.prefix_end],
                    Reader::Main,
                    view,
                    &pending,
                    &[],
                    &[],
                ))
            })
            .sum();
        println!(
            "{} gates · mean tail {} chars · {} chars per session",
            selected.len(),
            total / selected.len(),
            total
        );
        return Ok(());
    }
    if dry_run {
        let g = selected[0];
        let pending: Vec<Pending<'_>> = g
            .calls
            .iter()
            .zip(&g.raws)
            .map(|(call, raw)| Pending { call, raw })
            .collect();
        let messages = peek_request(
            &entries[..g.prefix_end],
            Reader::Main,
            view,
            &pending,
            &[],
            &[],
        );
        eprintln!("note: gate at seq {} · {} result(s)", g.seq, pending.len());
        let messages = efference::project::encode_messages(
            &messages[messages.len() - 1..],
            placement,
            breakpoint,
        );
        println!("{}", efference::api::render_messages_pretty(&messages)?);
        return Ok(());
    }

    let inference = inference?;
    let (main_selection, mut endpoint) = inference.main();
    let model = main_selection.model.as_str();
    // The session's sticky-routing key (docs/transport.md), derived from its dir
    // exactly as boot derives it, so the replay reads the session's own prefix on
    // the upstream that holds it — and the own peek takes its `-triage` suffix, its
    // own lineage's key. Without it a provider with sticky routing re-derives a key
    // per request and the replay wanders between upstreams, cold every time.
    if let Some(dir) = path.parent() {
        endpoint.session_id = Some(efference::api::sticky_session_id(dir));
    }
    // The own peek replays the exact live shape (docs/redaction-fork.md): the same
    // constructor boot uses, the same system prompt over the recorded context
    // block, the session's own model named as the agent. The shared peek replays on
    // the agent client, whose model is `--model`.
    let agent_model = recorded_identity
        .map(|identity| identity.model.clone())
        .or_else(|| session_model(&system))
        .unwrap_or_else(|| model.to_string());
    let (peek, model) = match inference.triage() {
        Some((_, mut triage_endpoint, t)) => {
            triage_endpoint.session_id = endpoint.session_id.clone();
            let triage_system = efference::env::triage_system_prompt(
                &t.model,
                &agent_model,
                context_block.as_deref(),
            );
            let (peek, caps) = ForkPeek::for_triage_model(
                &triage_endpoint,
                t,
                triage_system,
                std::time::Duration::from_secs(600),
            )
            .await?;
            eprintln!(
                "triage: {} · context {} · thinking {} · tool choice {} · reference: {agent_model}'s own verdicts",
                t.model,
                efference::boot::describe_window(caps.context_length),
                match (&t.effort, t.thinking) {
                    (Some(effort), _) => format!("on, effort {effort}"),
                    (None, true) => "on".into(),
                    (None, false) => "off".into(),
                },
                if t.auto_choice { "auto" } else { "forced" }
            );
            (peek, t.model.as_str())
        }
        None => (ForkPeek::shared(), model),
    };
    let client = Client::new(endpoint, model, system, None);
    let mut sink = match &out {
        Some(path) => Some(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => None,
    };
    let mut compared: Vec<Compared> = Vec::new();

    eprintln!(
        "replaying {} gate(s) from {label} × {repeat} on {model} — first attempt only, no retries",
        selected.len()
    );
    let (mut draws, mut compliant, mut cache_read, mut uncached) = (0usize, 0usize, 0u64, 0u64);
    // Output includes reasoning tokens, which bill as output and vary enormously by model
    // — the term most likely to make a cost estimate wrong.
    let (mut out_tokens, mut prompt_tokens) = (0u64, 0u64);
    // Draws that never produced a parseable response at all — counted as failures, and
    // reported separately because "the model refused the ask" and "the response didn't
    // decode" are different defects.
    let mut transport_errors = 0usize;
    let mut seconds_total = 0f64;
    // Gates whose recorded first attempt failed, and whose replays now all pass (and the
    // reverse) — the discordant pairs, which are what a paired comparison actually rests on.
    let (mut fixed, mut broke) = (0usize, 0usize);
    for g in &selected {
        let pending: Vec<Pending<'_>> = g
            .calls
            .iter()
            .zip(&g.raws)
            .map(|(call, raw)| Pending { call, raw })
            .collect();
        // The fork tail — result blocks, roster, ask — is uncached by construction, so
        // its size is what a gate costs beyond re-reading the prefix, on every tool result.
        uncached += tail_chars(&peek_request(
            &entries[..g.prefix_end],
            peek.reader(),
            view,
            &pending,
            &[],
            &[],
        )) as u64;
        let mut passes = 0usize;
        for draw in 1..=repeat {
            // A transport error is this draw's problem, not the sweep's. Propagating it
            // threw away 160-odd completed draws when one response came back with
            // malformed JSON — and a model that sometimes fails to produce a parseable
            // response *is* part of what a cross-model comparison is measuring, so it is
            // recorded as a failed draw rather than hidden by aborting.
            let started = std::time::Instant::now();
            let attempt = match peek
                .attempt(
                    &client,
                    &tools,
                    view,
                    &entries[..g.prefix_end],
                    &pending,
                    &[],
                    &[],
                )
                .await
            {
                Ok(attempt) => attempt,
                Err(e) => {
                    draws += 1;
                    transport_errors += 1;
                    println!(
                        "seq {:>4} · draw {draw}/{repeat} · {:<24} {e}",
                        g.seq, "(error)"
                    );
                    continue;
                }
            };
            draws += 1;
            let seconds = started.elapsed().as_secs_f64();
            seconds_total += seconds;
            cache_read += attempt.usage.usage.cache_read_input_tokens;
            out_tokens += attempt.usage.usage.output_tokens;
            prompt_tokens += attempt.usage.usage.input_tokens
                + attempt.usage.usage.cache_read_input_tokens
                + attempt.usage.usage.cache_creation_input_tokens;
            // NB: not `usage.input_tokens`. That counts everything neither read from nor
            // written to cache, so a prefix segment that missed cache lands in it too —
            // it moved the wrong way between two runs whose tails differed by 1 KB. Tail
            // size is measured off the request we build instead: deterministic, free, and
            // exactly the thing a cheaper roster is supposed to move.
            if attempt.compliant() {
                compliant += 1;
                passes += 1;
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
            println!(
                "seq {:>4} · draw {draw}/{repeat} · {:<24} {}{against} · {seconds:.1}s",
                g.seq,
                called,
                outcome(&attempt)
            );
            if let Some(sink) = &mut sink {
                for c in &rows {
                    writeln!(sink, "{}", serde_json::to_string(c)?)?;
                }
            }
            compared.extend(rows);
            // A failing draw prints what it reached for and what it said. The tool name
            // alone says the peek answered with the wrong thing; the arguments and the
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
        }
        match (&g.recorded_first_failure, passes == repeat, passes == 0) {
            (Some(_), true, _) => fixed += 1,
            (None, _, true) => broke += 1,
            _ => {}
        }
    }

    let recorded_failures = selected
        .iter()
        .filter(|g| g.recorded_first_failure.is_some())
        .count();
    println!("\nPEEK REPLAY · {model}");
    // The fork tail is uncached by construction, so this is what a gate costs beyond
    // re-reading the prefix — paid on every tool result, for the whole session. It is the
    // number a cheaper roster has to move, and it belongs next to the failure rate because
    // the two are the trade.
    println!(
        "  tail      {} chars per gate, uncached (result + roster + ask)",
        uncached / selected.len().max(1) as u64
    );
    println!(
        "  recorded  {recorded_failures}/{} gates failed their first attempt  ({:.0}%)",
        selected.len(),
        100.0 * recorded_failures as f64 / selected.len() as f64
    );
    println!(
        "  replayed  {}/{draws} draws failed  ({:.0}%)",
        draws - compliant,
        100.0 * (draws - compliant) as f64 / draws as f64
    );
    // The paired view: an aggregate rate can move without any gate changing behaviour,
    // so the gates that flipped are reported separately from the totals.
    println!(
        "  flipped   {fixed} recorded-failure gates now pass every draw · {broke} recorded-pass gates now fail every draw"
    );
    // Output carries reasoning tokens, which differ by an order of magnitude between
    // models and are the term that makes a borrowed cost estimate wrong. A short
    // `--limit` probe reports this on the model itself.
    if transport_errors > 0 {
        println!(
            "  errors    {transport_errors} draw(s) returned nothing parseable (counted as failures)"
        );
    }
    println!(
        "  tokens    {}k prompt · {} output per draw (output includes reasoning)",
        prompt_tokens / draws.max(1) as u64 / 1000,
        out_tokens / draws.max(1) as u64
    );
    println!(
        "  cache     {cache_read} tokens read (a cold total means the replay isn't reading the session's prefix)"
    );
    // On the critical path of every tool call: a verdict's latency is paid by the
    // agent's step, so it is reported beside tokens rather than left to the clock.
    println!(
        "  latency   {:.1}s per draw, request to verdict",
        seconds_total / draws.max(1) as f64
    );
    if !compared.is_empty() {
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
        print_table(table, &compared);
        if let Some(path) = &out {
            println!("\n{n} rows written to {}", path.display());
        }
    }
    Ok(())
}
