//! The instruction text and the rosters appended to a fork peek (docs/redaction-fork.md).

use super::*;

pub(super) const TRIAGE_INSTRUCTION: &str = "You're between tasks and have been woken to triage \
out-of-band events. Review the pending notifications (and any running background \
processes) above and call `triage` now: `pin` what genuinely needs to enter your working \
context and be acted on now, `dismiss` the noise, and leave anything you're deferring \
unlisted. If you pin nothing you simply go back to idle and nothing changes — so pin only \
what truly warrants picking the work back up.";

/// The ask's banner. Every other section of a peek view carries one (`═══ result ═══`,
/// `═══ context ═══`, `═══ inbox ═══`); the ask carrying none left the only *instruction*
/// in the view formatted as a trailing note about the sections above it.
pub(super) const ASK_BANNER: &str = "═══ triage · answer, don't act ═══";

/// The `keep`/`note` rows of the field block. `note` says *only what it showed* because a
/// peek once turned a raw `unknown tool: ferritin` into "ferritin is not available as a CLI
/// tool in this environment" — false, and the raw is discarded, so the invented claim was
/// the only thing that survived onto the canonical wire
/// (journal/2026-08-07-session-key-review.md). Dropping the wrong lines is the failure this
/// gate is designed against; asserting past the output is worse, because nothing
/// downstream can detect it.
// NB: each row's first line sits on the opening-quote line. A `"\` continuation would eat
// the newline *and* the leading indent behind it, silently de-indenting the first row only.
const KEEP_FIELDS: &str =
    "  keep        \"all\", \"none\", or comma-separated 1-based inclusive line ranges like
              \"40-52,310-318\". Whatever you drop is gone from the working context, so keep
              anything that might matter for the rest of the task, while staying as lean
              as you can.
  note        One line, saying what this output showed — only what it showed. Do not
              generalize past it or explain what it means for the task.";

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
///
/// A third thing, from a session that answered 29% of its gates with a work tool instead
/// of a verdict — usually the tool it went on to call on the next canonical turn, at a rate
/// that climbed with session depth (journal/2026-08-07-session-key-review.md). The ask had
/// no structural weight against that: one unbroken paragraph, assembled from four
/// separately-authored fragments, opening on a description and closing on a tangent about
/// `surfaced`, while every other section of the view carried a banner and `tools[]` carried
/// the whole world toolset under `tool_choice: auto`. So the ask now leads with the
/// imperative, states plainly that this turn is not the task, carries the per-field asks as
/// a scannable block instead of appended prose, and repeats the imperative last.
///
/// The "no other tool will run" sentence is a **fact**, not a discouragement: a
/// non-`retain` call is answered with an error `tool_result` and the ask is renewed
/// (`extend_with_retry`). Saying so non-optionally beats hinting, and stays true.
///
/// `riders` are the field rows for whatever else this gate carries (inbox, background,
/// surfacing roster), placed inside the block rather than appended after it.
pub(super) fn retain_instruction(offered: &[String], riders: &[&str]) -> String {
    let scope = "Nothing earlier in this conversation is being triaged — those results are \
already decided, and their [n] tags are not call ids.";
    let shape = match offered {
        [_one] => format!(
            "The block above is the FULL, line-numbered output of the one call you just made. \
{scope} Answer with a top-level `keep` and `note`: no `results` array and no call id, since \
there is only one result in question."
        ),
        many => format!(
            "The blocks above are the FULL, line-numbered output of the {n} calls you just made, \
each under a header naming its [call id]. Line numbers restart at 1 within each result, so ranges \
are always relative to the result you are deciding. {scope} Answer with exactly {n} `results` \
entries, one per call id listed here and no others: [{ids}]. Copy each `call_id` exactly.",
            n = many.len(),
            ids = many.join("], ["),
        ),
    };
    let fields = std::iter::once(KEEP_FIELDS)
        .chain(riders.iter().copied())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{ASK_BANNER}\n\n\
Call `retain` now. This is a triage turn, not a work turn: the task continues on your NEXT \
turn, from whatever you keep here. No other tool will run until you have called `retain` — a \
call to anything else is rejected and this ask comes back.\n\n\
{shape}\n\n\
{fields}\n\n\
Call `retain`."
    )
}

