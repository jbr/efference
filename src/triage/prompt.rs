//! The instruction text and the rosters appended to a triage (docs/triage.md).

use super::*;

pub(super) const TRIAGE_INSTRUCTION: &str = "You're between tasks and have been woken to triage \
out-of-band events. Review the pending notifications (and any running background \
processes) above and call `triage` now: `pin` what genuinely needs to enter your working \
context and be acted on now, `dismiss` the noise, and leave anything you're deferring \
unlisted. If you pin nothing you simply go back to idle and nothing changes — so pin only \
what truly warrants picking the work back up.";

/// The ask's banner. Every other section of a triage view carries one (`═══ result ═══`,
/// `═══ context ═══`, `═══ inbox ═══`); the ask carrying none left the only *instruction*
/// in the view formatted as a trailing note about the sections above it. It opens on the
/// harness's `⟢` because the preamble names a `⟢ triage` block as the harness's one ask
/// (assets/prompts/session.md); without that, the ask arrives in an unprefixed user turn
/// the preamble attributes to the operator (docs/triage.md).
pub(super) const ASK_BANNER: &str = "⟢ triage · the harness asks: answer, don't act";

/// The `keep`/`note` rows of the field block. `note` says *only what it showed* because a
/// triage once turned a raw `unknown tool: ferritin` into "ferritin is not available as a CLI
/// tool in this environment" — false, and the raw is discarded, so the invented claim was
/// the only thing that survived onto the canonical wire
/// (journal/2026-08-07-session-key-review.md). Dropping the wrong lines is the failure this
/// triage is designed against; asserting past the output is worse, because nothing
/// downstream can detect it.
///
/// Two clauses answer a second shape of the same failure, where the note is true of the
/// *raw* and unsupported by the *kept lines* — both halves reach the wire disagreeing, and
/// a reader has to re-verify by hand before using either. So `note` is asked to be grounded
/// in what survives rather than in what was shown, and `keep` is told that a partial keep
/// of an enumeration reads afterwards as the whole set — the case where a lean verdict
/// silently changes the answer rather than shortening it.
// NB: each row's first line sits on the opening-quote line. A `"\` continuation would eat
// the newline *and* the leading indent behind it, silently de-indenting the first row only.
const KEEP_FIELDS: &str =
    "  keep        \"all\", \"none\", or comma-separated 1-based inclusive line ranges like
              \"40-52,310-318\". Whatever you drop is gone from the working context, so keep
              anything that might matter for the rest of the task, while staying as lean
              as you can. Where the output enumerates — matches, a listing, a set of
              candidates — the lines you keep read afterwards as the whole set, so keep
              the enumeration whole or say in the `note` how many of how many survived.
  note        One line, saying what this output showed — only what it showed. It is a
              note to self: it is read on the next turn, in the working context, in
              place of the lines that were dropped. Do not
              generalize past it or explain what it means for the task. Ground it in
              the lines you kept, not the ones you dropped: if the note turns on a
              particular line, that line belongs in `keep`. Where the
              result header carries an `intent`, answer it from what is shown. If the
              output does not contain what the intent asked for, say that rather than
              summarizing what it does contain; \"it didn't\" is often the answer
              worth keeping.
  intent_feedback
              Omit almost always. Use only when the `intent` gave you nothing to
              decide against — it restated the arguments, or named no question the
              output could answer — and say so in one clause, so the next intent is
              one you can act on. This is about the intent, never about the task: do
              not suggest what to call next or what the output means.";

