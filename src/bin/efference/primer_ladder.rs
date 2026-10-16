//! Timed primer/peek cache ladder (docs/transport.md).
//!
//! Experiment verb, dev-gated: plants triage primers at controlled gaps against one
//! recorded gate and replays the recorded peek per rung, reporting cache reads.
//! Decides eviction vs write-visibility lag vs envelope keying without spending
//! survey gates: a back-to-back primer pair that chains proves truncated primers
//! plant; a gapped pair that misses bounds the lifetime; a peek after a forced
//! primer is the v1.1 verdict live. Sends real requests unless `--dry-run`.

use super::*;
use efference::distill::{ForkPeek, Pending, TriageModel, TriageView};
use efference::log::Payload;
use std::io::Write;

/// Parse `--gaps` (`"0,120,300,480"`) into seconds between primer pairs.
pub(crate) fn parse_gaps(gaps: &str) -> Result<Vec<u64>, CliError> {
    gaps.split(',')
        .map(|g| {
            g.trim()
                .parse::<u64>()
                .map_err(|_| format!("gaps must be comma-separated seconds, got `{g}`").into())
        })
        .collect()
}

/// One ladder report line off a primer's usage.
fn line(label: &str, usage: &efference::api::RequestUsage) -> String {
    format!(
        "{label:<24} in {:<8} read {:<8} out {:<6}",
        usage.usage.input_tokens, usage.usage.cache_read_input_tokens, usage.usage.output_tokens
    )
}

/// One primer send through the own peek. `None` where no implicit lineage exists;
/// an `Err` where the send failed (warned at the send site).
async fn plant(
    peek: &ForkPeek,
    turns: &[efference::project::WireTurn],
    tools: &[serde_json::Value],
    choice: efference::api::ToolChoice,
) -> Option<Result<efference::api::RequestUsage, efference::api::Error>> {
    let outcome = peek.prime_with(turns, tools, choice).await?;
    Some(outcome.usage.ok_or_else(|| {
        Box::new(std::io::Error::other(
            outcome
                .error
                .as_deref()
                .unwrap_or("primer sent but recorded no usage"),
        )) as efference::api::Error
    }))
}

