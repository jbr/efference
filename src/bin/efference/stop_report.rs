//! `efference dev stops` — read session logs and report what turn endings bought
//! (docs/operating.md).
//!
//! The classification is [`efference::log::stops`]; this is the table over it. The
//! number to read is the **barren** rate: endings where the model asked nothing and
//! the next turn supplied nothing, so the round trip cost a request and a cache
//! write and returned no information. It is the one stopping-behaviour measure the
//! existing corpus can answer, which makes it the baseline a preamble change is
//! judged against — so it is worth recording before changing the preamble, not
//! after.

use super::*;

use efference::log::{LogEntry, Stop, stops};

#[derive(Default)]
pub(crate) struct StopReport {
    pub(crate) sessions: usize,
    pub(crate) question: u64,
    pub(crate) answered: u64,
    pub(crate) barren: u64,
    pub(crate) final_stop: u64,
    /// The longest run of consecutive barren stops, and where it happened — a run
    /// is the shape a stalled session makes, where a scattered few are ordinary.
    pub(crate) longest_run: u32,
    pub(crate) longest_run_session: String,
    /// Barren stops whose reply came from a labelled client rather than the
    /// operator — a supervising agent spending its own window on the nudge.
    pub(crate) barren_from_agents: u64,
    /// Sessions with at least one barren stop, over sessions with any stop at all.
    pub(crate) sessions_with_barren: usize,
    pub(crate) sessions_with_stops: usize,
}

impl StopReport {
    pub(crate) fn stops(&self) -> u64 {
        self.question + self.answered + self.barren + self.final_stop
    }
}

pub(crate) fn fold(sessions: &[(String, Vec<LogEntry>)]) -> StopReport {
    let mut report = StopReport {
        sessions: sessions.len(),
        ..Default::default()
    };
    for (id, entries) in sessions {
        let stops = stops::classify(entries);
        if !stops.is_empty() {
            report.sessions_with_stops += 1;
        }
        let mut run = 0u32;
        let mut barren_here = false;
        for (stop, source) in &stops {
            match stop {
                Stop::Question => report.question += 1,
                Stop::Answered => report.answered += 1,
                Stop::Final => report.final_stop += 1,
                Stop::Barren => {
                    report.barren += 1;
                    barren_here = true;
                    if source.is_some() {
                        report.barren_from_agents += 1;
                    }
                }
            }
            if *stop == Stop::Barren {
                run += 1;
                if run > report.longest_run {
                    report.longest_run = run;
                    report.longest_run_session = id.clone();
                }
            } else {
                run = 0;
            }
        }
        if barren_here {
            report.sessions_with_barren += 1;
        }
    }
    report
}

fn read_log(path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let text = std::fs::read_to_string(path)?;
    // A truncated trailing line is ordinary in a live session's log; skip what will
    // not parse rather than refusing the whole record.
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<LogEntry>(line).ok())
        .collect())
}

pub(crate) fn stops_report(session: Option<String>, name: Option<String>, all: bool) -> CliResult {
    use std::io::IsTerminal;

    // Same resolution order the other reader verbs use: the positional, then the
    // global `--name`, then the most recently written session.
    let targets = if all {
        let found = spot_report::resolve_spotter_logs(None)?;
        if found.is_empty() {
            return Err(format!("no {LOG_NAME} under {BASE_DIR}/.").into());
        }
        found
    } else {
        let target = session
            .or(name)
            .or_else(|| session_ids().first().cloned())
            .ok_or_else(|| CliError::from(format!("no {LOG_NAME} under {BASE_DIR}/.")))?;
        let (path, label) = resolve_session_log(&target)?;
        vec![(label, path)]
    };

    let mut sessions = Vec::new();
    for (id, path) in &targets {
        match read_log(path) {
            Ok(entries) => sessions.push((id.clone(), entries)),
            Err(e) => eprintln!("skipping {}: {e}", path.display()),
        }
    }
    let report = fold(&sessions);
    let tty = std::io::stdout().is_terminal();

    let scope = if targets.len() == 1 {
        targets[0].0.clone()
    } else {
        format!("{} sessions", report.sessions)
    };
    println!("{}", paint(&format!("TURN ENDINGS  {scope}"), BOLD, tty));
    let total = report.stops();
    if total == 0 {
        println!("  no turn endings recorded.");
        return Ok(());
    }
    println!(
        "  {:<10} {:>7} {:>6}  what it bought",
        "ending", "count", "share"
    );
    let rows: [(&str, u64, &str, &str); 4] = [
        ("question", report.question, GREEN, "a question was put"),
        (
            "answered",
            report.answered,
            GREEN,
            "the reply carried something",
        ),
        ("final", report.final_stop, DIM, "nothing followed"),
        ("barren", report.barren, RED, "the reply carried nothing"),
    ];
    for (label, count, code, note) in rows {
        if count == 0 {
            continue;
        }
        println!(
            "  {:<10} {:>7} {:>5}%  {}",
            paint(label, code, tty),
            count,
            format!("{:.0}", count as f64 * 100.0 / total as f64),
            paint(note, DIM, tty),
        );
    }
    println!();
    println!(
        "  sessions with a barren stop  {:>4}   of {} with any ending",
        report.sessions_with_barren, report.sessions_with_stops
    );
    if report.barren_from_agents > 0 {
        println!(
            "  barren stops nudged by an agent {:>1}   a supervisor spending its window on \"continue\"",
            report.barren_from_agents
        );
    }
    let run = format!(
        "  longest run of barren stops  {:>4}{}",
        report.longest_run,
        if report.longest_run_session.is_empty() || targets.len() == 1 {
            String::new()
        } else {
            format!("   ({})", report.longest_run_session)
        }
    );
    println!(
        "{}",
        paint(&run, if report.longest_run >= 3 { RED } else { DIM }, tty)
    );
    if report.longest_run >= 3 {
        println!();
        println!(
            "{}",
            paint(
                "  A run means the session kept handing the turn back with nothing to say and\n  \
                 nothing to ask. `efference dev view <session>` shows what it said each time.",
                DIM,
                tty
            )
        );
    }
    Ok(())
}

#[cfg(test)]
mod test;
