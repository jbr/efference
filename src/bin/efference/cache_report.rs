//! `efference dev cache` — read a session's `usage.jsonl` and report the prompt-cache
//! read rate (docs/log-and-projection.md, docs/operating.md).
//!
//! The pins in `project.rs` and `api/test.rs` prove the harness still emits the
//! bytes it emitted yesterday. Only a read rate proves the *provider* still matches
//! them, so this verb replays the recorded per-request usage lines through the same
//! [`ColdStreak`] the daemon folds live, and reports what came back warm — by
//! request origin, with the cold reads that need no explanation (session boot, a
//! model change, an idle gap past the cache TTL) separated from the ones that do.

use super::*;

use efference::daemon::{CacheRead, ColdStart, ColdStreak, RequestOrigin, UsageLine};

pub(crate) const USAGE_NAME: &str = "usage.jsonl";

/// Per-origin counts for the table: how many requests of this shape, how many of
/// them read a cached prefix, and what they cost.
///
/// [`Self::input`] is the column the rate cannot supply. A read rate calls a request
/// warm on any nonzero read, so a session that matched its preamble and paid for the
/// rest of every 50k context reports as warm; the uncached total states what that
/// came to. It is a recorded fact rather than a model of what a perfect cache would
/// have covered — the prefix a request was *entitled* to reuse depends on breakpoint
/// placement, the model's cache policy, and the upstream's own behaviour, none of
/// which this record carries (docs/log-and-projection.md).
#[derive(Default, Clone, Copy)]
pub(crate) struct OriginTally {
    pub(crate) requests: u64,
    pub(crate) warm: u64,
    pub(crate) cache_read: u64,
    pub(crate) input: u64,
    pub(crate) output: u64,
}

/// What one session's usage record says about its cache (docs/log-and-projection.md).
/// `turn` rows carry the classification; every origin carries a read rate, because a
/// side channel going cold is its own (different) signal.
#[derive(Default)]
pub(crate) struct CacheReport {
    pub(crate) sessions: usize,
    pub(crate) lines: u64,
    /// Tallies in [`ORIGIN_ORDER`]'s order, so the table's rows are stable.
    pub(crate) by_origin: Vec<(RequestOrigin, OriginTally)>,
    /// Cold main-loop reads with a reason: boot, model change, idle past the TTL.
    pub(crate) cold_starts: [u64; 3],
    /// Cold main-loop reads with none — the prefix-moved evidence.
    pub(crate) unexplained: u64,
    /// The longest run of consecutive unexplained cold main-loop reads.
    pub(crate) longest_streak: u32,
    /// Which session held [`Self::longest_streak`], for the corpus sweep.
    pub(crate) longest_streak_session: String,
    /// The streak as the record ends — a live session's current standing.
    pub(crate) final_streak: u32,
}

/// The table's row order: the main loop first, then the side channels in the order
/// they cost tokens.
const ORIGIN_ORDER: [RequestOrigin; 8] = [
    RequestOrigin::Turn,
    RequestOrigin::Peek,
    RequestOrigin::Spot,
    RequestOrigin::Triage,
    RequestOrigin::Title,
    RequestOrigin::Keepalive,
    RequestOrigin::AbortedAttempt,
    RequestOrigin::Unknown,
];

fn origin_label(origin: RequestOrigin) -> &'static str {
    match origin {
        RequestOrigin::Turn => "turn",
        RequestOrigin::Peek => "peek",
        RequestOrigin::Spot => "spot",
        RequestOrigin::Triage => "triage",
        RequestOrigin::Title => "title",
        RequestOrigin::Keepalive => "keepalive",
        RequestOrigin::AbortedAttempt => "aborted",
        RequestOrigin::Unknown => "unknown",
    }
}

