//! `efference dev spotter-log` — read a session log and report the spotter's verdicts (docs/permissions.md).

use super::*;
use crate::dev::StanceArg;

/// The uppercase display label and colour for a stance in the survey listing.
pub(crate) fn stance_label(stance: efference::log::Stance) -> (&'static str, &'static str) {
    use efference::log::Stance;
    match stance {
        Stance::Clear => ("CLEAR", GREEN),
        Stance::ClearAlways => ("CLEAR_ALWAYS", GREEN_BOLD),
        Stance::Watch => ("WATCH", YELLOW),
        Stance::Gate => ("GATE", RED),
    }
}

/// A compact one-line description of the tool call a verdict judged, for the survey listing.
/// Prefers the human-obvious field per tool shape — a bash `command`, else a `path`/`file_path`,
/// else a `pattern`/`query` — falling back to compact JSON of the whole input.
pub(crate) fn describe_call(name: &str, input: &serde_json::Value) -> String {
    let field = |k: &str| input.get(k).and_then(|v| v.as_str());
    // A `paths` array (the structured read/list tools) → its string elements, comma-joined.
    let paths = input.get("paths").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    });
    if let Some(cmd) = field("command") {
        format!("{name}: {}", one_line(cmd))
    } else if let Some(p) = field("path").or_else(|| field("file_path")) {
        format!("{name}: {p}")
    } else if let Some(p) = paths.filter(|s| !s.is_empty()) {
        format!("{name}: {p}")
    } else if let Some(q) = field("pattern").or_else(|| field("query")) {
        format!("{name}: {q}")
    } else {
        format!("{name}: {}", one_line(&input.to_string()))
    }
}

/// Collapse whitespace/newlines to single spaces and cap length so a call reads as one tidy cell.
pub(crate) fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 120;
    if flat.chars().count() > MAX {
        let head: String = flat.chars().take(MAX - 1).collect();
        format!("{head}…")
    } else {
        flat
    }
}

/// One surveyed spotter verdict, flattened for the `spotter-log` listing.
pub(crate) struct SpotRow {
    pub(crate) session: String,
    pub(crate) seq: u64,
    pub(crate) turn: u32,
    pub(crate) stance: efference::log::Stance,
    pub(crate) origin: efference::log::Origin,
    pub(crate) reason: String,
    pub(crate) probed_prefix: Option<String>,
    pub(crate) call: String,
}

/// Extract the spotter verdicts from one session's loaded log, in append order, correlating each
/// `Payload::Spot` to the tool call it judged (by `call_id`) and applying the `--stance` /
/// `--no-fallback` filters. The pure, testable core of [`spotter_log`].
pub(crate) fn spots_in(
    session: &str,
    entries: &[efference::log::LogEntry],
    stances: &[StanceArg],
    no_fallback: bool,
) -> Vec<SpotRow> {
    use efference::log::{CallId, Origin, Payload, ToolCall};
    use std::collections::HashMap;

    // Index every tool call by id so each Spot can name what it judged.
    let mut calls: HashMap<CallId, ToolCall> = HashMap::new();
    for entry in entries {
        if let Payload::Assistant { output, .. } = &entry.payload {
            for call in output.tool_calls().unwrap_or_default() {
                calls.insert(call.id.clone(), call);
            }
        }
    }

    let mut rows = Vec::new();
    for entry in entries {
        let Payload::Spot(spot) = &entry.payload else {
            continue;
        };
        if no_fallback && spot.origin == Origin::Fallback {
            continue;
        }
        if !stances.is_empty() && !stances.iter().any(|s| s.matches(spot.stance)) {
            continue;
        }
        let call = calls.get(&spot.call_id).map_or_else(
            || format!("<unresolved call {}>", spot.call_id.0),
            |c| describe_call(&c.name, &c.input),
        );
        rows.push(SpotRow {
            session: session.to_string(),
            seq: entry.seq,
            turn: entry.turn,
            stance: spot.stance,
            origin: spot.origin,
            reason: spot.reason.clone(),
            probed_prefix: spot.probed_prefix.clone(),
            call,
        });
    }
    rows
}

