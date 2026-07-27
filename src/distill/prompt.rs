//! The instruction text and the rosters appended to a fork peek (docs/redaction-fork.md).

use super::*;

pub(super) const TRIAGE_INSTRUCTION: &str = "You're between tasks and have been woken to triage \
out-of-band events. Review the pending notifications (and any running background \
processes) above and call `triage`: `pin` what genuinely needs to enter your working \
context and be acted on now, `dismiss` the noise, and leave anything you're deferring \
unlisted. If you pin nothing you simply go back to idle and nothing changes — so pin only \
what truly warrants picking the work back up.";

pub(super) const RETAIN_INSTRUCTION: &str = "Above is the FULL, line-numbered output of the \
tool call you just made. Call `retain` to choose what stays on the record going \
forward: \"all\", \"none\", or comma-separated 1-based inclusive line ranges like \
\"40-52,310-318\". Whatever you drop is gone from the working context — so keep \
anything that might matter for the rest of the task, while staying as lean as you \
can. Always add a one-line `note` summarizing what the output showed.";

pub(super) const INBOX_INSTRUCTION: &str = " You also have pending notifications, listed under \
\"inbox\" above with an [id] each. In the `inbox` field, `pin` the ids worth carrying \
into your working context going forward and `dismiss` the noise; any id you don't list \
is left in the inbox and shown again at the next decision point.";

pub(super) const BACKGROUND_INSTRUCTION: &str = " Background processes are listed under \
\"background\" above, each with a [bg#n] handle and its recent output. In the \
`background` field, `pin` the handles whose current output you want captured onto the \
record now, and `mute` the ones whose tail you no longer need to see (they keep \
running and still notify you when they exit). Anything you don't list keeps streaming \
its latest tail to you at the next decision point.";

pub(super) const SURFACE_INSTRUCTION: &str = " Under \"context\" above are one-line-summarized \
files available to you but not yet loaded, each with a [name]. In the `surfaced` field, \
name any that genuinely bear on what you're doing right now — each you name will be \
surfaced as a reminder to read it before you rely on memory. Name nothing if none apply.";

/// Render the inbox + background sections of a peek view (docs/inbox.md, docs/supervisor.md) — shared by the
/// tool-result-riding gate and the standalone idle triage. Each item by its `[seq]` /
/// `[bg#n]` handle, with running-process tails indented.
pub(super) fn append_inbox_and_background(
    view: &mut String,
    inbox: &[ShownNotification],
    background: &[BgView],
) {
    if !inbox.is_empty() {
        view.push_str(&format!("\n\n═══ inbox · {} pending ═══", inbox.len()));
        for n in inbox {
            view.push_str(&format!("\n[{}] {} · {}", n.seq, n.source, n.body));
        }
    }
    if !background.is_empty() {
        view.push_str(&format!(
            "\n\n═══ background · {} running ═══",
            background.len()
        ));
        for b in background {
            view.push_str(&format!(
                "\n[{}] {} · running {}s · idle {}s",
                b.handle, b.command, b.running_secs, b.idle_secs
            ));
            for line in b.tail.lines() {
                view.push_str("\n    ");
                view.push_str(line);
            }
        }
    }
}

/// The fork's surfacing roster (docs/redaction-fork.md): the names of **listed**
/// `[context]` items — stub files carrying a `path`, read on demand — that no prior gate
/// has surfaced yet. A pure fold over the prefix: the boot `Preamble`'s structured listing
/// (last one wins on a multi-boot log) minus every name any prior `Triage.surfaced` already
/// carried (the surfaced-once dedup, `Triage`-only — no path-load tracking). Inlined and
/// exec items are excluded: their whole body is already on the wire, so there is nothing to
/// read on demand. Names double as the constrained output vocabulary.
pub(super) fn surfaceable_roster(prefix: &[LogEntry]) -> Vec<String> {
    let mut sections: &[crate::context::Section] = &[];
    let mut surfaced: HashSet<&str> = HashSet::new();
    for entry in prefix {
        match &entry.payload {
            Payload::Preamble { context, .. } => sections = &context.sections,
            Payload::Triage(t) => surfaced.extend(t.surfaced.iter().map(String::as_str)),
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    sections
        .iter()
        .flat_map(|s| &s.items)
        .filter_map(|item| match item {
            Item::Listed { name, .. } => Some(name),
            Item::Inline { .. } | Item::Exec { .. } => None,
        })
        .filter(|name| !surfaced.contains(name.as_str()) && seen.insert(name.as_str()))
        .cloned()
        .collect()
}

/// Render the surfacing roster section of a peek view (docs/redaction-fork.md): the
/// available-but-unloaded context items by `[name]`, the coordinates the `retain.surfaced`
/// field addresses. Only in the tool-result gate — the idle triage rides no work in flight.
pub(super) fn append_roster(view: &mut String, roster: &[String]) {
    if roster.is_empty() {
        return;
    }
    view.push_str(&format!("\n\n═══ context · {} available ═══", roster.len()));
    for name in roster {
        view.push_str(&format!("\n[{name}]"));
    }
}

/// Render every line `cat -n`-style (1-based, 6-wide, tab-separated). This is the
/// coordinate system the fork-peek gate (docs/redaction-fork.md) addresses with
/// `retain(lines=…)`, so it is shown ONLY in the throwaway fork — never on the
/// canonical wire, which uses the leaner `@@`-hunks in
/// [`render_distilled`](crate::project::render_distilled).
pub(super) fn number_lines(lines: &[&str]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}
