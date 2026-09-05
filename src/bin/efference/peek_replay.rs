//! `efference peek` — replay a recorded session's fork gates (docs/redaction-fork.md).
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
use efference::distill::{Pending, peek_attempt, peek_request};
use efference::log::{Payload, RawResult, ToolCall};

/// One recorded gate, rebuilt from the log: the prefix the peek saw, and the results it
/// was asked to triage.
///
/// The prefix ends **before** the gate's tool results, because that is where the live
/// loop snapshots it — `peek_messages` appends the results itself as fresh
/// `tool_result` blocks, so a prefix that already contained them would double them.
pub(crate) struct Gate {
    /// Index into the log of the first result this gate carries — the prefix boundary.
    pub(crate) prefix_end: usize,
    /// The `seq` of the first result, which is how `--seq` addresses a gate and how the
    /// recorded `Triage.for_result` names it.
    pub(crate) seq: u64,
    pub(crate) calls: Vec<ToolCall>,
    pub(crate) raws: Vec<RawResult>,
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
    // Every result, by call id, and every gate's recorded first-attempt breadcrumb.
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
        let mut kept_calls = Vec::new();
        let mut prefix_end = None;
        let mut seq = 0;
        for (j, later) in entries.iter().enumerate().skip(i + 1) {
            match &later.payload {
                Payload::ToolResult { call_id, raw, .. } if ids.contains(&call_id.0.as_str()) => {
                    if prefix_end.is_none() {
                        prefix_end = Some(j);
                        seq = later.seq;
                    }
                    if let Some(call) = calls.iter().find(|c| c.id == *call_id) {
                        kept_calls.push(call.clone());
                        raws.push(raw.clone());
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
            seq,
            calls: kept_calls,
            raws,
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

/// Replay one gate or every gate in a recorded session, `repeat` draws each.
pub(crate) async fn peek(
    path: PathBuf,
    seq: Option<u64>,
    repeat: usize,
    limit: Option<usize>,
    dry_run: bool,
    model: &str,
) -> CliResult {
    // A `log.jsonl`, or a session dir / name that resolves to one — the same target
    // grammar the other reader verbs take.
    let (path, label) = resolve_session_log(&path.to_string_lossy())?;
    let entries = efference::persist::load(&path)?;
    // Replays use the env-declared provider's projection-shaping choices — image
    // placement and cache-breakpoint placement — matching what a live session against
    // that provider projects; a keyless dry run falls back to the defaults
    // (docs/log-and-projection.md).
    let provider = efference::api::Endpoint::from_env()
        .map(|e| e.provider)
        .ok();
    let placement = provider
        .map(|p| p.image_placement(model))
        .unwrap_or_default();
    let breakpoint = provider
        .map(|p| p.breakpoint_placement())
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
    let system = entries
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            Payload::Preamble { system, .. } => Some(system.clone()),
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
                tail_chars(&peek_request(&entries[..g.prefix_end], &pending, &[], &[]))
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
        let messages = peek_request(&entries[..g.prefix_end], &pending, &[], &[]);
        eprintln!("note: gate at seq {} · {} result(s)", g.seq, pending.len());
        let messages = efference::project::encode_messages(
            &messages[messages.len() - 1..],
            placement,
            breakpoint,
        );
        println!("{}", efference::api::render_messages_pretty(&messages)?);
        return Ok(());
    }

    let endpoint = efference::api::Endpoint::from_env()?;
    let client = Client::new(endpoint, model, system, None);

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
        uncached += tail_chars(&peek_request(&entries[..g.prefix_end], &pending, &[], &[])) as u64;
        let mut passes = 0usize;
        for draw in 1..=repeat {
            // A transport error is this draw's problem, not the sweep's. Propagating it
            // threw away 160-odd completed draws when one response came back with
            // malformed JSON — and a model that sometimes fails to produce a parseable
            // response *is* part of what a cross-model comparison is measuring, so it is
            // recorded as a failed draw rather than hidden by aborting.
            let attempt = match peek_attempt(
                &client,
                &tools,
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
            println!(
                "seq {:>4} · draw {draw}/{repeat} · {:<24} {}",
                g.seq,
                called,
                outcome(&attempt)
            );
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
    Ok(())
}