/// Resolve which session logs the survey reads. An explicit path is either a `log.jsonl` file
/// (used directly) or a session dir (→ its `LOG_NAME`); omitting it scans every
/// `.efference/*/log.jsonl` under the cwd. Returns `(session id, log path)` pairs, sorted by id.
pub(crate) fn resolve_spotter_logs(
    path: Option<PathBuf>,
) -> std::io::Result<Vec<(String, PathBuf)>> {
    if let Some(p) = path {
        let (id, log) = if p.is_dir() {
            let id = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("session")
                .to_string();
            (id, p.join(LOG_NAME))
        } else {
            // A log file directly — name it by its parent dir (the session id) when we can.
            let id = p
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("session")
                .to_string();
            (id, p)
        };
        return Ok(vec![(id, log)]);
    }
    let mut out = Vec::new();
    let base = Path::new(BASE_DIR);
    if !base.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let log = entry.path().join(LOG_NAME);
        if log.is_file() {
            out.push((entry.file_name().to_string_lossy().into_owned(), log));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Survey the recorded spotter verdicts across sessions (docs/permissions.md): read every
/// `Payload::Spot` back out of the session logs, correlate each to the tool call it judged, and
/// print a stance-distribution summary and/or the per-verdict listing. Offline — no daemon, no
/// network, config-independent. The read side of the `spotter` dev tool.
pub(crate) fn spotter_log(
    path: Option<PathBuf>,
    stances: &[StanceArg],
    summary_only: bool,
    list_only: bool,
    no_fallback: bool,
) -> CliResult {
    use efference::log::{Origin, Stance};
    use std::io::IsTerminal;

    let logs = resolve_spotter_logs(path)?;
    if logs.is_empty() {
        eprintln!("no session logs found (looked under {BASE_DIR}/*/{LOG_NAME}).");
        return Ok(());
    }
    let tty = std::io::stdout().is_terminal();

    let mut rows: Vec<SpotRow> = Vec::new();
    let mut sessions_with_spots = 0usize;
    for (session, log_path) in &logs {
        let entries = match efference::persist::load(log_path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skipping {}: {e}", log_path.display());
                continue;
            }
        };
        let found = spots_in(session, &entries, stances, no_fallback);
        if !found.is_empty() {
            sessions_with_spots += 1;
        }
        rows.extend(found);
    }

    if rows.is_empty() {
        let suffix = if stances.is_empty() && !no_fallback {
            ""
        } else {
            " matching the filter"
        };
        println!("no spotter verdicts{suffix} across {} log(s).", logs.len());
        return Ok(());
    }

    // ---- summary: stance distribution + fail-closed rate ----
    if !list_only {
        let total = rows.len();
        let count = |s: Stance| rows.iter().filter(|r| r.stance == s).count();
        let fallback = rows.iter().filter(|r| r.origin == Origin::Fallback).count();
        let pct = |n: usize| n as f64 * 100.0 / total as f64;

        println!("{}", paint("SPOTTER SURVEY", BOLD, tty));
        println!(
            "  {} log(s) scanned · {sessions_with_spots} with verdicts · {total} verdict(s)",
            logs.len()
        );
        let line = |label: &str, n: usize, code: &str| {
            if n > 0 {
                let lbl = paint(&format!("{label:<12}"), code, tty);
                println!("  {lbl} {n:>4}  ({:>3.0}%)", pct(n));
            }
        };
        line("clear", count(Stance::Clear), GREEN);
        line("clear_always", count(Stance::ClearAlways), GREEN_BOLD);
        line("watch", count(Stance::Watch), YELLOW);
        line("gate", count(Stance::Gate), RED);
        if fallback > 0 {
            let lbl = paint(&format!("{:<12}", "fallback"), DIM, tty);
            println!(
                "  {lbl} {fallback:>4}  ({:>3.0}%)  fail-closed",
                pct(fallback)
            );
        }
    }

    // ---- listing: each verdict, grouped by session ----
    if !summary_only {
        if !list_only {
            println!();
        }
        let mut current = "";
        for r in &rows {
            if r.session.as_str() != current {
                current = r.session.as_str();
                let hdr = format!("── {} ", r.session);
                println!("{}", paint(&format!("{hdr:─<64}"), DIM, tty));
            }
            let (label, code) = stance_label(r.stance);
            let mut tags = String::new();
            if r.origin == Origin::Fallback {
                tags.push_str(" · fallback");
            }
            if let Some(p) = &r.probed_prefix {
                tags.push_str(&format!(" · probe `{p}`"));
            }
            println!(
                "  seq {} · turn {} · {}{tags}",
                r.seq,
                r.turn,
                paint(label, code, tty)
            );
            println!("    {}", r.call);
            println!("    {}", paint(&format!("“{}”", r.reason.trim()), DIM, tty));
        }
    }
    Ok(())
}
