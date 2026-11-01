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

use efference::daemon::{CacheRead, ColdStart, ColdStreak, USAGE_NAME, UsageLine};
use efference::inference::api::{CacheMiss, CacheMissReason};
use efference::record::log::RequestOrigin;

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

/// Provider-attributed cache misses by reason (docs/transport.md) — what the provider
/// itself said moved, which is the one thing a token total cannot tell you.
///
/// The four `*_changed` reasons are divergences and count toward [`Self::total`]'s
/// first line; the other two report that no comparison happened at all, so they explain
/// nothing about a read but are worth seeing when they dominate a record — a run of
/// `previous_message_not_found` means the chain broke, not the prefix.
///
/// Counted for every line that carries one, warm or cold, because a divergence the
/// provider named on a read that *still* hit an earlier breakpoint is a real finding
/// with low impact, and hiding it behind the cold-read columns would lose it.
#[derive(Default)]
pub(crate) struct Attributed {
    pub(crate) model_changed: u64,
    pub(crate) system_changed: u64,
    pub(crate) tools_changed: u64,
    pub(crate) messages_changed: u64,
    /// The chain broke: no stored fingerprint for the id we named, so nothing was
    /// compared — the previous request did not carry the object, or it aged out of the
    /// provider's retention.
    pub(crate) previous_message_not_found: u64,
    /// A prompt-affecting parameter the comparison does not cover differs, or the
    /// divergence sits past the provider's horizon: the provider's way of saying it could
    /// not answer. This harness produces some of these itself, by moving `tool_choice`
    /// or `thinking` on a retry (docs/transport.md), so a count here is not by itself a
    /// defect.
    pub(crate) unavailable: u64,
    /// A reason this build does not know, kept visible rather than folded in so a new
    /// provider value shows up as itself.
    pub(crate) other: u64,
}

impl Attributed {
    pub(crate) fn observe(&mut self, miss: &CacheMiss) {
        match miss.reason {
            CacheMissReason::ModelChanged => self.model_changed += 1,
            CacheMissReason::SystemChanged => self.system_changed += 1,
            CacheMissReason::ToolsChanged => self.tools_changed += 1,
            CacheMissReason::MessagesChanged => self.messages_changed += 1,
            CacheMissReason::PreviousMessageNotFound => self.previous_message_not_found += 1,
            CacheMissReason::Unavailable => self.unavailable += 1,
            CacheMissReason::Other => self.other += 1,
        }
    }

    /// How many misses the provider attributed to a divergence.
    pub(crate) fn divergences(&self) -> u64 {
        self.model_changed + self.system_changed + self.tools_changed + self.messages_changed
    }

    /// How many requests the provider answered no comparison for — a fact about the
    /// chain or about parameters the comparison does not cover, never about the prefix.
    pub(crate) fn no_comparison(&self) -> u64 {
        self.previous_message_not_found + self.unavailable + self.other
    }

    /// Every attribution on the record, no-comparison ones included.
    pub(crate) fn total(&self) -> u64 {
        self.divergences() + self.no_comparison()
    }
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
    /// Provider-attributed misses by reason, across every origin that opts in.
    pub(crate) attributed: Attributed,
    /// The longest run of consecutive unexplained cold main-loop reads.
    pub(crate) longest_streak: u32,
    /// Which session held [`Self::longest_streak`], for the corpus sweep.
    pub(crate) longest_streak_session: String,
    /// The streak as the record ends — a live session's current standing.
    pub(crate) final_streak: u32,
}

/// The table's row order: the main loop first, then the side channels in the order
/// they cost tokens.
const ORIGIN_ORDER: [RequestOrigin; 11] = [
    RequestOrigin::Turn,
    RequestOrigin::Triage,
    RequestOrigin::Spot,
    RequestOrigin::Title,
    RequestOrigin::Keepalive,
    RequestOrigin::TriagePrimer,
    RequestOrigin::TriageKeepalive,
    RequestOrigin::Compaction,
    RequestOrigin::Consolidation,
    RequestOrigin::AbortedAttempt,
    RequestOrigin::Unknown,
];

