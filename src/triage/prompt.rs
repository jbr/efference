//! The instruction text and the rosters appended to a triage (docs/triage.md).

use super::*;

/// What each disposition of a notification does, in the words both asks use: the idle
/// [`TRIAGE_INSTRUCTION`] and the per-result [`INBOX_FIELD`] row (docs/inbox.md, "Triage:
/// pin · dismiss · leave"). A macro because `concat!` takes only literals.
macro_rules! dispositions {
    () => {
        "`pin` — it enters your working context and you resume work on it now. `leave` \
(list nothing for it) — it stays pending and is shown again at the next triage, \
no turn runs, and the harness may dismiss it if it keeps going unmentioned. `dismiss` — it \
is cleared for good, you will not see it again, and no turn runs."
    };
}

/// [`dispositions!`] in the third person, for the text view a separate triage model reads
/// (docs/triage.md): the same three choices, said of the agent.
macro_rules! dispositions_third_person {
    () => {
        "`pin` — it enters the agent's working context and the agent resumes work on it now. \
`leave` (list nothing for it) — it stays pending and is shown again at the next triage, \
no turn runs, and the harness may dismiss it if it keeps going unmentioned. `dismiss` — it \
is cleared for good, the agent will not see it again, and no turn runs."
    };
}

/// Whom a triage request's wording addresses (docs/triage.md). The agent triaging its
/// own results reads its own transcript and is asked about "the call you just made"; a
/// separate triage model reads the text view, a record of the agent's work, and is asked
/// about the agent's calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Framing {
    FirstPerson,
    ThirdPerson,
}

// After the disposition macros, which the inbox rows expand.
mod fields;
pub(super) use fields::*;

pub(super) const TRIAGE_INSTRUCTION: &str = concat!(
    "You're between tasks and have been woken to triage out-of-band events. Review the \
pending notifications (and any running background processes) above and call `triage` now. \
For each notification: ",
    dispositions!(),
    " Nothing else will wake you: if a notification is something you said you would act \
on — a process you started, a timer you set, a reply you are waiting for — leaving or \
dismissing it means you wait until someone else sends a turn."
);

/// The ask's banner. Every other section of a triage view carries one (`═══ result ═══`,
/// `═══ context ═══`, `═══ inbox ═══`); the ask carrying none left the only *instruction*
/// in the view formatted as a trailing note about the sections above it. It opens on the
/// harness's `⟢` because the preamble names a `⟢ triage` block as the harness's one ask
/// (assets/prompts/session.md); without that, the ask arrives in an unprefixed user turn
/// the preamble attributes to the operator (docs/triage.md).
pub(super) const ASK_BANNER: &str = "⟢ triage · the harness asks: answer, don't act";

/// The header over the riders (inbox, background, roster), which follow the ask. It says
/// they are not the output of the call: the ask refers to the result blocks above it, and a
/// rider read as that output gets a note written about it (docs/triage.md).
pub(super) const RIDERS_HEADER: &str = "═══ also pending · not the output of the call above ═══\n\
The sections below are separate from the result blocks. Answer them in the matching fields \
described above; the result's `keep` and `note` are about the result alone.";