/// Fold one or more sessions' usage lines into a report. Each session gets its own
/// [`ColdStreak`], because a prefix belongs to a session — running the corpus
/// through one tracker would join the end of one session to the start of the next.
/// Unparseable lines are skipped: the record is diagnostic, and a truncated final
/// line (the daemon was killed mid-flush) must not cost the whole report.
pub(crate) fn fold_sessions(sessions: &[(String, Vec<UsageLine>)]) -> CacheReport {
    let mut report = CacheReport::default();
    let mut tallies: Vec<(RequestOrigin, OriginTally)> = ORIGIN_ORDER
        .iter()
        .map(|o| (*o, OriginTally::default()))
        .collect();
    for (session, lines) in sessions {
        if lines.is_empty() {
            continue;
        }
        report.sessions += 1;
        let mut streak = ColdStreak::default();
        for line in lines {
            report.lines += 1;
            if let Some((_, tally)) = tallies.iter_mut().find(|(o, _)| *o == line.origin) {
                tally.requests += 1;
                tally.cache_read += line.cache_read;
                tally.input += line.input;
                tally.output += line.output;
                if line.cache_read > 0 {
                    tally.warm += 1;
                }
            }
            let verdict = streak.observe(
                line.origin,
                line.at_ms,
                line.model.as_deref(),
                line.cache_read,
            );
            match verdict {
                Some(CacheRead::ColdStart(ColdStart::Boot)) => report.cold_starts[0] += 1,
                Some(CacheRead::ColdStart(ColdStart::ModelChange)) => report.cold_starts[1] += 1,
                Some(CacheRead::ColdStart(ColdStart::Idle)) => report.cold_starts[2] += 1,
                Some(CacheRead::Cold) => report.unexplained += 1,
                Some(CacheRead::Warm) | None => {}
            }
            if streak.streak() > report.longest_streak {
                report.longest_streak = streak.streak();
                report.longest_streak_session = session.clone();
            }
        }
        report.final_streak = streak.streak();
    }
    report.by_origin = tallies;
    report
}

/// Read one session's usage record. A missing file is an empty record rather than an
/// error — a session that made no requests, or one from before the record existed.
pub(crate) fn read_usage(path: &Path) -> std::io::Result<Vec<UsageLine>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<UsageLine>(line).ok())
        .collect())
}

/// Which usage records the report reads: `--all` sweeps every session under
/// `.efference/`, an explicit target resolves like the other reader verbs (a path to
/// a `usage.jsonl`, a session dir, or a session id), and neither takes the most
/// recently written session — the one an operator just ran.
pub(crate) fn resolve_usage_targets(
    target: Option<&str>,
    all: bool,
) -> Result<Vec<(String, PathBuf)>, CliError> {
    if all {
        let found: Vec<(String, PathBuf)> = session_ids()
            .into_iter()
            .map(|id| {
                let path = session_dir(&id).join(USAGE_NAME);
                (id, path)
            })
            .filter(|(_, path)| path.is_file())
            .collect();
        if found.is_empty() {
            return Err(format!("no {USAGE_NAME} under {BASE_DIR}/.").into());
        }
        return Ok(found);
    }
    if let Some(target) = target {
        let as_path = Path::new(target);
        if as_path.is_file() {
            return Ok(vec![(target.to_string(), as_path.to_path_buf())]);
        }
        let dir = if as_path.is_dir() {
            as_path.to_path_buf()
        } else {
            session_dir(target)
        };
        let usage = dir.join(USAGE_NAME);
        if usage.is_file() {
            return Ok(vec![(target.to_string(), usage)]);
        }
        return Err(format!("no {USAGE_NAME} for {target:?}.{}", session_listing()).into());
    }
    // `session_ids` is most-recently-written first, so the head is the session that
    // just ran — including the live one, whose record is readable mid-session.
    for id in session_ids() {
        let usage = session_dir(&id).join(USAGE_NAME);
        if usage.is_file() {
            return Ok(vec![(id, usage)]);
        }
    }
    Err(format!("no session has a {USAGE_NAME}.{}", session_listing()).into())
}

