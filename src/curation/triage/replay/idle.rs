//! Dev-tool replay of the standalone idle triage at a different effort
//! (docs/triage.md, docs/process-monitor.md).
//!
//! The question is narrow: when an event arrives while the session is idle, does the
//! wake/dismiss decision hold on a cheaper effort? The ask is not rebuilt here — the
//! measured path calls the production [`Triager::triage`], so the shared agent client, its
//! full `tools[]`, `tool_choice: auto`, the message shape and the retry policy are the ones
//! a live session would send. Only the effort on the client differs between the arms.
//!
//! # What the recorded log does not contain
//!
//! [`HISTORICAL_LIMITS`] is the short version. In detail:
//!
//! - The main agent's effort, thinking, temperature and upstream pin are **not recorded**;
//!   `recorded_inference` keeps connection, provider and model. An idle triage found in a
//!   session log therefore cannot be replayed at the effort it actually ran at, and the
//!   recorded verdict is not a baseline that a different effort can be measured against.
//! - A recorded idle `Triage` keeps dispositions, retries, thinking and usage but not the
//!   model's prose, and the running-process tails it was shown are sampled live and never
//!   logged (the same limit the per-result replay records).
//! - A logged verdict is one sample with no ground truth: nothing in the record says
//!   whether waking was the right call for that operator. Agreement with it measures
//!   self-consistency, not quality.
//!
//! So the comparison runs on synthetic cases with *labels* for the expected discretionary
//! decision. The cases are built through the production log builders, and the score is
//! against the label — never against what a model happened to answer.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::super::Triager;
use crate::curation::triage::BgVerdict;
use crate::inference::api::{Client, RequestUsage};
use crate::record::log::{Disposition, Origin, Seq};

/// The headline of what a session log cannot supply, for a caller's `--help` or report
/// header. The module docs have the reasoning.
pub const HISTORICAL_LIMITS: &str = "the main agent's effort is not logged, an idle triage's \
verdict is one sample with no ground truth, and the process tails it saw are never logged — \
so the historical log cannot supply either arm or a baseline; these cases are labelled \
instead";

mod cases;

pub use cases::*;

// ─────────────────────────────── attempts ───────────────────────────────

/// One idle triage at the effort of the client it was handed.
///
/// Production's in-fork retries are included, like a live idle triage; `retries` records how
/// many non-compliant attempts it took, so a cheaper arm that only complies on the second
/// ask is visible rather than folded into a verdict.
pub struct IdleAttempt {
    pub origin: Origin,
    pub inbox: Vec<(Seq, Disposition)>,
    pub background: Vec<(String, BgVerdict)>,
    pub retries: Vec<String>,
    pub thinking: Vec<String>,
    /// One entry per request the triage made, in-fork retries included.
    pub usage: Vec<RequestUsage>,
    pub elapsed: Duration,
}

impl IdleAttempt {
    /// The live loop's rule (docs/inbox.md): a trajectory starts iff an **inbox**
    /// disposition is a pin; with no pin the session stays idle. A background `Pin`
    /// snapshots a running tail onto the wire but starts no trajectory, so it is not a wake
    /// here either.
    pub fn wakes(&self) -> bool {
        self.inbox
            .iter()
            .any(|(_, d)| matches!(d, Disposition::Pin))
    }

    /// A usable verdict was produced. `false` means the model-free failure path: nothing
    /// pinned, nothing dismissed, and the event stays in the inbox for the next show.
    pub fn has_verdict(&self) -> bool {
        !matches!(self.origin, Origin::Fallback)
    }

    pub fn prompt_tokens(&self) -> u64 {
        self.usage.iter().map(|u| u.usage.prompt_tokens()).sum()
    }

    pub fn output_tokens(&self) -> u64 {
        self.usage.iter().map(|u| u.usage.output_tokens).sum()
    }

    pub fn cache_read_tokens(&self) -> u64 {
        self.usage
            .iter()
            .map(|u| u.usage.cache_read_input_tokens)
            .sum()
    }

    /// What the attempt decided, compactly — the per-draw trace in the report.
    pub fn decision(&self) -> String {
        if !self.has_verdict() {
            return "no verdict".to_string();
        }
        let mut parts = Vec::new();
        let mut pinned = Vec::new();
        let mut dismissed = Vec::new();
        for (seq, disposition) in &self.inbox {
            match disposition {
                Disposition::Pin => pinned.push(seq.to_string()),
                Disposition::Dismiss => dismissed.push(seq.to_string()),
            }
        }
        parts.push(if pinned.is_empty() {
            "no pin".to_string()
        } else {
            format!("pin [{}]", pinned.join(","))
        });
        if !dismissed.is_empty() {
            parts.push(format!("dismiss [{}]", dismissed.join(",")));
        }
        if !self.background.is_empty() {
            let tails: Vec<String> = self
                .background
                .iter()
                .map(|(handle, verdict)| {
                    let verdict = match verdict {
                        BgVerdict::Pin => "pin",
                        BgVerdict::Mute => "mute",
                    };
                    format!("{handle} {verdict}")
                })
                .collect();
            parts.push(format!("bg {}", tails.join(", ")));
        }
        parts.join(" · ")
    }
}