/// The `retain` ask, scoped to the results **this triage** is deciding
/// (docs/triage.md).
///
/// Two properties the text must hold, each answering a live failure on a long transcript:
///
/// - **The ask must be bounded.** "Every [call id] above" reads as the whole
///   conversation, and every prior `tool_result` on the canonical wire already carries a
///   `[seq]` block-addressing tag in the same bracket syntax the result headers once used
///   for call ids. A
///   model deep in a session answered by re-triaging the entire transcript — twenty-odd
///   entries keyed by `[seq]` tags, its own dialect's internal tool names, and invented
///   ids — and the id actually under the triage was often not among them, so coverage
///   failed and the triage hard-errored. Saying what is *not* in scope is what stops it.
/// - **A single-result triage should not ask for a label at all.** Naming the result when
///   only one is in question is transcription with no decision in it. The flat top-level
///   `keep`/`note` is asked for instead; the `results` array is for triages that genuinely
///   carry several, keyed by the short labels of [`labels`] rather than
///   provider call ids, which models copied back with characters dropped. The labels
///   are interpolated in that case — the triage's tail is thrown
///   away, so nothing here is cache-keyed (docs/log-and-projection.md), unlike the
///   static schema it drives.
///
/// A third property: the ask carries **structural weight** against being answered with a
/// work tool. `tools[]` offers the whole world toolset under `tool_choice: auto`, and every
/// other section of the view carries a banner, so an ask that read as an unbannered
/// trailing paragraph drew work calls instead of verdicts at a rate that climbed with
/// session depth (journal/2026-08-07-session-key-review.md). So it leads with the
/// imperative, states plainly that this turn is not the task, carries the per-field asks as
/// a scannable block rather than appended prose, and repeats the imperative last.
///
/// The "no other tool will run" sentence is a **fact**, not a discouragement: a
/// non-`retain` call is answered with an error `tool_result` and the ask is renewed
/// (`extend_with_retry`). Saying so non-optionally beats hinting, and stays true.
///
/// `riders` are the field rows for whatever else this triage carries (inbox, background,
/// surfacing roster), placed inside the block rather than appended after it.
/// `reader` says who answers: the agent's own model (`Reader::Main`, the shared triage) is
/// asked for a note to self ([`KEEP_FIELDS`]); a separate triage model
/// (`Reader::Triage`) for a report ([`REPORT_FIELDS`]).
///
/// `framing` says whom the wording addresses: the shared triage's own transcript is
/// first person, the text view third person ([`Framing`]). The text view carries no
/// `[n]` tags, so its scope sentence does not mention them.
pub(super) fn retain_instruction(
    offered: &[String],
    riders: &[&str],
    reader: Reader,
    framing: Framing,
) -> String {
    let v = AskVariant::from_env();
    let (who, scope) = match framing {
        Framing::FirstPerson => (
            "you",
            "Nothing earlier in this conversation is being triaged — those results are \
already decided, and their [n] tags are not result labels.",
        ),
        Framing::ThirdPerson => (
            "the agent",
            "Nothing earlier in the record is being triaged — those results are already decided.",
        ),
    };
    let shape = match offered {
        [_one] => format!(
            "The block headed `═══ result` above is the FULL, line-numbered output of the one call {who} \
just made. {scope} Answer with a top-level `keep` and `note`: no `results` array and no label, since \
there is only one result in question."
        ),
        many => format!(
            "The blocks headed `═══ result` above are the FULL, line-numbered output of the {n} calls \
{who} just made, each under a header naming its label. Line numbers restart at 1 within each result, so ranges \
are always relative to the result you are deciding. {scope} Answer with exactly {n} `results` \
entries, one per label listed here and no others: {labels}.",
            n = many.len(),
            labels = many.join(", "),
        ),
    };
    // An experiment's replacement for the `keep`/`note` rows (backlog/inbox/triage-ground-truth),
    // read from a file so a wording mutation is a text edit; unset, the shipped rows.
    let keep_fields = match std::env::var("EFFERENCE_TRIAGE_FIELDS") {
        Ok(path) => std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("EFFERENCE_TRIAGE_FIELDS: {path}: {e}"))
            .trim_end()
            .to_string(),
        Err(_) => match reader {
            Reader::Main => KEEP_FIELDS.to_string(),
            Reader::Triage => REPORT_FIELDS.to_string(),
        },
    };
    let fields: Vec<&str> = std::iter::once(keep_fields.as_str())
        .chain(riders.iter().copied())
        .collect();
    v.assemble(&shape, &fields, framing)
}

