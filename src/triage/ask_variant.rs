//! The ask's experiment axes (docs/triage.md): the roster's spelling and the features
//! of the `retain` ask that the `triage` replay rig ablates through
//! `EFFERENCE_TRIAGE_ASK`. Unset, every axis is what ships.

use super::*;

/// How much of a roster item the triage view spells out — the cost axis
/// (docs/triage.md). The roster is re-sent on **every** triage and lives in the
/// fork's uncached tail, so its size is paid per tool result for the whole session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RosterStyle {
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
    pub(super) fn render(self, item: &Item) -> String {
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
// One flag per ablation axis, each set by its own `EFFERENCE_TRIAGE_ASK` token; any
// combination is a variant.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct AskVariant {
    pub(super) roster: RosterStyle,
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
    /// Read `EFFERENCE_TRIAGE_ASK`: a comma-separated list of `no-<feature>` ablations and
    /// `+<feature>` additions, or `legacy` for every ablation at once. Unset — the whole
    /// production path — yields [`Self::default`]. An unrecognized token is a hard panic
    /// rather than a silent no-op: a typo'd ablation that quietly measured the shipped ask
    /// would read as "this feature does nothing".
    pub(super) fn from_env() -> Self {
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
    ///
    /// `framing` words the stance's "the task continues" for whom it addresses: the
    /// agent's own next turn, or the agent's next turn as a separate model sees it.
    pub(super) fn assemble(&self, shape: &str, fields: &[&str], framing: Framing) -> String {
        let mut stance = String::new();
        if self.stance {
            let continues = match framing {
                Framing::FirstPerson => "the task continues on your NEXT turn",
                Framing::ThirdPerson => "the agent's task continues on its NEXT turn",
            };
            stance.push_str(&format!(
                " This is a triage turn, not a work turn: {continues}, from whatever you keep \
here. No other tool will run until you have called `retain` — a call to anything else is \
rejected and this ask comes back."
            ));
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
