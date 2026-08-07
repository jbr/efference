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
        let Payload::Assistant { calls, .. } = &entry.payload else {
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
    dry_run: bool,
    model: &str,
) -> CliResult {
    // A `log.jsonl`, or a session dir / name that resolves to one — the same target
    // grammar the other reader verbs take.
    let (path, label) = resolve_session_log(&path.to_string_lossy())?;
    let entries = efference::persist::load(&path)?;
    let all = gates(&entries);
    let selected: Vec<&Gate> = match seq {
        Some(s) => all.iter().filter(|g| g.seq == s).collect(),
        None => all.iter().collect(),
    };
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
        println!(
            "{}",
            efference::api::render_messages_pretty(&messages[messages.len() - 1..])?
        );
        return Ok(());
    }

    let endpoint = efference::api::Endpoint::from_env()?;
    let client = Client::new(endpoint, model, system);

    eprintln!(
        "replaying {} gate(s) from {label} × {repeat} on {model} — first attempt only, no retries",
        selected.len()
    );
    let (mut draws, mut compliant, mut cache_read) = (0usize, 0usize, 0u64);
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
        let mut passes = 0usize;
        for draw in 1..=repeat {
            let attempt = peek_attempt(
                &client,
                &tools,
                &entries[..g.prefix_end],
                &pending,
                &[],
                &[],
            )
            .await?;
            draws += 1;
            cache_read += attempt.usage.usage.cache_read_input_tokens;
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
    println!(
        "  cache     {cache_read} tokens read (a cold total means the replay isn't reading the session's prefix)"
    );
    Ok(())
}