/// Render the inbox + background sections of a triage view (docs/inbox.md, docs/process-monitor.md) — shared by the
/// tool-result-riding triage and the standalone idle triage. Each item by its `[seq]` /
/// `[p#n]` handle, with running-process tails indented.
pub(super) fn append_inbox_and_background(
    view: &mut String,
    inbox: &[ShownNotification],
    background: &[BgView],
) {
    if !inbox.is_empty() {
        view.push_str(&format!("\n\n═══ inbox · {} pending ═══", inbox.len()));
        for n in inbox {
            let source = crate::names::sources::wire_label(&n.source);
            view.push_str(&format!("\n[{}] {source} · {}", n.seq, n.body));
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

/// The fork's surfacing roster (docs/triage.md): the **listed** `[context]` items
/// — stub files carrying a `path`, read on demand — that no prior triage has surfaced yet.
/// A pure fold over the prefix: the boot `Preamble`'s structured listing (last one wins on
/// a multi-boot log) minus every name any prior `Triage.surfaced` already carried (the
/// surfaced-once dedup, `Triage`-only — no path-load tracking). Inlined and exec items are
/// excluded: their whole body is already on the wire, so there is nothing to read on
/// demand. Names double as the constrained output vocabulary.
///
/// The whole item is carried, not just the name, so [`append_roster`] can re-render it as
/// the preamble spelled it — summary and path included.
pub(super) fn surfaceable_roster(prefix: &[LogEntry]) -> Vec<Item> {
    let mut sections: &[crate::log::context::Section] = &[];
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

/// Whether this triage actually shows a roster section — and so whether the ask should offer
/// a `surfaced` field at all. Not the same as "the roster is non-empty": a style that
/// renders nothing must also drop the field, or the ask names a section that isn't there.
pub(super) fn roster_shown(roster: &[Item]) -> bool {
    !roster.is_empty() && AskVariant::from_env().roster != RosterStyle::Off
}

/// Render the surfacing roster section of a triage view (docs/triage.md): the
/// available-but-unloaded context items, each spelled exactly as the boot preamble spelled
/// it, through the preamble's own renderer so the two can't drift. Only in the tool-result
/// triage — the idle triage rides no work in flight.
///
/// The path rides alongside the name because a name alone under-describes what the item
/// *is*: a session met a listed skill as a bare `[ferritin]` and built a tool call out of
/// the name rather than reading the file (journal/2026-08-07-session-key-review.md).
pub(super) fn append_roster(view: &mut String, roster: &[Item]) {
    if !roster_shown(roster) {
        return;
    }
    let style = AskVariant::from_env().roster;
    view.push_str(&format!("\n\n═══ context · {} available ═══", roster.len()));
    for item in roster {
        view.push('\n');
        view.push_str(&style.render(item));
    }
}

/// The sentence a result's header carries about its numbering (docs/triage.md, "Line
/// numbers are the coordinate system"). Much output has line numbers of its own (`read`,
/// `grep -n`, `cat -n`), so the view's are set apart by a margin glyph no common tool
/// emits, and the header says which numbers `keep` uses.
const HEADER_MARGIN: &str = "Each line starts with its line number and a │ margin (`12 │ text`); \
numbers after the margin are part of the text, and `keep` uses the margin's numbers.";

/// Where a truncation marker's `…` sits: under the margin's `│`.
const MARGIN_INDENT: &str = "     ";

/// The lines as the triage view shows them: `{:>4} │ text`, numbered from 1.
fn margin_lines(lines: &[&str]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4} │ {line}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One result's block in a triage view (docs/triage.md): a header naming the
/// result's label ([`labels`]) and tool, then the capped, line-numbered
/// output.
///
/// Numbering restarts at 1 in every block, and the header is what the `retain`
/// verdict's `result` addresses. A triage can carry several results, so a single
/// continuous coordinate system would invite the model to keep lines from one result
/// under another's verdict — a silent mis-attribution, since a wrong verdict renders
/// exactly like a right one. Per-block numbering makes the coordinate systems visibly
/// separate. The cap matches `render_triaged`'s, so the verdict's coordinates stay
/// aligned with what the model saw.
pub(super) fn result_view(label: &str, call: &ToolCall, raw: &RawResult) -> String {
    let all: Vec<&str> = raw.content.lines().collect();
    let lines = &all[..all.len().min(TRIAGE_LINE_CAP)];
    let dropped = all.len() - lines.len();
    let mut view = format!("═══ result {label} · {} ═══\n", call.name);
    // The call's `intent` (docs/tools.md), so the verdict's `note` answers the
    // question the call was asking rather than restating what the tool did. The two
    // are written at different times by different requests, which is what lets them
    // disagree — a `note` that does not answer its `intent` is the signal that the
    // call went sideways, and it is unavailable from either half alone.
    //
    // Showing it here creates no incentive to write intents for this reader: triage
    // runs after the call, so nothing the intent says can change whether it ran.
    if let Some(intent) = call.intent() {
        view.push_str(&format!("intent: {intent}\n"));
    }
    // The images are not in this request; the agent receives them whatever the verdict.
    if !raw.images.is_empty() {
        let n = raw.images.len();
        let noun = if n == 1 { "image" } else { "images" };
        view.push_str(&format!(
            "images: {n} {noun} — not shown here; reaches the agent as it is\n"
        ));
    }
    view.push_str(HEADER_MARGIN);
    view.push('\n');
    view.push_str(&margin_lines(lines));
    if dropped > 0 {
        view.push_str(&format!(
            "\n{MARGIN_INDENT}… [{dropped} more lines truncated] …"
        ));
    }
    view
}

/// A bypassed result's block in the triage view (docs/triage.md). The call must
/// still be answered — the API rejects a request that leaves a `tool_use` unpaired — but
/// the output itself is not shown: it is going to the wire verbatim, so showing it would
/// spend the fork's uncached tail on text no verdict can change, and offering it beside
/// the results that *are* in question invites a verdict on it.
pub(super) fn bypassed_view(call: &ToolCall, shape: &str, framing: Framing) -> String {
    if is_reply(call) {
        return format!(
            "═══ result · {} ═══\nnot triaged — this is the operator's reply to a question the \
             agent asked, and it goes to the agent unchanged. It is not one of the results you \
             are deciding about below.",
            call.name
        );
    }
    let to = match framing {
        Framing::FirstPerson => "you",
        Framing::ThirdPerson => "the agent",
    };
    format!(
        "═══ result · {} ═══\nnot triaged — this result carries a {shape}, a value that \
         only works reproduced exactly, so its full output goes to {to} unchanged. It is not \
         one of the results you are deciding about below.",
        call.name
    )
}

/// Build the triage's request (docs/triage.md): the cached
/// prefix-through-`tool_use`, then one user message carrying every result plus the
/// triage-scoped riders and the retain prompt.
///
/// **The invariant this exists to hold:** the trailing assistant message carries one
/// `tool_use` per call the completion made, and the API rejects a request that leaves
/// any of them unanswered. So the tail emits one `tool_result` per [`Pending`] — which
/// is only correct because the loop runs *every* call of a completion before opening the
/// triage. Triaging between calls would leave the later ones orphaned, which is exactly the
/// failure this replaced.
///
/// The riders (inbox, background, surfacing roster) are triage-scoped, so they ride the
/// trailing text block rather than any one result. Pure and separate from the round-trip
/// so the pairing invariant is testable without a live client.
pub(super) fn triage_messages(
    prefix: &[LogEntry],
    reader: Reader,
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
) -> Vec<WireTurn> {
    let (results, ask) = triage_parts(
        pending,
        inbox,
        background,
        roster,
        reader,
        Framing::FirstPerson,
    );
    // The prefix hits cache; only the raws and the instruction are fresh, and they are
    // discarded — the canonical lineage never sees raw (docs/triage.md). The
    // frontier sits before that raw tail: the shared prefix is read, the throwaway
    // suffix is never written.
    let mut messages = project_semantic_for(prefix, reader);
    messages.push(WireTurn::Breakpoint);
    // Every call is answered — a call left unpaired makes the request invalid.
    let mut content: Vec<UserPart> = results
        .into_iter()
        .map(|r| UserPart::ToolResult {
            call_id: r.call_id,
            seq: None,
            content: r.content,
            is_error: r.is_error,
            images: Vec::new(),
        })
        .collect();
    content.push(UserPart::Text(ask));
    messages.push(WireTurn::User(content));
    messages
}

/// One result as the triage request shows it: labelled and line-numbered, or a stand-in
/// for a bypassed result.
pub(super) struct TriageResult {
    pub(super) call_id: crate::log::CallId,
    pub(super) content: String,
    pub(super) is_error: bool,
}

/// The triage-scoped half of a triage request, shared by every view of the history: each
/// result as triage reads it, and the trailing text carrying the riders (inbox,
/// background, surfacing roster) and the retain instruction.
pub(super) fn triage_parts(
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
    reader: Reader,
    framing: Framing,
) -> (Vec<TriageResult>, String) {
    let mut sections = String::new();
    append_inbox_and_background(&mut sections, inbox, background);
    append_roster(&mut sections, roster);

    // Scoped to exactly this triage's ids, and shape-selected on its size — the tail is
    // discarded with the fork, so nothing here is cache-keyed (docs/triage.md).
    // A rider's field row is offered only when its section is actually shown.
    let labels = labels(pending);
    let offered: Vec<String> = labels.iter().flatten().cloned().collect();
    let mut riders: Vec<&str> = Vec::new();
    if pending
        .iter()
        .any(|p| bypassed(p).is_none() && !p.raw.images.is_empty())
    {
        riders.push(IMAGES_FIELD);
    }
    let (inbox_field, background_field, surface_field) = match framing {
        Framing::FirstPerson => (INBOX_FIELD, BACKGROUND_FIELD, SURFACE_FIELD),
        Framing::ThirdPerson => (
            INBOX_FIELD_THIRD_PERSON,
            BACKGROUND_FIELD_THIRD_PERSON,
            SURFACE_FIELD_THIRD_PERSON,
        ),
    };
    if !inbox.is_empty() {
        riders.push(inbox_field);
    }
    if !background.is_empty() {
        riders.push(background_field);
    }
    if roster_shown(roster) {
        riders.push(surface_field);
    }
    let instruction = retain_instruction(&offered, &riders, reader, framing);

    // A bypassed result answers with a stand-in rather than its output: the harness
    // already decided it (docs/triage.md), and the ask does not offer it.
    let results = pending
        .iter()
        .zip(&labels)
        .map(|(p, label)| TriageResult {
            call_id: p.call.id.clone(),
            content: match (bypassed(p), label) {
                (Some(shape), _) => bypassed_view(p.call, shape, framing),
                (None, Some(label)) => result_view(label, p.call, p.raw),
                (None, None) => unreachable!("labels() labels every unbypassed result"),
            },
            is_error: p.raw.is_error,
        })
        .collect();
    // The riders follow the ask under their own header, so the block directly after the
    // result blocks is the ask that names them. `sections` opens with its own blank line
    // when non-empty and is empty outright on a triage carrying no riders.
    let ask = match sections.trim() {
        "" => instruction,
        sections => format!("{instruction}\n\n{RIDERS_HEADER}\n\n{sections}"),
    };
    (results, ask)
}
