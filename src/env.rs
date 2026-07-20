//! The system preamble assembly (docs/substrate.md, docs/context.md).
//!
//! **Caching discipline (load-bearing).** The system prompt sits in the cached
//! prefix (`tools → system → messages`), so everything assembled here must be
//! **byte-stable for the whole session** or it invalidates the cache every turn.
//! [`system_prompt`] is built **once at startup** from parts that are themselves
//! frozen at boot (the `[context]` block, the role section, the config note).
//!
//! Orientation — *where the session is* (cwd, platform) — is no longer a
//! hardcoded block: it is an ordinary `[context]` exec entry in the config
//! (the scaffold ships one; see docs/context.md), so what a session boots
//! knowing is entirely the operator's declaration. The incident that motivated
//! orientation (a live Haiku run searching for Python/JS/TS files in a Rust
//! repo before finding its footing) is now answered by the config, not the code.

/// The system preamble. Explains the curation model so the agent stops defending
/// against context bloat the old way (docs/redaction-fork.md) — the run that
/// motivated this watched the model reflexively `grep`/`head` its own tool output.
/// The prose lives in `assets/prompts/session.md` so it can be edited as markdown
/// rather than an escaped Rust literal (the same `include_str!` pattern the spotter
/// persona uses, see [`crate::spotter`]); [`system_prompt`] trims its trailing
/// newline so the joined prompt stays byte-clean.
const PREAMBLE: &str = include_str!("../assets/prompts/session.md");

/// The full system prompt sent in the request's `system` field (docs/substrate.md): the
/// curation-model [`PREAMBLE`], the `[context]` preamble builder's block when configured
/// (`context_block`, rendered by [`crate::context::Resolved::render`] — docs/context.md; this is
/// where orientation now lives, as a config exec entry), and the config's preamble section
/// when the session has a scope (`role_section`, rendered by
/// [`crate::config::Config::preamble_section`]). Built once at session start and frozen into the
/// cached prefix, so every part must be byte-stable. The `wire` dev view shows the recorded boot
/// snapshot instead of reconstructing (docs/substrate.md).
///
/// The session's config file is deliberately **not** named here. Surfacing it up front read as
/// "this is what you're allowed to do" and drew sessions into reading and probing their own
/// perimeter as a first move (a dogfood session did exactly that) — attention the guardrails are
/// designed to spare the agent. The permission perimeter stays invisible until a move actually
/// strays, at which point the gate-rejection message names the config as the request channel
/// ([`crate::agent`], backlog/permissions/session-dir-and-config-as-reviewed-file.md).
pub fn system_prompt(context_block: Option<&str>, role_section: Option<&str>) -> String {
    // `include_str!` keeps the file's trailing newline; trim it so the join is clean.
    let mut prompt = PREAMBLE.trim_end().to_string();
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
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_appends_role_section_when_present() {
        let without = system_prompt(None, None);
        let with = system_prompt(None, Some("Your role for this session:\n\nStay put."));
        // The role section is appended verbatim after the env block; without a role the
        // prompt is exactly the preamble + env (no trailing role text).
        assert!(with.starts_with(&without));
        assert!(with.ends_with("Your role for this session:\n\nStay put."));
        assert!(!without.contains("Your role for this session"));
    }

    /// The `[context]` block splices in after the curation preamble and **before** the role/scope —
    /// knowledge before perimeter (docs/context.md).
    #[test]
    fn system_prompt_splices_context_block_before_the_role() {
        let s = system_prompt(
            Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
            Some("Your role for this session:\n\nDo it."),
        );
        assert!(s.contains("<CLAUDE.md>\nhi\n</CLAUDE.md>"));
        assert!(s.find("<CLAUDE.md>").unwrap() < s.find("Your role").unwrap());
        // No block → nothing spliced.
        assert!(!system_prompt(None, None).contains("<CLAUDE.md>"));
    }

    /// The session's config file is never named in the system prompt: the permission perimeter
    /// stays out of the agent's attention until a move strays and the gate surfaces it. Guards
    /// against reintroducing the up-front config mention a dogfood session found distracting.
    #[test]
    fn system_prompt_never_names_the_config_file() {
        let full = system_prompt(
            Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
            Some("Your role for this session:\n\nDo it."),
        );
        assert!(!full.contains(".toml"));
        assert!(!full.to_lowercase().contains("permission"));
        assert!(!full.contains("request a change"));
    }
}