/// Print the report (docs/operating.md): a read-rate row per origin, then the cold
/// main-loop reads split by whether the prefix has to explain them.
pub(crate) fn cache(session: Option<String>, name: Option<String>, all: bool) -> CliResult {
    use efference::daemon::COLD_STREAK_WARN;
    use std::io::IsTerminal;

    // The positional wins, then the global `--name` (as `view` reads it), then the
    // most recently written session.
    let target = session.or(name);
    let targets = resolve_usage_targets(target.as_deref(), all)?;
    let mut sessions = Vec::new();
    for (id, path) in &targets {
        match read_usage(path) {
            Ok(lines) => sessions.push((id.clone(), lines)),
            Err(e) => eprintln!("skipping {}: {e}", path.display()),
        }
    }
    let report = fold_sessions(&sessions);
    let tty = std::io::stdout().is_terminal();

    let scope = if targets.len() == 1 {
        targets[0].0.clone()
    } else {
        format!("{} sessions", report.sessions)
    };
    println!("{}", paint(&format!("CACHE READS  {scope}"), BOLD, tty));
    if report.lines == 0 {
        println!("  no recorded requests.");
        return Ok(());
    }
    println!(
        "  {:<10} {:>7} {:>7} {:>6} {:>11} {:>11}",
        "origin", "reqs", "warm", "rate", "read tok", "uncached"
    );
    for (origin, tally) in &report.by_origin {
        if tally.requests == 0 {
            continue;
        }
        let rate = tally.warm as f64 * 100.0 / tally.requests as f64;
        // The main loop is the row the pins are about; a low rate there is the
        // finding, and the colour says so at a glance.
        let code = match (*origin == RequestOrigin::Turn, rate) {
            (true, r) if r < 50.0 => RED,
            (true, r) if r < 80.0 => YELLOW,
            (true, _) => GREEN,
            (false, _) => DIM,
        };
        println!(
            "  {:<10} {:>7} {:>7} {:>5}% {:>11} {:>11}",
            paint(origin_label(*origin), code, tty),
            tally.requests,
            tally.warm,
            format!("{rate:.0}"),
            tally.cache_read,
            tally.input
        );
    }
    println!();
    let aborted = report
        .by_origin
        .iter()
        .find(|(o, _)| *o == RequestOrigin::AbortedAttempt)
        .map(|(_, t)| t.requests)
        .unwrap_or(0);
    let turns = report
        .by_origin
        .iter()
        .find(|(o, _)| *o == RequestOrigin::Turn)
        .map(|(_, t)| t.requests)
        .unwrap_or(0);
    if aborted > 0 {
        let attempts = aborted + turns;
        let rate = aborted as f64 * 100.0 / attempts as f64;
        println!(
            "  abort rate (main loop)    {:>4}   {rate:.0}% of turn attempts ({aborted} of {attempts})",
            aborted
        );
    }
    let [boot, model_change, idle] = report.cold_starts;
    println!(
        "  cold starts (main loop)   {:>4}   {boot} boot · {model_change} model change · {idle} idle past TTL",
        boot + model_change + idle
    );
    println!(
        "  unexplained cold reads    {:>4}   longest run {}{}",
        report.unexplained,
        report.longest_streak,
        if report.longest_streak_session.is_empty() || targets.len() == 1 {
            String::new()
        } else {
            format!(" ({})", report.longest_streak_session)
        }
    );
    // The live standing, for the single-session read: what the sidebar is showing
    // right now, in the same units.
    if targets.len() == 1 {
        let line = format!(
            "  current streak            {:>4}   warn at {COLD_STREAK_WARN}",
            report.final_streak
        );
        let code = if report.final_streak >= COLD_STREAK_WARN {
            RED
        } else {
            DIM
        };
        println!("{}", paint(&line, code, tty));
    }
    if report.longest_streak >= COLD_STREAK_WARN {
        println!();
        println!(
            "{}",
            paint(
                "  A run this long means the provider stopped matching the prefix — check\n  \
                 what moved between requests (a dependency's serialization, a per-session\n  \
                 field on the wire, a provider-side normalization). `efference dev wire` prints\n  \
                 the projection the pins cover.",
                DIM,
                tty
            )
        );
    }
    Ok(())
}
