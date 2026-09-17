//! The system preamble assembly (docs/log-and-projection.md, docs/context.md).
//!
//! **Caching discipline.** The system prompt sits in the cached
//! prefix (`tools → system → messages`), so everything assembled here must be
//! **byte-stable for the whole session** or it invalidates the cache every turn.
//! [`system_prompt`] is built **once at startup** from parts that are themselves
//! frozen at boot (the `[context]` block, the role section, the config note).
//!
//! Orientation — *where the session is* (cwd, platform) — is not a hardcoded
//! block: it is an ordinary `[context]` exec entry in the config (the scaffold
//! ships one; see docs/context.md), so what a session boots knowing is entirely
//! the operator's declaration. Without it a session spends its first turns
//! working out what kind of repo it is in.

/// The system preamble. Explains the curation model so the agent stops defending
/// against context bloat the old way (docs/redaction-fork.md) — the run that
/// motivated this watched the model reflexively `grep`/`head` its own tool output.
/// The prose lives in `assets/prompts/session.md` so it can be edited as markdown
/// rather than an escaped Rust literal (the same `include_str!` pattern the spotter
/// persona uses, see [`crate::spotter`]); [`system_prompt`] trims its trailing
/// newline so the joined prompt stays byte-clean.
const PREAMBLE: &str = include_str!("../assets/prompts/session.md");

/// The separate-model fork peek's persona (docs/redaction-fork.md). `{agent_model}`
/// is substituted with the main model's id.
const TRIAGE_PERSONA: &str = include_str!("../assets/prompts/triage.md");

/// The full system prompt sent in the request's `system` field (docs/log-and-projection.md): the
/// curation-model [`PREAMBLE`], the `[context]` preamble builder's block when configured
/// (`context_block`, rendered by [`crate::context::Resolved::render`] — docs/context.md; this is
/// where orientation lives, as a config exec entry), and the config's preamble section
/// when the session has a scope (`role_section`, rendered by
/// [`crate::config::Config::preamble_section`]). Built once at session start and frozen into the
/// cached prefix, so every part must be byte-stable. The `wire` dev view shows the recorded boot
/// snapshot instead of reconstructing (docs/log-and-projection.md).
///
/// The session's config file is deliberately **not** named here. Surfacing it up front read as
/// "this is what you're allowed to do" and drew sessions into reading and probing their own
/// perimeter as a first move (a dogfood session did exactly that) — attention the guardrails are
/// designed to spare the agent. The permission perimeter stays invisible until a move actually
/// strays, at which point the gate-rejection message names the config as the request channel
/// ([`crate::agent`], docs/permissions.md).
pub fn system_prompt(
    model: &str,
    session_id: &str,
    context_block: Option<&str>,
    role_section: Option<&str>,
    unmounted: Option<&str>,
) -> String {
    let mut prompt = String::new();
    // Identity, unconditionally (docs/context.md): an untold model answers "which
    // model are you" from its training priors, and a log that mixes models needs
    // the current one to know which earlier turns were not its own. The configured
    // model string, verbatim — it is also the line a knowledge-cutoff note would
    // extend when provider metadata carries one.
    // Leads the system prompt so the model knows what it is before reading anything else.
    prompt.push_str("You are ");
    prompt.push_str(model);
    prompt.push('.');
    // The session's own id, in different grammar from the model claim
    // (docs/context.md): the session *has* an id, the model is not it. A second
    // "you are" beside the first reads as a persona to inhabit. `id` is the noun
    // the rest of the surface already uses — turns arrive prefixed `agent[<id>]:`
    // and every control tool takes a `session` id — so the sentence names the
    // address a session would otherwise have to hand-carry in prose. The id is
    // fixed for the session's life, so the line is byte-stable in the prefix.
    prompt.push_str(" This session is `");
    prompt.push_str(session_id);
    prompt.push_str("` on this host; that is the id other agents use to reach you.");
    // How the harness works — `include_str!` keeps the file's trailing newline;
    // trim it so the join is clean.
    prompt.push_str("\n\n");
    prompt.push_str(PREAMBLE.trim_end());
    // Knowledge (what the session knows) sits before the perimeter (what it may do)
    // (docs/context.md).
    if let Some(block) = context_block {
        prompt.push_str("\n\n");
        prompt.push_str(block);
    }
    if let Some(section) = role_section {
        prompt.push_str("\n\n");
        prompt.push_str(section);
    }
    // Last, next to the perimeter it qualifies: what the operator declared that
    // this session does *not* have (docs/tools.md). Without it, the model reads
    // its tool table as the whole truth and tells the operator a configured
    // capability does not exist.
    if let Some(unmounted) = unmounted {
        prompt.push_str("\n\n");
        prompt.push_str(unmounted);
    }
    prompt
}

/// The system prompt of the separate-model fork peek (docs/redaction-fork.md): the
/// triage model's own identity line, the persona that discloses the model seam once
/// and then reads the transcript as its own, and the session's `[context]` block
/// verbatim. No curation preamble, role section, or unmounted list — those describe
/// the agent's perimeter, and the peek never acts inside it.
///
/// Assumptions this text encodes, each an experiment knob rather than a finding
/// (backlog/distiller-experiment.md): the seam is disclosed at the head rather than
/// hidden; the tail ask stays first-person and byte-identical to the shared peek's;
/// the whole context block is shared. A knowledge-cutoff note or a provider system
/// prompt would sit on the identity line, as it does for the agent.
pub fn triage_system_prompt(
    triage_model: &str,
    agent_model: &str,
    context_block: Option<&str>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are ");
    prompt.push_str(triage_model);
    prompt.push('.');
    prompt.push_str("\n\n");
    prompt.push_str(
        TRIAGE_PERSONA
            .trim_end()
            .replace("{agent_model}", agent_model)
            .as_str(),
    );
    if let Some(block) = context_block {
        prompt.push_str("\n\n");
        prompt.push_str(block);
    }
    prompt
}

#[cfg(test)]
mod test;
