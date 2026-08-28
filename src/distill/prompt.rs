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
  note        One line, saying what this output showed — only what it showed. Do not
              generalize past it or explain what it means for the task. Ground it in
              the lines you kept, not the ones you dropped: if the note turns on a
              particular line, that line belongs in `keep`. Where the
              result header carries an `intent`, answer it from what is shown; \"it
              didn't\" is an answer, and often the one worth keeping.";

/// The `retain` ask, scoped to the results **this gate** is deciding
/// (docs/redaction-fork.md).
///
/// Two properties the text must hold, each answering a live failure on a long transcript:
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
/// `riders` are the field rows for whatever else this gate carries (inbox, background,
/// surfacing roster), placed inside the block rather than appended after it.
pub(super) fn retain_instruction(offered: &[String], riders: &[&str]) -> String {
    let v = AskVariant::from_env();
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
    let fields: Vec<&str> = std::iter::once(KEEP_FIELDS)
        .chain(riders.iter().copied())
        .collect();
    v.assemble(&shape, &fields)
}

/// How much of a roster item the peek view spells out — the cost axis
/// (docs/redaction-fork.md). The roster is re-sent on **every** gate and lives in the
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
    /// No roster section at all, and so no `surfaced` decision this gate.
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

/// Which features of the ask are on — the ablation axis the `peek` rig varies
/// (docs/redaction-fork.md).
///
/// Experiment scaffolding. It lives here
/// rather than in the dev tool because the shape (single vs multi result) and the rider
/// rows must stay exactly what production sends — a variant assembled anywhere else would
/// drift from `retain_instruction`, which is the one divergence the replay rig exists to
/// rule out. A full-text override was the other candidate and can't work: a fifth of the
/// gates in a real session carry several results and need their ids interpolated.
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
    /// 15 gates improved to 4 regressed (p ≈ 0.02).
    verdict_channel: bool,
    /// Additive candidate: the output above is complete, nothing further is coming.
    output_final: bool,
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
        }
    }
}

impl AskVariant {
    /// Read `EFFERENCE_PEEK_ASK`: a comma-separated list of `no-<feature>` ablations and
    /// `+<feature>` additions, or `legacy` for every ablation at once. Unset — the whole
    /// production path — yields [`Self::default`]. An unrecognized token is a hard panic
    /// rather than a silent no-op: a typo'd ablation that quietly measured the shipped ask
    /// would read as "this feature does nothing".
    fn from_env() -> Self {
        let Ok(spec) = std::env::var("EFFERENCE_PEEK_ASK") else {
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
                other => panic!("EFFERENCE_PEEK_ASK: unknown variant {other:?}"),
            }
        }
        v
    }

    /// Assemble the ask from the gate's `shape` sentence and its `fields` rows.
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
        let shape = match self.output_final {
            true => format!(
                "{shape} That call has finished and the output above is complete — nothing \
further is coming, so decide on what is there."
            ),
            false => shape.to_string(),
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

/// Whether this gate actually shows a roster section — and so whether the ask should offer
/// a `surfaced` field at all. Not the same as "the roster is non-empty": a style that
/// renders nothing must also drop the field, or the ask names a section that isn't there.
pub(super) fn roster_shown(roster: &[Item]) -> bool {
    !roster.is_empty() && AskVariant::from_env().roster != RosterStyle::Off
}

/// Render the surfacing roster section of a peek view (docs/redaction-fork.md): the
/// available-but-unloaded context items, each spelled exactly as the boot preamble spelled
/// it, through the preamble's own renderer so the two can't drift. Only in the tool-result
/// gate — the idle triage rides no work in flight.
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

/// A bypassed result's block in the peek view (docs/redaction-fork.md). The call must
/// still be answered — the API rejects a request that leaves a `tool_use` unpaired — but
/// the output itself is not shown: it is going to the wire verbatim, so showing it would
/// spend the fork's uncached tail on text no verdict can change, and offering it beside
/// the results that *are* in question invites a verdict on it.
pub(super) fn bypassed_view(call: &ToolCall, shape: &str) -> String {
    format!(
        "═══ result · [{}] {} ═══\nnot triaged — this result carries a {shape}, a value that \
         only works reproduced exactly, so its full output goes to you unchanged. It is not \
         one of the results you are deciding about below.",
        call.id.0, call.name
    )
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
