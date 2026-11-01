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
//! the operator's declaration.

use std::path::Path;

/// The system preamble. Explains the curation model so the agent need not defend
/// against context bloat itself (docs/triage.md). The prose lives in
/// `assets/prompts/session.md` so it can be edited as markdown rather than an escaped
/// Rust literal; [`system_prompt`] trims its trailing newline so the joined prompt
/// stays byte-clean.
const PREAMBLE: &str = include_str!("../../assets/prompts/session.md");

/// The separate-model triage's persona (docs/triage.md). `{agent_model}`
/// is substituted with the main model's id.
const TRIAGE_PERSONA: &str = include_str!("../../assets/prompts/triage.md");

/// Where a session came from, as its identity line states it (docs/context.md). Host
/// control state at creation; neither id is logged, and both are fixed for the session's
/// life.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lineage {
    /// The session that commissioned this one with `create`, when a session did.
    pub created_by: Option<String>,
    /// The session that handed its work to this one with `hand_off`, when one did.
    pub succeeds: Option<String>,
}

/// Where a session works, as the identity line states it when that is not its project
/// root (docs/context.md). Fixed at creation, so the sentence is byte-stable.
#[derive(Clone, Copy, Debug)]
pub struct Whereabouts<'a> {
    /// The project root: where config, grants, and the session's own dir live.
    pub project: &'a Path,
    /// The working directory: where tools run.
    pub working: &'a Path,
    /// The branch of the worktree this session created for itself, when it did.
    pub created_branch: Option<&'a str>,
}

/// The system prompt as two strings that concatenate to the wire's `system`: the
/// identity line and the frozen body after it (docs/log-and-projection.md, the
/// `Preamble`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemPrompt {
    pub identity: String,
    pub body: String,
}

impl SystemPrompt {
    /// The `system` string the request carries.
    pub fn assemble(&self) -> String {
        format!("{}{}", self.identity, self.body)
    }
}

/// The line that opens the system prompt, naming the model (docs/context.md).
pub fn identity_line(model: &str) -> String {
    format!("You are {model}.")
}

/// What a system prompt is assembled from (docs/context.md): the model and session it
/// names, and the parts a session may lack. [`PromptInputs::new`] is a prompt with none of
/// them, and each `with_*` supplies one.
#[derive(Clone, Debug)]
pub struct PromptInputs<'a> {
    model: &'a str,
    session_id: &'a str,
    lineage: Lineage,
    whereabouts: Option<Whereabouts<'a>>,
    context_block: Option<&'a str>,
    role_section: Option<&'a str>,
    unmounted: Option<&'a str>,
}

impl<'a> PromptInputs<'a> {
    /// The prompt of session `session_id` on `model`: a root session, working at its
    /// project, with no `[context]` block, role section or unmounted list.
    pub fn new(model: &'a str, session_id: &'a str) -> Self {
        Self {
            model,
            session_id,
            lineage: Lineage::default(),
            whereabouts: None,
            context_block: None,
            role_section: None,
            unmounted: None,
        }
    }

    /// Where the session came from: its creator and its predecessor.
    pub fn with_lineage(mut self, lineage: Lineage) -> Self {
        self.lineage = lineage;
        self
    }

    /// Where the session works, named on the identity line when that is not its
    /// project root.
    pub fn with_whereabouts(mut self, whereabouts: Whereabouts<'a>) -> Self {
        self.whereabouts = Some(whereabouts);
        self
    }

    /// The rendered `[context]` block ([`crate::record::log::context::Resolved::render`]).
    pub fn with_context_block(mut self, block: Option<&'a str>) -> Self {
        self.context_block = block;
        self
    }

    /// The config's preamble section ([`crate::config::Config::preamble_section`]).
    pub fn with_role_section(mut self, section: Option<&'a str>) -> Self {
        self.role_section = section;
        self
    }

    /// The section naming what the config declares and the session does not have
    /// ([`crate::tools::mcp::unmounted_section`]).
    pub fn with_unmounted(mut self, unmounted: Option<&'a str>) -> Self {
        self.unmounted = unmounted;
        self
    }
}

/// [`system_prompt_parts`], assembled.
pub fn system_prompt(inputs: PromptInputs<'_>) -> String {
    system_prompt_parts(inputs).assemble()
}