/// The `keep`/`note` rows a **separate triage model** is asked (docs/triage.md):
/// the note is a report for the agent, not a note to self. A different model chiming in
/// with what to do next is noise at best and steering at worst, so it reports what the
/// output shows — what was dropped and whether that is safe, the answers to the intent's
/// questions, what the output lacks — and never what to do about it. What the intent asks
/// to *see* is kept; what it asks as a *question* is answered in the note, keeping the one
/// line that proves the answer; lines that only witness something (a run of passing
/// tests) give way to the fact, and a line the agent may copy, search for, or edit from
/// never does (backlog: triage-ground-truth). The grounding rules above hold unchanged.
const REPORT_FIELDS: &str =
    "keep        \"all\", \"none\", or comma-separated 1-based inclusive line ranges like
              \"40-52,310-318\". Keep what answers the intent and what the task will need
              next; whatever you drop is gone from the working context. What the intent
              asks to SEE (a struct, the imports, the failing case) is kept. What it asks
              as a QUESTION (did it pass? did anything move? is X here?) is answered in
              the `note` from the output, keeping only the line that proves the answer —
              a totals or summary line — not every line the answer covers. Where many
              lines say one thing — a run of passing tests, progress and build chatter,
              repeated matches of the same kind — drop them and state the fact in the
              `note`. Never drop a line the agent may need to copy, search for, or edit
              from: paths, identifiers, exact error or warning text, the code it is about
              to change. Where the output enumerates distinct candidates, the lines you
              keep read afterwards as the whole set, so keep it whole or say in the
              `note` how many of how many survived.
  note        A report for the agent, who will see only the lines you kept: what you
              dropped and whether it is safe not to see it, the answers to the intent's
              questions, and anything it asked about that the output does not contain.
              Report what the output shows, not what to do about it: \"the doc comment
              says 'four channels'\", not \"the doc comment needs updating\"; \"line 72
              repeats line 70's text\", not \"delete line 72\". Do not restate the kept
              lines. Every claim must rest on the output; a claim that turns on one line
              puts that line in `keep`. Never say something passed, was absent, or was
              unrelated unless the output shows it. If you kept everything, leave it
              empty.
  intent_feedback
              Omit almost always. Use only when the `intent` gave you nothing to
              decide against — it restated the arguments, or named no question the
              output could answer — and say so in one clause, so the next intent is
              one you can act on. This is about the intent, never about the task: do
              not suggest what to call next or what the output means.";

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
///   carry several, keyed by the short labels of [`labels`](super::labels) rather than
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
pub(super) fn retain_instruction(offered: &[String], riders: &[&str], reader: Reader) -> String {
    let v = AskVariant::from_env();
    let scope = "Nothing earlier in this conversation is being triaged — those results are \
already decided, and their [n] tags are not result labels.";
    let shape = match offered {
        [_one] => format!(
            "The block above is the FULL, line-numbered output of the one call you just made. \
{scope} Answer with a top-level `keep` and `note`: no `results` array and no label, since \
there is only one result in question."
        ),
        many => format!(
            "The blocks above are the FULL, line-numbered output of the {n} calls you just made, \
each under a header naming its label. Line numbers restart at 1 within each result, so ranges \
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
    v.assemble(&shape, &fields)
}

/// How much of a roster item the triage view spells out — the cost axis
/// (docs/triage.md). The roster is re-sent on **every** triage and lives in the
/// fork's uncached tail, so its size is paid per tool result for the whole session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RosterStyle {
    /// `<name path="…">summary</name>` — what the boot preamble spells in full.
    Full,
    /// The same tag with the summary cut to its first sentence.
    FirstSentence,
    /// `<name path="…"/>` — the coordinates with no summary. **What ships.** The summary
    /// costs a quarter of the session's uncached tail and buys no measurable compliance;
    /// the `path=` attribute is the part that says "a file to open" rather than "a thing
    /// you can call", which is what a bare name was misread as.
    NamePath,
    /// `[name]` alone, as the roster shipped before.
    Name,
    /// No roster section at all, and so no `surfaced` decision this triage.
    Off,
}

impl RosterStyle {
    fn render(self, item: &Item) -> String {
        let Item::Listed {
            name,
            description,
            path,
        } = item
        else {
            return crate::context::render_item(item);
        };
        match self {
            Self::Full | Self::Off => crate::context::render_item(item),
            Self::FirstSentence => {
                format!(
                    "<{name} path=\"{path}\">{}</{name}>",
                    first_sentence(description)
                )
            }
            Self::NamePath => format!("<{name} path=\"{path}\"/>"),
            Self::Name => format!("[{name}]"),
        }
    }
}

/// The first sentence of `s` — up to the first `. ` that isn't inside an obvious
/// abbreviation-free position, else the whole string. Deliberately crude: this trims a
/// roster summary for a cost experiment, and over-keeping is harmless.
fn first_sentence(s: &str) -> &str {
    match s.find(". ") {
        Some(i) => &s[..=i],
        None => s,
    }
}

/// Which features of the ask are on — the ablation axis the `triage` rig varies
/// (docs/triage.md).
///
/// Experiment scaffolding. It lives here
/// rather than in the dev tool because the shape (single vs multi result) and the rider
/// rows must stay exactly what production sends — a variant assembled anywhere else would
/// drift from `retain_instruction`, which is the one divergence the replay rig exists to
/// rule out. A full-text override was the other candidate and can't work: a fifth of the
/// triages in a real session carry several results and need their ids interpolated.
struct AskVariant {
    roster: RosterStyle,
    banner: bool,
    /// The imperative leading the ask and repeated at its end.
    lede: bool,
    /// "This is a triage turn, not a work turn" plus the closed-toolbox fact.
    stance: bool,
    /// Per-field asks as a scannable block; off renders them as appended prose.
    block: bool,
    /// The verdict is the call's arguments; prose is not read. Ships on — measured
    /// against the all-features-off base, where there was headroom to see it: 14% → 5%,
    /// 15 triages improved to 4 regressed (p ≈ 0.02).
    verdict_channel: bool,
    /// Additive candidate: the output above is complete, nothing further is coming.
    output_final: bool,
    /// The shape sentence says "the call the agent just made" instead of "the call you
    /// just made" — the separate-model triage's alternative framing
    /// (backlog/inbox/distiller-experiment.md). Off: the tail keeps its first-person frame for
    /// both the shared and the separate-model triage (their `keep`/`note` rows differ by
    /// reader, [`REPORT_FIELDS`]).
    third_person: bool,
}

impl Default for AskVariant {
    /// What ships. Ablations turn features **off** from here, so the default path is the
    /// one under test in production and an unset env var changes nothing.
    fn default() -> Self {
        Self {
            roster: RosterStyle::NamePath,
            banner: true,
            lede: true,
            stance: true,
            block: true,
            verdict_channel: true,
            output_final: false,
            third_person: false,
        }
    }
}

impl AskVariant {
    /// Read `EFFERENCE_TRIAGE_ASK`: a comma-separated list of `no-<feature>` ablations and
    /// `+<feature>` additions, or `legacy` for every ablation at once. Unset — the whole
    /// production path — yields [`Self::default`]. An unrecognized token is a hard panic
    /// rather than a silent no-op: a typo'd ablation that quietly measured the shipped ask
    /// would read as "this feature does nothing".
    fn from_env() -> Self {
        let Ok(spec) = std::env::var("EFFERENCE_TRIAGE_ASK") else {
            return Self::default();
        };
        let mut v = Self::default();
        for token in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            match token {
                "shipped" => {}
                "legacy" => {
                    v.banner = false;
                    v.lede = false;
                    v.stance = false;
                    v.block = false;
                    v.verdict_channel = false;
                }
                "no-banner" => v.banner = false,
                "no-lede" => v.lede = false,
                "no-stance" => v.stance = false,
                "no-block" => v.block = false,
                "roster-full" => v.roster = RosterStyle::Full,
                "roster-sentence" => v.roster = RosterStyle::FirstSentence,
                "roster-path" => v.roster = RosterStyle::NamePath,
                "roster-name" => v.roster = RosterStyle::Name,
                "roster-off" => v.roster = RosterStyle::Off,
                "no-verdict-channel" => v.verdict_channel = false,
                "+verdict-channel" => v.verdict_channel = true,
                "+output-final" => v.output_final = true,
                "+third-person" => v.third_person = true,
                other => panic!("EFFERENCE_TRIAGE_ASK: unknown variant {other:?}"),
            }
        }
        v
    }

    /// Assemble the ask from the triage's `shape` sentence and its `fields` rows.
    ///
    /// `no-block` collapses the same rows into one paragraph rather than restoring the
    /// pre-reshape wording, so the ablation isolates *formatting* and holds content fixed;
    /// swapping the old sentences back would confound the two.
    fn assemble(&self, shape: &str, fields: &[&str]) -> String {
        let mut stance = String::new();
        if self.stance {
            stance.push_str(
                " This is a triage turn, not a work turn: the task continues on your NEXT turn, \
from whatever you keep here. No other tool will run until you have called `retain` — a call to \
anything else is rejected and this ask comes back.",
            );
        }
        if self.verdict_channel {
            stance.push_str(
                " The verdict is the `retain` call's arguments and nothing else — prose is not \
read. Describing what to keep instead of calling `retain` leaves the result untriaged.",
            );
        }
        let shape = match self.third_person {
            true => shape.replace("you just made", "the agent just made"),
            false => shape.to_string(),
        };
        let shape = match self.output_final {
            true => format!(
                "{shape} That call has finished and the output above is complete — nothing \
further is coming, so decide on what is there."
            ),
            false => shape,
        };

        let fields = match self.block {
            true => fields.join("\n"),
            false => fields
                .iter()
                .map(|f| f.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join(" "),
        };

        let mut out = String::new();
        if self.banner {
            out.push_str(ASK_BANNER);
            out.push_str("\n\n");
        }
        if self.lede {
            out.push_str(&format!(
                "Call `retain` now.{stance}\n\n{shape}\n\n{fields}"
            ));
            out.push_str("\n\nCall `retain`.");
        } else {
            // The pre-reshape placement: the imperative sits mid-paragraph, after the
            // description, and the ask closes on whatever rider came last.
            out.push_str(&format!("{shape} Call `retain` now.{stance}\n\n{fields}"));
        }
        out
    }
}

/// The triage-scoped riders, as field rows for [`retain_instruction`]'s block. Each is added
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

/// Render the inbox + background sections of a triage view (docs/inbox.md, docs/supervisor.md) — shared by the
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

/// One result's block in a triage view (docs/triage.md): a header naming the
/// result's label ([`labels`](super::labels)) and tool, then the capped, line-numbered
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
    view.push_str(&number_lines(lines));
    if dropped > 0 {
        view.push_str(&format!("\n       … [{dropped} more lines truncated] …"));
    }
    view
}

