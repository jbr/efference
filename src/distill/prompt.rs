//! The instruction text and the rosters appended to a fork peek (docs/redaction-fork.md).

use super::*;

pub(super) const TRIAGE_INSTRUCTION: &str = "You're between tasks and have been woken to triage \
out-of-band events. Review the pending notifications (and any running background \
processes) above and call `triage` now: `pin` what genuinely needs to enter your working \
context and be acted on now, `dismiss` the noise, and leave anything you're deferring \
unlisted. If you pin nothing you simply go back to idle and nothing changes — so pin only \
what truly warrants picking the work back up.";

/// The shared tail of the `retain` ask — what `keep` means and how to choose it. The
/// shape-specific opening is prepended by [`retain_instruction`].
const RETAIN_TAIL: &str = " `keep` is \"all\", \"none\", or comma-separated 1-based inclusive \
line ranges like \"40-52,310-318\". Whatever you drop is gone from the working context — so keep \
anything that might matter for the rest of the task, while staying as lean as you can. Always \
add a one-line `note` summarizing what that output showed.";

/// The `retain` ask, scoped to the results **this gate** is deciding
/// (docs/redaction-fork.md).
///
/// Two things the earlier static text got wrong, both observed against a live model on a
/// long transcript:
///
/// - **The ask must be bounded.** "Every [call id] above" reads as the whole
///   conversation, and every prior `tool_result` on the canonical wire already carries a
///   `[seq]` block-addressing tag in the same bracket syntax as a `[call id]` header. A
///   model deep in a session answered by re-triaging the entire transcript — twenty-odd
///   entries keyed by `[seq]` tags, its own dialect's internal tool names, and invented
///   ids — and the id actually under the gate was often not among them, so coverage
///   failed and the gate hard-errored. Saying what is *not* in scope is what stops it.
/// - **A single-result gate should not ask for a call id at all.** Copying an id when
///   only one result is in question is transcription with no decision in it, and it is
///   the single most failure-prone token in the response. The flat top-level
///   `keep`/`note` is asked for instead; the `results` array is for gates that genuinely
///   carry several. The ids are interpolated in that case — the peek's tail is thrown
///   away, so nothing here is cache-keyed (docs/log-and-projection.md), unlike the
///   static schema it drives.
pub(super) fn retain_instruction(offered: &[String]) -> String {
    let scope = "Nothing earlier in this conversation is being triaged — those results are \
already decided, and their [n] tags are not call ids.";
    match offered {
        [_one] => format!(
            "This message carries the FULL, line-numbered output of the one tool call you just \
made. {scope} Call `retain` now with a top-level `keep` and `note` for that output — no \
`results` array and no call id: there is only one result in question.{RETAIN_TAIL}"
        ),
        many => format!(
            "This message carries the FULL, line-numbered output of the {n} tool calls you just \
made, each under a header naming its [call id]. Line numbers restart at 1 within each result, so \
ranges are always relative to the result you are deciding. {scope} Call `retain` now with exactly \
{n} `results` entries, one per call id listed here and no others: [{ids}]. Copy each `call_id` \
exactly.{RETAIN_TAIL}",
            n = many.len(),
            ids = many.join("], ["),
        ),
    }
}

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

/// One result's block in a peek view (docs/redaction-fork.md): a header naming the
/// `[call id]` and tool, then the capped, line-numbered output.
///
/// Numbering restarts at 1 in every block, and the header is what the `retain`
/// verdict's `call_id` addresses. A gate can carry several results, so a single
/// continuous coordinate system would invite the model to keep lines from one result
/// under another's verdict — a silent mis-attribution, since a wrong verdict renders
/// exactly like a right one. Per-block numbering makes the coordinate systems visibly
/// separate. The cap matches `render_distilled`'s, so the verdict's coordinates stay
/// aligned with what the model saw.
pub(super) fn result_view(call: &ToolCall, raw: &RawResult) -> String {
    let all: Vec<&str> = raw.content.lines().collect();
    let lines = &all[..all.len().min(PEEK_LINE_CAP)];
    let dropped = all.len() - lines.len();
    let mut view = format!("═══ result · [{}] {} ═══\n", call.id.0, call.name);
    view.push_str(&number_lines(lines));
    if dropped > 0 {
        view.push_str(&format!("\n       … [{dropped} more lines truncated] …"));
    }
    view
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
