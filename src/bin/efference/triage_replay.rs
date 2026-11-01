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
mod sweep;

use super::*;
use crate::dev::TriageArgs;
use efference::curation::triage::{Pending, TriageView, Triager, triage_request};
use efference::inference::TriageModel;
use efference::record::project::Reader;
use std::io::Write;

use compare::*;
pub(crate) use record::{Recorded, recorded_envelope, recorded_triages, session_model};
use sweep::{Replay, Sweep};

impl TriageArgs {
    /// The separate-model triage the `--triage-*` flags describe, when `--triage-model`
    /// names one.
    fn triage_model(&self) -> Option<TriageModel> {
        self.triage_model.clone().map(|model| TriageModel {
            model,
            thinking: self.triage_thinking,
            auto_choice: self.triage_auto_choice,
            effort: self.triage_effort.clone(),
        })
    }

    /// The history the replayed triage reads: `--view`, else the view the replayed
    /// configuration reads live, which is the text view under a separate triage model.
    fn view(&self, separate: bool) -> Result<TriageView, CliError> {
        match &self.view {
            Some(view) => TriageView::parse(view).ok_or_else(|| {
                format!("--view {view}: expected transcript, text, tail:N or intent").into()
            }),
            None if separate => Ok(TriageView::Text),
            None => Ok(TriageView::Transcript),
        }
    }
}

/// Replay one triage or every triage in a recorded session, `repeat` draws each.
pub(crate) async fn triage(args: TriageArgs) -> CliResult {
    let triage = args.triage_model();
    let view = args.view(triage.is_some())?;
    let TriageArgs {
        path,
        seq,
        repeat,
        limit,
        dry_run,
        model,
        out,
        table,
        ..
    } = args;
    let model = model.as_str();
    // A `log.jsonl`, or a session dir / name that resolves to one — the same target
    // grammar the other reader verbs take.
    let (path, label) = resolve_session_log(&path.to_string_lossy())?;
    let entries = efference::record::persist::load(&path)?;
    let inference = inference::resolve(
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
    let recorded_identity = efference::record::log::recorded_inference(&entries);
    let (placement, breakpoint) =
        projection_shapes(inference.as_ref().ok(), recorded_identity, model);
    let all = recorded_triages(&entries);
    let selected = select_triages(&all, &seq, limit, &path)?;
    note_unlogged_background(&selected);

    let (system, context_block, tools) = recorded_envelope(&entries)?;

    // The projection the replayed triage reads: the triage reader's on a separate model,
    // the main one on the shared triage (docs/summary.md).
    let reader = match triage {
        Some(_) => Reader::Triage,
        None => Reader::Main,
    };
    if dry_run {
        return dry_run_report(
            &entries,
            &selected,
            (reader, view),
            seq.is_empty(),
            (placement, breakpoint),
        );
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
        endpoint.session_id = Some(efference::inference::api::sticky_session_id(dir));
    }
    // The own triage replays the exact live shape (docs/triage.md): the same
    // constructor boot uses, the same system prompt over the recorded context
    // block, the session's own model named as the agent. The shared triage replays on
    // the agent client, whose model is `--model`.
    let agent_model = recorded_identity
        .map(|identity| identity.model.clone())
        .or_else(|| session_model(&system))
        .unwrap_or_else(|| model.to_string());
    let (triage, model) = replayed_triager(
        &inference,
        endpoint.session_id.as_deref(),
        &agent_model,
        context_block.as_deref(),
        model,
    )
    .await?;
    let client = Client::new(endpoint, model, system);
    let sink = match &out {
        Some(path) => Some(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => None,
    };

    eprintln!(
        "replaying {} triage(s) from {label} × {repeat} on {model} — first attempt only, no retries",
        selected.len()
    );
    let replay = Replay {
        entries: &entries,
        selected: &selected,
        reader,
        view,
        repeat,
    };
    let mut sweep = Sweep::new(replay, &triage, &client, &tools, sink);
    sweep.run().await?;
    sweep.print_summary(model, table, out.as_deref());
    Ok(())
}

/// The image and cache-breakpoint placement the replay projects with. Replays use the
/// selected connection's projection-shaping choices — image placement and cache-breakpoint
/// placement — matching what a live session on that provider projects. If the current
/// config cannot resolve the selection, the boot identity supplies the historical shapes; a
/// legacy log falls back to the defaults (docs/log-and-projection.md).
fn projection_shapes(
    inference: Option<&efference::inference::SessionInference>,
    recorded: Option<&efference::record::log::LoggedInference>,
    model: &str,
) -> (
    efference::record::project::ImagePlacement,
    efference::record::project::BreakpointPlacement,
) {
    let (provider, placement_model) = match inference {
        Some(inference) => (Some(inference.main().1.provider), model),
        None => (
            recorded.map(|identity| identity.provider),
            recorded.map_or(model, |identity| identity.model.as_str()),
        ),
    };
    (
        provider.map_or_default(|p| p.image_placement(placement_model)),
        provider.map_or_default(|p| p.breakpoint_placement(placement_model)),
    )
}

/// The triages to replay: those `--seq` names, else all, thinned to `limit` when given.
fn select_triages<'a>(
    all: &'a [Recorded],
    seq: &[u64],
    limit: Option<usize>,
    path: &Path,
) -> Result<Vec<&'a Recorded>, CliError> {
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
        return Err(match seq {
            [] => format!("no triages in {}", path.display()).into(),
            seqs => format!(
                "no triage triaging result seq {seqs:?} in {}",
                path.display()
            )
            .into(),
        });
    }
    Ok(selected)
}

