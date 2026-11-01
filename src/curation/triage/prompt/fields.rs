//! The field rows of the `retain` ask (docs/triage.md): `keep`/`note` for each reader,
//! and the riders' rows in each framing.

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
pub(in crate::curation::triage) const KEEP_FIELDS: &str =
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
pub(in crate::curation::triage) const REPORT_FIELDS: &str =
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

/// The triage-scoped riders, as field rows for
/// [`retain_instruction`](super::retain_instruction)'s block. Each is added only when its
/// section is actually present in the request.
pub(in crate::curation::triage) const INBOX_FIELD: &str = concat!(
    "  inbox       The pending notifications under \"inbox\" in the sections after this ask, an
              [id] each. ",
    dispositions!()
);

/// [`INBOX_FIELD`] in the third person ([`Framing`](super::Framing)).
pub(in crate::curation::triage) const INBOX_FIELD_THIRD_PERSON: &str = concat!(
    "  inbox       The pending notifications under \"inbox\" in the sections after this ask, an
              [id] each. ",
    dispositions_third_person!()
);

/// Offered when a result in the triage carries images. They reach the agent whatever the
/// verdict (`render_result` projects `raw.images` for every triaged verdict), and the triage
/// request does not carry them, so without this row a note says the image "was not retained"
/// (docs/triage.md).
pub(in crate::curation::triage) const IMAGES_FIELD: &str =
    "  images      A result header that says `images:` is a result that carries images. They reach
              the agent as they are, whatever you decide, and are not yours to triage: triage the
              text (for `read_image`, the header line) and the inbox, and write no note about the
              image or whether it was retained. `keep: none` on such a result drops only its text.";

pub(in crate::curation::triage) const BACKGROUND_FIELD: &str =
    "  background  The processes under \"background\" in the sections after this ask, a [p#n]
              handle each. `pin` the handles whose current output you want captured onto the
              record now, and `mute` the ones whose tail you no longer need (they keep running, and still
              notify you when they exit). Anything you don't list keeps streaming its
              latest tail to you at the next decision point.";

pub(in crate::curation::triage) const SURFACE_FIELD: &str =
    "  surfaced    The files under \"context\" in the sections after this ask — available to you,
              not yet loaded, each listed with the path to read. Name any that genuinely bear on
              what you're doing right now and it will be surfaced as a reminder to read it
              before you rely on memory. Name nothing if none apply.";

/// [`BACKGROUND_FIELD`] in the third person ([`Framing`](super::Framing)).
pub(in crate::curation::triage) const BACKGROUND_FIELD_THIRD_PERSON: &str =
    "  background  The processes under \"background\" in the sections after this ask, a [p#n]
              handle each. `pin` the handles whose current output the agent needs captured onto
              the record now, and `mute` the ones whose tail it no longer needs (they keep running,
              and still notify the agent when they exit). Anything you don't list keeps streaming
              its latest tail to the agent at the next decision point.";

/// [`SURFACE_FIELD`] in the third person ([`Framing`](super::Framing)).
pub(in crate::curation::triage) const SURFACE_FIELD_THIRD_PERSON: &str =
    "  surfaced    The files under \"context\" in the sections after this ask — available to the
              agent, not yet loaded, each listed with the path to read. Name any that genuinely
              bear on what the agent is doing right now and it will be surfaced to the agent as a
              reminder to read it before it relies on memory. Name nothing if none apply.";