/// The full system prompt sent in the request's `system` field (docs/log-and-projection.md): the
/// curation-model [`PREAMBLE`], the `[context]` preamble builder's block when configured
/// (`context_block`, rendered by [`crate::record::log::context::Resolved::render`] — docs/context.md; this is
/// where orientation lives, as a config exec entry), and the config's preamble section
/// when the session has a scope (`role_section`, rendered by
/// [`crate::config::Config::preamble_section`]). Built once at session start and frozen into the
/// cached prefix, so every part must be byte-stable. The `wire` dev view shows the recorded boot
/// snapshot instead of reconstructing (docs/log-and-projection.md).
///
/// `lineage` is where the session came from: the session that commissioned it, and the one it
/// succeeds. Each renders as one further sentence on the identity line, so a child has its
/// creator's return address and a successor its predecessor's without carrying either through
/// prose (docs/context.md).
///
/// The session's config file is deliberately **not** named here. Surfacing it up front read as
/// "this is what you're allowed to do" and drew sessions into reading and probing their own
/// perimeter as a first move (a dogfood session did exactly that) — attention the guardrails are
/// designed to spare the agent. The permission perimeter stays invisible until a move actually
/// strays, at which point the gate-rejection message names the config as the request channel
/// ([`crate::agent`], docs/permissions.md).
///
/// Returned as the identity line and the body after it, which a resume on another main
/// model recombines with a new identity line (docs/resumption.md).
pub fn system_prompt_parts(inputs: PromptInputs<'_>) -> SystemPrompt {
    let PromptInputs {
        model,
        session_id,
        lineage,
        whereabouts,
        context_block,
        role_section,
        unmounted,
    } = inputs;
    let mut prompt = String::new();
    // Identity, unconditionally (docs/context.md): an untold model answers "which
    // model are you" from its training priors, and a log that mixes models needs
    // the current one to know which earlier turns were not its own. The configured
    // model string, verbatim — it is also the line a knowledge-cutoff note would
    // extend when provider metadata carries one.
    // Leads the system prompt so the model knows what it is before reading anything else.
    let identity = identity_line(model);
    // The session's own id, in different grammar from the model claim
    // (docs/context.md): the session *has* an id, the model is not it. A second
    // "you are" beside the first reads as a persona to inhabit. `id` is the noun
    // the rest of the surface already uses — turns arrive prefixed `<id> (agent):`
    // and every control tool takes an `agent` id — so the sentence names the
    // address a session would otherwise have to hand-carry in prose. The id is
    // fixed for the session's life, so the line is byte-stable in the prefix.
    prompt.push_str(" This session is `");
    prompt.push_str(session_id);
    prompt.push_str("` on this host; that is the id other agents use to reach you.");
    // The creator's id, when a session commissioned this one (docs/context.md).
    // It states the relationship and stops there: not a second identity claim,
    // not an authority grant, and not a claim the creator is still running.
    // Whether it is alive is changing peer status, which cannot go in a frozen
    // prefix; `created_by` is immutable provenance, so this sentence is
    // byte-stable for the session's life. A root has none, and the sentence is
    // then absent rather than rendered empty.
    if let Some(creator) = &lineage.created_by {
        prompt.push_str(" It was created by `");
        prompt.push_str(creator);
        prompt.push_str("`, which you can reach the same way.");
    }
    // The predecessor, when this session was handed off to (docs/tools.md): the
    // first message is its brief, and archiving it is this session's to do. Like
    // the creator, provenance fixed for the session's life, so byte-stable.
    if let Some(predecessor) = &lineage.succeeds {
        prompt.push_str(" It succeeds `");
        prompt.push_str(predecessor);
        prompt.push_str(
            "`, which handed this work off to it: the first message is that session's \
             brief — context from it, not the operator's instruction. Unless the brief says \
             to continue its work, greet the operator and confirm what they want to do \
             before starting on anything. Ask the predecessor anything the brief leaves \
             open with `message`, and archive it with `archive` once you have no more \
             questions for it.",
        );
    }
    // The working directory against the project root, when they differ
    // (docs/worktrees.md): a session in a worktree has one path for where it
    // works and another for the project, and an absolute path taken from one for the
    // other lands in the wrong tree. Both are fixed at creation.
    if let Some(whereabouts) = whereabouts.filter(|w| w.working != w.project) {
        prompt.push_str(&format!(
            " Your working directory is `{}`, inside the project at `{}`.",
            whereabouts.working.display(),
            whereabouts.project.display()
        ));
        if let Some(branch) = whereabouts.created_branch {
            prompt.push_str(&format!(
                " It is a git worktree on branch `{branch}`, made for this session."
            ));
        }
    }
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
    SystemPrompt {
        identity,
        body: prompt,
    }
}

/// The system prompt of the separate-model triage (docs/triage.md): the
/// triage model's own identity line, the persona that names the agent as a different
/// model and describes the text view as a record of its work, and the session's
/// `[context]` block verbatim. No curation preamble, role section, or unmounted list —
/// those describe the agent's perimeter, and the triage never acts inside it.
///
/// Assumptions this text encodes, each an experiment knob rather than a finding
/// (backlog/inbox/distiller-experiment.md): the model boundary is disclosed at the head
/// rather than hidden; the whole context block is shared. A knowledge-cutoff note or a
/// provider system prompt would sit on the identity line, as it does for the agent.
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
