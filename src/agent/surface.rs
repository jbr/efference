//! The tool surface a turn advertises: the array itself, the names a call may use, and
//! the lookups that mirror it for the spotter and the catalog route.

use super::*;

/// The exact `tools[]` a turn advertises: the world tools (`toolbox`) plus the
/// harness tools handled structurally in the loop — `consolidate` (docs/consolidate.md)
/// and the supervisor/inbox tools (docs/supervisor.md). One source of truth so the
/// keepalive ping (docs/log-and-projection.md) and the real turns send a byte-identical prefix;
/// drift here silently breaks the tools cache tier.
pub fn tool_definitions(toolbox: &Toolbox) -> Vec<Value> {
    let mut tools = toolbox.definitions();
    tools.push(consolidate::definition());
    tools.extend(supervisor::tool_definitions());
    // The peek tools (docs/redaction-fork.md) stand in every turn's array even though
    // only a fork peek's tail instruction ever asks for them: with the tools array,
    // `tool_choice`, and thinking policy identical, the fork peek is the same
    // conversation continued with a different tail and reads the agent lineage's cache
    // instead of keying its own (docs/log-and-projection.md). Their schemas are static
    // for the same reason. An un-asked call is paired with a nudge result in the loop.
    tools.push(crate::distill::retain_tool());
    tools.push(crate::distill::triage_tool());
    tools
}

/// The names a turn's `tools[]` carries that a call may actually name — read off
/// [`tool_definitions`] itself, so the advertised array and this list cannot drift.
///
/// The peek tools are excluded: `retain`/`triage` stand in every array for
/// cache-lineage reasons but nothing is ever asked of them on an agent turn, and an
/// un-asked call is short-circuited before this is consulted
/// (docs/redaction-fork.md). What is left is what a call can name and have run.
pub fn callable_names(toolbox: &Toolbox) -> Vec<String> {
    tool_definitions(toolbox)
        .iter()
        .filter_map(|d| d.get("name")?.as_str().map(str::to_string))
        .filter(|name| !crate::distill::is_peek_tool(name))
        .collect()
}

/// The spotter's brief for any tool a turn advertises — the lookup mirror of
/// [`tool_definitions`]: the toolbox providers, then the loop-owned tools
/// (`consolidate`, the supervisor/inbox family). One lookup covering the whole
/// array, so a tool in `tools[]` can't be invisible to the permission rung —
/// the loop-owned family once was, and the spotter judged `check_inbox` and
/// `list_processes` as bare names, calling them opaque (docs/permissions.md).
/// Bash stays brief-less by design (judged as command text), and the peek tools
/// never reach a gate (an un-asked call pairs with a nudge result).
pub fn brief<'a>(toolbox: &'a Toolbox, name: &str) -> Option<crate::tools::ToolBrief<'a>> {
    toolbox
        .brief(name)
        .or_else(|| consolidate::brief(name))
        .or_else(|| supervisor::brief(name))
}

/// The full definition behind every wire name a turn's `tools[]` can carry —
/// the `GET /v1/tools/:name` mirror of [`tool_definitions`], the way [`brief`]
/// already mirrors it for the spotter. Built once, before the loop starts (the
/// toolbox's mounted set and this harness's own tool family are both fixed for
/// the session, docs/log-and-projection.md), and snapshotted into `Session`
/// ([`crate::daemon::Session::set_tool_catalog`]).
///
/// A catalog built from `toolbox.tool_catalog()` alone once missed this whole
/// family — `check_inbox`, `list_processes`, `consolidate`, the two peek
/// tools — the same shape of gap [`brief`]'s doc comment records for the
/// spotter (`check_inbox`/`list_processes` once judged as bare names because
/// the lookup consulted only the providers). Composed from the same brief
/// sources `brief` reads, plus the two peek tools, which `brief` excludes
/// (they never reach a gate) but this route does not: `retain`/`triage` still
/// ride every turn's `tools[]`, so a reader destructuring the fork peek's own
/// call wants their schema too.
///
/// None of these five sources inject [`crate::tools::INTENT`] the way a
/// `Toolbox`-mounted tool's wire entry does — [`tool_definitions`] pushes
/// `consolidate::definition()`, `supervisor::tool_definitions()`, and the peek
/// tools onto `toolbox.definitions()` *after* `with_intent` has already run
/// over the toolbox half — so each is served with `inject_intent: false`,
/// matching the schema the model was actually sent.
pub fn tool_catalog(
    toolbox: &Toolbox,
) -> std::collections::HashMap<String, crate::tools::ToolEntry> {
    let mut catalog = toolbox.tool_catalog();

    if let Some(brief) = consolidate::brief(consolidate::NAME) {
        catalog.insert(
            consolidate::NAME.to_string(),
            crate::tools::ToolEntry::from_definition(brief.definition, None, false),
        );
    }
    for name in [
        supervisor::LIST,
        supervisor::READ,
        supervisor::KILL,
        supervisor::WATCH,
        supervisor::CHECK_INBOX,
        supervisor::WAIT,
        supervisor::ASK,
    ] {
        if let Some(brief) = supervisor::brief(name) {
            catalog.insert(
                name.to_string(),
                crate::tools::ToolEntry::from_definition(brief.definition, None, false),
            );
        }
    }
    for raw in [crate::distill::retain_tool(), crate::distill::triage_tool()] {
        if let Some(name) = raw.get("name").and_then(Value::as_str) {
            catalog.insert(
                name.to_string(),
                crate::tools::ToolEntry::from_raw(name, &raw, None, false),
            );
        }
    }
    catalog
}