/// Background-process tails are sampled live and never logged, so the replayed
/// request lacks that rider; say so, or a clean replay reads as a faithful one.
fn note_unlogged_background(selected: &[&Recorded]) {
    let with_processes = selected.iter().filter(|g| g.running_processes > 0).count();
    if with_processes > 0 {
        eprintln!(
            "note: {with_processes} of {} triage(s) had background processes running; their \
             tails were not logged, so the replayed request omits the background rider",
            selected.len()
        );
    }
}

/// A dry run: what the replayed requests would look like, and cost, with no request sent.
/// `whole_session` is whether no `--seq` narrowed the triages, which prints the session's
/// tail sizes rather than one request.
fn dry_run_report(
    entries: &[efference::record::log::LogEntry],
    selected: &[&Recorded],
    (reader, view): (Reader, TriageView),
    whole_session: bool,
    (placement, breakpoint): (
        efference::record::project::ImagePlacement,
        efference::record::project::BreakpointPlacement,
    ),
) -> CliResult {
    if view == TriageView::Text {
        layout::report(entries, selected, reader);
    }

    // A dry run over the whole session costs nothing and answers the other half of the
    // question a variant raises: what it costs. The tail is the fresh part of every triage's
    // request, so its mean size is what a cheaper roster is trying to move — and reading
    // it here rather than off `usage` keeps it free of cache jitter.
    if whole_session {
        let total: usize = selected
            .iter()
            .map(|g| {
                tail_chars(&triage_request(
                    &entries[..g.prefix_end],
                    reader,
                    view,
                    &g.pending(),
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
    let g = selected[0];
    let pending = g.pending();
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
    let messages = efference::record::project::encode_messages(shown, placement, breakpoint);
    println!(
        "{}",
        efference::inference::api::render_messages_pretty(&messages)?
    );
    Ok(())
}

/// The triager the replay asks, and the model it is named by. A separate triage model
/// replays the exact live shape (docs/triage.md); the shared triage replays on the agent
/// client, whose model is `model`. `session_id` is the sticky-routing key the main endpoint
/// carries.
async fn replayed_triager<'a>(
    inference: &'a efference::inference::SessionInference,
    session_id: Option<&str>,
    agent_model: &str,
    context_block: Option<&str>,
    model: &'a str,
) -> Result<(Triager, &'a str), CliError> {
    match inference.triage() {
        Some((_, mut triage_endpoint, t)) => {
            triage_endpoint.session_id = session_id.map(str::to_string);
            let triage_system =
                efference::config::env::triage_system_prompt(&t.model, agent_model, context_block);
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
                efference::host::boot::describe_window(caps.context_length),
                match (&t.effort, t.thinking) {
                    (Some(effort), _) => format!("on, effort {effort}"),
                    (None, true) => "on".into(),
                    (None, false) => "off".into(),
                },
                if t.auto_choice { "auto" } else { "forced" }
            );
            Ok((triage, t.model.as_str()))
        }
        None => Ok((Triager::shared(), model)),
    }
}