/// Run one case's ask through the production idle triage on `agent`'s client, and time it.
///
/// `agent` supplies the effort under test, the model, the system prompt and every world
/// tool; `tools` is the session's `tools[]` verbatim. Nothing here reads the case's label.
pub async fn idle_attempt(agent: &Client, tools: &[Value], case: &IdleCase) -> IdleAttempt {
    let started = Instant::now();
    let triage = Triager::shared()
        .triage(
            agent,
            tools,
            &case.prefix,
            &case.inbox,
            &case.background,
            &|_| {},
        )
        .await;
    IdleAttempt {
        origin: triage.origin,
        inbox: triage.inbox,
        background: triage.background,
        retries: triage.retries,
        thinking: triage.thinking,
        usage: triage.usage,
        elapsed: started.elapsed(),
    }
}

// ─────────────────────────────── scoring ───────────────────────────────

/// One arm's draws on one case. `matches_label` counts usable verdicts whose decision
/// matched the label; a failed parse is `no_verdicts`, not a dismissal, because the event is
/// left in the inbox rather than cleared.
#[derive(Default, Clone)]
pub struct IdleArmTally {
    pub draws: usize,
    /// Draws whose verdict was a wake, over usable verdicts. Kept raw rather than derived
    /// from `matches_label`, which counts dismissals on a no-wake case.
    pub wakes: usize,
    pub matches_label: usize,
    pub false_wakes: usize,
    pub false_dismisses: usize,
    pub no_verdicts: usize,
    pub retries: usize,
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub seconds: f64,
    /// One compact line per draw, in draw order.
    pub decisions: Vec<String>,
    /// The first usable draw whose decision missed the label, with the model's reasoning
    /// excerpted — the thing to read when an arm looks wrong.
    pub first_miss: Option<String>,
}

impl IdleArmTally {
    pub fn misses(&self) -> usize {
        self.false_wakes + self.false_dismisses + self.no_verdicts
    }

    pub fn average_seconds(&self) -> f64 {
        if self.draws == 0 {
            0.0
        } else {
            self.seconds / self.draws as f64
        }
    }

    /// The most frequent decision among this arm's draws, over usable verdicts only. `None` on a tie
    /// or with no usable verdict.
    pub fn modal_wake(&self) -> Option<bool> {
        let usable = self.draws - self.no_verdicts;
        if usable == 0 {
            return None;
        }
        if self.wakes * 2 > usable {
            Some(true)
        } else if self.wakes * 2 < usable {
            Some(false)
        } else {
            None
        }
    }

    fn merge(&mut self, other: &Self) {
        self.draws += other.draws;
        self.wakes += other.wakes;
        self.matches_label += other.matches_label;
        self.false_wakes += other.false_wakes;
        self.false_dismisses += other.false_dismisses;
        self.no_verdicts += other.no_verdicts;
        self.retries += other.retries;
        self.prompt_tokens += other.prompt_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.seconds += other.seconds;
    }
}

/// One case, both arms.
pub struct IdleCaseReport {
    pub name: &'static str,
    pub expects_wake: bool,
    pub rationale: &'static str,
    pub default: IdleArmTally,
    pub lower: IdleArmTally,
    /// The arms' modal decisions differ.
    pub disagrees: bool,
}

/// Both arms over the whole corpus.
pub struct IdleComparison {
    pub model: String,
    /// The level the lower arm was sent at; `None` means the arm omitted effort.
    pub lower_effort: Option<String>,
    pub cases: Vec<IdleCaseReport>,
}

fn tally(case: &IdleCase, attempts: Vec<IdleAttempt>) -> IdleArmTally {
    let mut tally = IdleArmTally::default();
    for attempt in attempts {
        tally.draws += 1;
        tally.retries += attempt.retries.len();
        tally.seconds += attempt.elapsed.as_secs_f64();
        tally.prompt_tokens += attempt.prompt_tokens();
        tally.output_tokens += attempt.output_tokens();
        tally.cache_read_tokens += attempt.cache_read_tokens();

        let decision = attempt.decision();
        if !attempt.has_verdict() {
            tally.no_verdicts += 1;
        } else {
            let woke = attempt.wakes();
            if woke {
                tally.wakes += 1;
            }
            if woke == case.expects_wake {
                tally.matches_label += 1;
            } else if woke {
                tally.false_wakes += 1;
                if tally.first_miss.is_none() {
                    tally.first_miss = Some(miss_line(&decision, &attempt));
                }
            } else {
                tally.false_dismisses += 1;
                if tally.first_miss.is_none() {
                    tally.first_miss = Some(miss_line(&decision, &attempt));
                }
            }
        }
        tally.decisions.push(decision);
    }
    tally
}