fn origin_label(origin: RequestOrigin) -> &'static str {
    match origin {
        RequestOrigin::Turn => "turn",
        RequestOrigin::Triage => "triage",
        RequestOrigin::Spot => "spot",
        RequestOrigin::Title => "title",
        RequestOrigin::Keepalive => "keepalive",
        RequestOrigin::TriagePrimer => "triage_primer",
        RequestOrigin::TriageKeepalive => "triage_keepalive",
        RequestOrigin::Compaction => "compaction",
        RequestOrigin::Consolidation => "consolidation",
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
            if let Some(miss) = &line.cache_miss {
                report.attributed.observe(miss);
            }
            let verdict = streak.observe_touching(
                line.origin,
                line.touches_main_prefix(),
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
            return Err(format!("no {USAGE_NAME} under {SESSION_BASE}/.").into());
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
    use efference::protocol::state::COLD_STREAK_WARN;
    use std::io::IsTerminal;

    // The positional wins, then `--name` (as `view` reads it), then the
    // most recently written session.
    let target = session.or(name);
    let targets = resolve_usage_targets(target.as_deref(), all)?;
    let mut sessions = Vec::new();
    for (id, path) in &targets {
        match UsageLine::read(path) {
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
    let single = targets.len() == 1;
    print_origins(&report, tty);
    println!();
    print_cold_reads(&report, single);
    print_attribution(&report);
    // The live standing, for the single-session read: what the sidebar is showing
    // right now, in the same units.
    if single {
        print_current_streak(&report, tty);
    }
    if report.longest_streak >= COLD_STREAK_WARN {
        print_streak_hint(tty);
    }
    Ok(())
}

/// The read rate per request origin.
fn print_origins(report: &CacheReport, tty: bool) {
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
}

/// The main loop's aborted attempts and cold starts, and the cold reads the prefix does not
/// explain. `single` is whether the report covers one session, whose longest run needs no
/// session name beside it.
fn print_cold_reads(report: &CacheReport, single: bool) {
    let aborted = report
        .by_origin
        .iter()
        .find(|(o, _)| *o == RequestOrigin::AbortedAttempt)
        .map_or(0, |(_, t)| t.requests);
    let turns = report
        .by_origin
        .iter()
        .find(|(o, _)| *o == RequestOrigin::Turn)
        .map_or(0, |(_, t)| t.requests);
    if aborted > 0 {
        let attempts = aborted + turns;
        let rate = aborted as f64 * 100.0 / attempts as f64;
        println!(
            "  abort rate (main loop)    {aborted:>4}   {rate:.0}% of turn attempts ({aborted} of {attempts})"
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
        if report.longest_streak_session.is_empty() || single {
            String::new()
        } else {
            format!(" ({})", report.longest_streak_session)
        }
    );
}

/// What the provider itself said moved, when the record carries any
/// (docs/transport.md). Printed only then: a provider without the feature writes no
/// such field, and a session whose prefix never moved gets nothing — so the two
/// lines address the reader who has a cold streak and no cause for it.
fn print_attribution(report: &CacheReport) {
    if report.attributed.total() > 0 {
        let a = &report.attributed;
        if a.divergences() > 0 {
            println!(
                "  provider named the cause  {:>4}   tools {} · system {} · messages {} · model {}",
                a.divergences(),
                a.tools_changed,
                a.system_changed,
                a.messages_changed,
                a.model_changed,
            );
        }
        if a.no_comparison() > 0 {
            println!(
                "  provider compared nothing {:>4}   {} no fingerprint · {} unavailable · {} unknown",
                a.no_comparison(),
                a.previous_message_not_found,
                a.unavailable,
                a.other,
            );
        }
    }
}

/// The cold-read run in force at the end of the record, against the sidebar's threshold.
fn print_current_streak(report: &CacheReport, tty: bool) {
    use efference::protocol::state::COLD_STREAK_WARN;

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

/// What a run of unexplained cold reads points at.
fn print_streak_hint(tty: bool) {
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
