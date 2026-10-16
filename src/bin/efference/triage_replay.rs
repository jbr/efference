//! `efference dev triage` — replay a recorded session's fork triages (docs/triage.md).
//!
//! A dev tool, like `wire`/`gallery`/`spotter`. The live triage retries, so a session's
//! `Triage.retries` records *that* a first attempt failed but can't compare two asks —
//! a change that halves first-attempt non-compliance and one that changes nothing both
//! end in a usable verdict. This re-sends a recorded triage's exact request, once, and
//! reports whether the first attempt answered.
//!
//! The eval set is the session itself: every triage carries its own prefix, at its own
//! session depth, and replaying one is a single warm prefix read.

mod compare;
mod layout;
mod record;

use super::*;
use efference::log::Payload;
use efference::project::Reader;
use efference::triage::{Pending, TriageModel, TriageView, Triager, triage_request};
use std::io::Write;

use compare::*;
pub(crate) use record::{Recorded, recorded_triages, session_model};

/// Replay one triage or every triage in a recorded session, `repeat` draws each.
#[allow(clippy::too_many_arguments)] // one flag, one parameter; the CLI is the arity
pub(crate) async fn triage(
    path: PathBuf,
    seq: Vec<u64>,
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
    let all = recorded_triages(&entries);
    let mut selected: Vec<&Recorded> = match seq.is_empty() {
        false => all.iter().filter(|g| seq.contains(&g.seq)).collect(),
        true => all.iter().collect(),
    };
    // Evenly spaced, not the first N. Failure rate climbs with session depth, so a prefix
    // of the triages samples the easy end and flatters the model.
    if let Some(limit) = limit.filter(|n| *n < selected.len() && *n > 0) {
        let step = selected.len() as f64 / limit as f64;
        selected = (0..limit)
            .map(|i| selected[(i as f64 * step) as usize])
            .collect();
    }
    if selected.is_empty() {
        return Err(match seq.as_slice() {
            [] => format!("no triages in {}", path.display()).into(),
            seqs => format!(
                "no triage triaging result seq {seqs:?} in {}",
                path.display()
            )
            .into(),
        });
    }

    // Background-process tails are sampled live and never logged, so the replayed
    // request lacks that rider; say so, or a clean replay reads as a faithful one.
    let with_processes = selected.iter().filter(|g| g.running_processes > 0).count();
    if with_processes > 0 {
        eprintln!(
            "note: {with_processes} of {} triage(s) had background processes running; their \
             tails were not logged, so the replayed request omits the background rider",
            selected.len()
        );
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

    // The projection the replayed triage reads: the triage reader's on a separate model,
    // the main one on the shared triage (docs/summary.md).
    let reader = match triage {
        Some(_) => Reader::Triage,
        None => Reader::Main,
    };
    if dry_run && view == TriageView::Text {
        layout::report(&entries, &selected, reader);
    }

    // A dry run over the whole session costs nothing and answers the other half of the
    // question a variant raises: what it costs. The tail is the fresh part of every triage's
    // request, so its mean size is what a cheaper roster is trying to move — and reading
    // it here rather than off `usage` keeps it free of cache jitter.
    if dry_run && seq.is_empty() {
        let total: usize = selected
            .iter()
            .map(|g| {
                let pending: Vec<Pending<'_>> = g
                    .calls
                    .iter()
                    .zip(&g.raws)
                    .map(|(call, raw)| Pending { call, raw })
                    .collect();
                tail_chars(&triage_request(
                    &entries[..g.prefix_end],
                    reader,
                    view,
                    &pending,
                    &g.inbox,
                    &[],
                ))
            })
            .sum();
        println!(
            "{} triages · mean tail {} chars · {} chars per session",
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
        let messages = triage_request(
            &entries[..g.prefix_end],
            reader,
            view,
            &pending,
            &g.inbox,
            &[],
        );
        eprintln!(
            "note: triage at seq {} · {} result(s)",
            g.seq,
            pending.len()
        );
        // The transcript view's prefix is the session's own wire, so only its fork tail
        // is printed; a text-only view is printed whole.
        let shown = match view {
            TriageView::Transcript => &messages[messages.len() - 1..],
            _ => &messages[..],
        };
        let messages = efference::project::encode_messages(shown, placement, breakpoint);
        println!("{}", efference::api::render_messages_pretty(&messages)?);
        return Ok(());
    }

    let inference = inference?;
    let (main_selection, mut endpoint) = inference.main();
    let model = main_selection.model.as_str();
    // The session's sticky-routing key (docs/transport.md), derived from its dir
    // exactly as boot derives it, so the replay reads the session's own prefix on
    // the upstream that holds it — and the own triage takes its `-triage` suffix, its
    // own lineage's key. Without it a provider with sticky routing re-derives a key
    // per request and the replay wanders between upstreams, cold every time.
    if let Some(dir) = path.parent() {
        endpoint.session_id = Some(efference::api::sticky_session_id(dir));
    }
    // The own triage replays the exact live shape (docs/triage.md): the same
    // constructor boot uses, the same system prompt over the recorded context
    // block, the session's own model named as the agent. The shared triage replays on
    // the agent client, whose model is `--model`.
    let agent_model = recorded_identity
        .map(|identity| identity.model.clone())
        .or_else(|| session_model(&system))
        .unwrap_or_else(|| model.to_string());
    let (triage, model) = match inference.triage() {
        Some((_, mut triage_endpoint, t)) => {
            triage_endpoint.session_id = endpoint.session_id.clone();
            let triage_system = efference::env::triage_system_prompt(
                &t.model,
                &agent_model,
                context_block.as_deref(),
            );
            let (triage, caps) = Triager::for_triage_model(
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
            (triage, t.model.as_str())
        }
        None => (Triager::shared(), model),
    };
    let client = Client::new(endpoint, model, system, None);
    let mut sink = match &out {
        Some(path) => Some(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => None,
    };
    let mut compared: Vec<Compared> = Vec::new();

    eprintln!(
        "replaying {} triage(s) from {label} × {repeat} on {model} — first attempt only, no retries",
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
    // Triages whose recorded first attempt failed, and whose replays now all pass (and the
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
        // its size is what a triage costs beyond re-reading the prefix, on every tool result.
        uncached += tail_chars(&triage_request(
            &entries[..g.prefix_end],
            triage.reader(),
            view,
            &pending,
            &g.inbox,
            &[],
        )) as u64;
        if view == TriageView::Text {
            println!(
                "seq {:>4} · {}",
                g.seq,
                layout::describe(&entries, g, reader)
            );
        }
        let mut passes = 0usize;
        for draw in 1..=repeat {
            // A transport error is this draw's problem, not the sweep's. Propagating it
            // threw away 160-odd completed draws when one response came back with
            // malformed JSON — and a model that sometimes fails to produce a parseable
            // response *is* part of what a cross-model comparison is measuring, so it is
            // recorded as a failed draw rather than hidden by aborting.
            let started = std::time::Instant::now();
            let attempt = match triage
                .attempt(
                    &client,
                    &tools,
                    view,
                    &entries[..g.prefix_end],
                    &pending,
                    &g.inbox,
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
        100.0 * (draws - compliant) as f64 / draws as f64
    );
    // The paired view: an aggregate rate can move without any gate changing behaviour,
    // so the triages that flipped are reported separately from the totals.
    println!(
        "  flipped   {fixed} recorded-failure triages now pass every draw · {broke} recorded-pass triages now fail every draw"
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