/// A bypassed result's block in the triage view (docs/triage.md). The call must
/// still be answered — the API rejects a request that leaves a `tool_use` unpaired — but
/// the output itself is not shown: it is going to the wire verbatim, so showing it would
/// spend the fork's uncached tail on text no verdict can change, and offering it beside
/// the results that *are* in question invites a verdict on it.
pub(super) fn bypassed_view(call: &ToolCall, shape: &str) -> String {
    if is_reply(call) {
        return format!(
            "═══ result · {} ═══\nnot triaged — this is the operator's reply to a question the \
             agent asked, and it goes to the agent unchanged. It is not one of the results you \
             are deciding about below.",
            call.name
        );
    }
    format!(
        "═══ result · {} ═══\nnot triaged — this result carries a {shape}, a value that \
         only works reproduced exactly, so its full output goes to you unchanged. It is not \
         one of the results you are deciding about below.",
        call.name
    )
}

/// Render every line `cat -n`-style (1-based, 6-wide, tab-separated). This is the
/// coordinate system the triage (docs/triage.md) addresses with
/// `retain(lines=…)`, so it is shown ONLY in the throwaway fork — never on the
/// canonical wire, which uses the leaner `@@`-hunks in
/// [`render_triaged`](crate::project::render_triaged).
pub(super) fn number_lines(lines: &[&str]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
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
    let (results, ask) = triage_parts(pending, inbox, background, roster, reader);
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
) -> (Vec<TriageResult>, String) {
    let mut sections = String::new();
    append_inbox_and_background(&mut sections, inbox, background);
    append_roster(&mut sections, roster);

    // Scoped to exactly this triage's ids, and shape-selected on its size — the tail is
    // discarded with the fork, so nothing here is cache-keyed (docs/triage.md).
    // A rider's field row is offered only when its section is actually above.
    let labels = labels(pending);
    let offered: Vec<String> = labels.iter().flatten().cloned().collect();
    let mut riders: Vec<&str> = Vec::new();
    if !inbox.is_empty() {
        riders.push(INBOX_FIELD);
    }
    if !background.is_empty() {
        riders.push(BACKGROUND_FIELD);
    }
    if roster_shown(roster) {
        riders.push(SURFACE_FIELD);
    }
    let instruction = retain_instruction(&offered, &riders, reader);

    // A bypassed result answers with a stand-in rather than its output: the harness
    // already decided it (docs/triage.md), and the ask does not offer it.
    let results = pending
        .iter()
        .zip(&labels)
        .map(|(p, label)| TriageResult {
            call_id: p.call.id.clone(),
            content: match (bypassed(p), label) {
                (Some(shape), _) => bypassed_view(p.call, shape),
                (None, Some(label)) => result_view(label, p.call, p.raw),
                (None, None) => unreachable!("labels() labels every unbypassed result"),
            },
            is_error: p.raw.is_error,
        })
        .collect();
    // `sections` already opens with its own blank line when non-empty, and is empty
    // outright on a triage carrying no riders — so trim rather than always joining with a
    // gap, which left the block opening on stray whitespace.
    let ask = match sections.trim() {
        "" => instruction,
        sections => format!("{sections}\n\n{instruction}"),
    };
    (results, ask)
}