/// The gate-scoped riders, as field rows for [`retain_instruction`]'s block. Each is added
/// only when its section is actually present in the view above.
pub(super) const INBOX_FIELD: &str =
    "  inbox       The pending notifications under \"inbox\" above, an [id] each. `pin` the ids
              worth carrying into your working context going forward and `dismiss` the
              noise; any id you don't list stays in the inbox and is shown again at the
              next decision point.";

pub(super) const BACKGROUND_FIELD: &str =
    "  background  The processes under \"background\" above, a [p#n] handle each. `pin` the
              handles whose current output you want captured onto the record now, and
              `mute` the ones whose tail you no longer need (they keep running, and still
              notify you when they exit). Anything you don't list keeps streaming its
              latest tail to you at the next decision point.";

pub(super) const SURFACE_FIELD: &str =
    "  surfaced    The files under \"context\" above — available to you, not yet loaded, each
              shown with its summary and the path to read. Name any that genuinely bear on
              what you're doing right now and it will be surfaced as a reminder to read it
              before you rely on memory. Name nothing if none apply.";

/// Render the inbox + background sections of a peek view (docs/inbox.md, docs/supervisor.md) — shared by the
/// tool-result-riding gate and the standalone idle triage. Each item by its `[seq]` /
/// `[p#n]` handle, with running-process tails indented.
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

/// The fork's surfacing roster (docs/redaction-fork.md): the **listed** `[context]` items
/// — stub files carrying a `path`, read on demand — that no prior gate has surfaced yet.
/// A pure fold over the prefix: the boot `Preamble`'s structured listing (last one wins on
/// a multi-boot log) minus every name any prior `Triage.surfaced` already carried (the
/// surfaced-once dedup, `Triage`-only — no path-load tracking). Inlined and exec items are
/// excluded: their whole body is already on the wire, so there is nothing to read on
/// demand. Names double as the constrained output vocabulary.
///
/// The whole item is carried, not just the name, so [`append_roster`] can re-render it as
/// the preamble spelled it — summary and path included.
pub(super) fn surfaceable_roster(prefix: &[LogEntry]) -> Vec<Item> {
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
        .filter(|item| match item {
            Item::Listed { name, .. } => {
                !surfaced.contains(name.as_str()) && seen.insert(name.as_str())
            }
            Item::Inline { .. } | Item::Exec { .. } => false,
        })
        .cloned()
        .collect()
}

/// The name a roster [`Item`] is addressed by — the `retain.surfaced` vocabulary.
pub(super) fn roster_name(item: &Item) -> &str {
    match item {
        Item::Listed { name, .. } | Item::Inline { name, .. } | Item::Exec { name, .. } => name,
    }
}

/// Render the surfacing roster section of a peek view (docs/redaction-fork.md): the
/// available-but-unloaded context items, each spelled exactly as the boot preamble spelled
/// it — `<name path="…">summary</name>`, through the preamble's own renderer so the two
/// can't drift. Only in the tool-result gate — the idle triage rides no work in flight.
///
/// The name alone was what shipped first, and it under-describes what the item *is*: a
/// session met a listed skill as a bare `[ferritin]` and built a tool call out of the name
/// rather than reading the file (journal/2026-08-07-session-key-review.md). Repeating the
/// summary and the path costs a line each and answers the question the name provokes.
pub(super) fn append_roster(view: &mut String, roster: &[Item]) {
    if roster.is_empty() {
        return;
    }
    view.push_str(&format!("\n\n═══ context · {} available ═══", roster.len()));
    for item in roster {
        view.push('\n');
        view.push_str(&crate::context::render_item(item));
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