fn miss_line(decision: &str, attempt: &IdleAttempt) -> String {
    match attempt.thinking.last() {
        Some(thinking) if !thinking.trim().is_empty() => {
            format!("{decision} — {}", brief(thinking))
        }
        _ => decision.to_string(),
    }
}

fn brief(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = collapsed.chars().take(200).collect();
    if collapsed.chars().count() > 200 {
        short.push('…');
    }
    short
}

fn case_report(
    case: &IdleCase,
    default_draws: Vec<IdleAttempt>,
    lower_draws: Vec<IdleAttempt>,
) -> IdleCaseReport {
    let default = tally(case, default_draws);
    let lower = tally(case, lower_draws);
    let disagrees = match (default.modal_wake(), lower.modal_wake()) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };
    IdleCaseReport {
        name: case.name,
        expects_wake: case.expects_wake,
        rationale: case.rationale,
        default,
        lower,
        disagrees,
    }
}

/// Draw every case `draws` times on each arm: the client as supplied, and a clone of it with
/// only the effort changed. Both arms therefore share the model, system prompt, tools,
/// `tool_choice` and envelope — an effort comparison that cannot be explained by any of
/// them.
///
/// The default arm is whatever the caller built. If the session's inherited effort is not
/// recoverable (it is not logged), pass a client with no effort set and read the result as
/// "the provider default", or set the level the session was declared to run at.
///
/// `lower_effort: None` strips the base client's effort: a real arm only when the base
/// has one. With a base that has none the two arms are identical, and the run measures
/// nothing.
pub async fn compare_idle_efforts(
    agent: &Client,
    tools: &[Value],
    lower_effort: Option<&str>,
    draws: usize,
    cases: &[IdleCase],
) -> IdleComparison {
    let draws = draws.max(1);
    let lower = agent.clone().with_effort(lower_effort.map(str::to_string));
    let mut reports = Vec::with_capacity(cases.len());
    for case in cases {
        let mut default_draws = Vec::with_capacity(draws);
        let mut lower_draws = Vec::with_capacity(draws);
        for _ in 0..draws {
            default_draws.push(idle_attempt(agent, tools, case).await);
            lower_draws.push(idle_attempt(&lower, tools, case).await);
        }
        reports.push(case_report(case, default_draws, lower_draws));
    }
    IdleComparison {
        model: agent.model().to_string(),
        lower_effort: lower_effort.map(str::to_string),
        cases: reports,
    }
}

/// [`compare_idle_efforts`] over [`synthetic_cases`] — the whole comparison in one call.
pub async fn run_synthetic(
    agent: &Client,
    tools: &[Value],
    lower_effort: Option<&str>,
    draws: usize,
) -> IdleComparison {
    compare_idle_efforts(agent, tools, lower_effort, draws, &synthetic_cases()).await
}

impl IdleComparison {
    /// A terminal-readable report: per case, both arms, and the overall totals.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let draws = self.cases.first().map_or(0, |c| c.default.draws);
        let _ = writeln!(
            out,
            "idle triage effort comparison · {} cases × {draws} draws per arm",
            self.cases.len()
        );
        let _ = writeln!(
            out,
            "  model: {} · default arm: the client as supplied · lower arm: {}",
            self.model,
            match &self.lower_effort {
                Some(level) => format!("effort={level}"),
                None => "effort unset".to_string(),
            }
        );
        for case in &self.cases {
            let _ = writeln!(
                out,
                "\n{} — expects {}",
                case.name,
                if case.expects_wake { "wake" } else { "no wake" }
            );
            let _ = writeln!(out, "  {}", case.rationale);
            for (label, tally) in [("default", &case.default), ("lower", &case.lower)] {
                let _ = writeln!(out, "  {}", arm_line(label, tally));
                if tally.misses() > 0 {
                    let _ = writeln!(out, "          draws: {}", tally.decisions.join(" | "));
                    if let Some(first) = &tally.first_miss {
                        let _ = writeln!(out, "          first miss: {first}");
                    }
                }
            }
            if case.disagrees {
                let _ = writeln!(out, "          the modal decision differs between the arms");
            }
        }

        let mut default = IdleArmTally::default();
        let mut lower = IdleArmTally::default();
        for case in &self.cases {
            default.merge(&case.default);
            lower.merge(&case.lower);
        }
        let _ = writeln!(out, "\noverall");
        let _ = writeln!(out, "  {}", arm_line("default", &default));
        let _ = writeln!(out, "  {}", arm_line("lower", &lower));
        out
    }
}

fn arm_line(label: &str, tally: &IdleArmTally) -> String {
    format!(
        "{label:<7} {}/{} correct · {} false-wake · {} false-dismiss · {} no-verdict · \
         retries {} · {:.1}s avg · {} prompt / {} out / {} cache-read tok",
        tally.matches_label,
        tally.draws,
        tally.false_wakes,
        tally.false_dismisses,
        tally.no_verdicts,
        tally.retries,
        tally.average_seconds(),
        tally.prompt_tokens,
        tally.output_tokens,
        tally.cache_read_tokens,
    )
}

#[cfg(test)]
mod test;