/// Run the ladder against one recorded gate: the deepest gate by default, which is
/// where a cold prefix hurts most.
#[allow(clippy::too_many_arguments)] // one flag, one parameter; the CLI is the arity
pub(crate) async fn ladder(
    path: PathBuf,
    seq: Option<u64>,
    gaps: &str,
    dry_run: bool,
    model: &str,
    triage: Option<TriageModel>,
    out: Option<PathBuf>,
) -> CliResult {
    let gaps = parse_gaps(gaps)?;
    let (path, label) = resolve_session_log(&path.to_string_lossy())?;
    let entries = efference::persist::load(&path)?;
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

    let all = super::peek_replay::gates(&entries);
    let gate = match seq {
        Some(seq) => all
            .iter()
            .find(|g| g.seq == seq)
            .ok_or_else(|| format!("no gate triaging result seq {seq} in {}", path.display()))?,
        None => all
            .last()
            .ok_or_else(|| format!("no gates in {}", path.display()))?,
    };

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

    // The primer input: the main request's turns through this gate's calls, borrowed
    // the way the live loop borrows them — byte-identical by construction. Through
    // the assistant turn's *callers*, not its calls: the live input ends before the
    // calls it produced, and replaying them without their results is refused.
    let prefix = &entries[..gate.prefix_end];
    let mut turns = efference::project::project_semantic(&entries[..gate.calls_at]);
    turns.push(efference::project::WireTurn::Breakpoint);
    let pending: Vec<Pending<'_>> = gate
        .calls
        .iter()
        .zip(&gate.raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();

    let mut rows: Vec<String> = Vec::new();
    let mut emit = |row: String| {
        println!("{row}");
        rows.push(row);
    };
    emit(format!(
        "ladder on {label} gate seq {} · {} primer turns · {} result(s)",
        gate.seq,
        turns.len(),
        pending.len()
    ));
    if dry_run {
        emit(
            "dry run: A auto/auto · B ladder · C auto→peek · D forced→peek · E peek-tools→peek"
                .into(),
        );
        return Ok(());
    }

    let inference = inference?;
    let (main_selection, mut endpoint) = inference.main();
    let model = main_selection.model.as_str();
    if let Some(dir) = path.parent() {
        endpoint.session_id = Some(efference::api::sticky_session_id(dir));
    }
    let agent_model = recorded_identity
        .map(|identity| identity.model.clone())
        .or_else(|| super::peek_replay::session_model(&system))
        .unwrap_or_else(|| model.to_string());
    let triage_selection = inference.triage().ok_or(
        "the ladder plants on the own triage lineage; this session resolves a shared peek",
    )?;
    let (_, mut triage_endpoint, t) = triage_selection;
    triage_endpoint.session_id = endpoint.session_id.clone();
    let triage_system =
        efference::env::triage_system_prompt(&t.model, &agent_model, context_block.as_deref());
    let (peek, _) = ForkPeek::for_triage_model(
        &triage_endpoint,
        t,
        triage_system,
        std::time::Duration::from_secs(600),
    )
    .await?;
    let client = Client::new(endpoint, model, system, None);

    // Arm A: two auto primers back-to-back — the control. Same shape, no gap for

    // eviction; a miss here kills shape theories outright (no-write or drift).
    for label in ["A plant(auto)", "A measure(auto)"] {
        match plant(&peek, &turns, &tools, efference::api::ToolChoice::auto()).await {
            Some(Ok(usage)) => emit(line(label, &usage)),
            Some(Err(e)) => emit(format!("{label:<24} ERROR {e}")),
            None => emit(format!("{label:<24} SKIPPED no implicit lineage")),
        }
    }
    // Arm B: the ladder. Each rung plants fresh, sleeps the gap, then measures —
    // every rung is self-contained, so order effects only warm the plants.
    for gap in &gaps {
        match plant(&peek, &turns, &tools, efference::api::ToolChoice::auto()).await {
            Some(Ok(usage)) => emit(line("B plant(auto)", &usage)),
            Some(Err(e)) => emit(format!("B plant(auto)            ERROR {e}")),
            None => emit("B plant(auto)            SKIPPED no implicit lineage".into()),
        }
        if *gap > 0 {
            eprintln!("sleeping {gap}s …");
            tokio::time::sleep(std::time::Duration::from_secs(*gap)).await;
        }
        match plant(&peek, &turns, &tools, efference::api::ToolChoice::auto()).await {
            Some(Ok(usage)) => emit(line(&format!("B measure(auto,{gap}s)"), &usage)),
            Some(Err(e)) => emit(format!("B measure(auto,{gap}s)      ERROR {e}")),
            None => emit(format!(
                "B measure(auto,{gap}s)      SKIPPED no implicit lineage"
            )),
        }
    }
    // Arms C and D: the recorded peek after an auto primer (v1 as shipped) and
    // after a forced primer (the choice fix). First attempt only — the measurement.
    for (label, choice) in [
        ("C auto→peek", efference::api::ToolChoice::auto()),
        ("D forced→peek", efference::api::ToolChoice::force("retain")),
    ] {
        match plant(&peek, &turns, &tools, choice).await {
            Some(Ok(usage)) => emit(line(&format!("{label} plant"), &usage)),
            Some(Err(e)) => emit(format!("{label} plant            ERROR {e}")),
            None => emit(format!(
                "{label} plant            SKIPPED no implicit lineage"
            )),
        }
        match peek
            .attempt(
                &client,
                &tools,
                TriageView::Transcript,
                prefix,
                &pending,
                &[],
                &[],
            )
            .await
        {
            Ok(attempt) => emit(line(&format!("{label} peek"), &attempt.usage)),
            Err(e) => emit(format!("{label} peek              DEFECT {e}")),
        }
    }
    // Arm E: the recorded peek after a primer sending the peek's own envelope —
    // peek tools plus forced choice, sharing only the input prefix. If the backend
    // keys the full envelope, this is the plant the peek resumes.
    {
        use efference::distill::{retain_tool, triage_tool};
        let peek_tools = vec![retain_tool(), triage_tool()];
        match plant(
            &peek,
            &turns,
            &peek_tools,
            efference::api::ToolChoice::force("retain"),
        )
        .await
        {
            Some(Ok(usage)) => emit(line("E peek-tools→peek plant", &usage)),
            Some(Err(e)) => emit(format!("E peek-tools→peek plant   ERROR {e}")),
            None => emit("E peek-tools→peek plant   SKIPPED no implicit lineage".into()),
        }
        match peek
            .attempt(
                &client,
                &tools,
                TriageView::Transcript,
                prefix,
                &pending,
                &[],
                &[],
            )
            .await
        {
            Ok(attempt) => emit(line("E peek-tools→peek peek", &attempt.usage)),
            Err(e) => emit(format!("E peek-tools→peek peek     DEFECT {e}")),
        }
    }

    if let Some(path) = out {
        let mut sink = std::io::BufWriter::new(std::fs::File::create(path)?);
        for row in &rows {
            writeln!(sink, "{row}")?;
        }
    }
    Ok(())
}
