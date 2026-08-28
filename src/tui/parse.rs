//! Decoding `state`-frame topic values into the typed views the sidebar and status bar
//! render (docs/control-api.md).
//!
//! Each function takes one topic's **whole value** — the `data` of a `state` frame,
//! identical to the same-named key of `GET /state` — never the composite blob. The
//! bridge feeds them from frames; anything reading `GET /state` directly indexes the
//! topic key first.

use super::*;

/// The `processes` topic's wire shape (`supervisor::ProcessState`): structure only.
/// `pid` is on the wire too — an attribute for direct OS interaction — but the panel
/// has no use for it yet, and serde ignores what isn't named.
#[derive(serde::Deserialize)]
struct WireProcess {
    id: u64,
    command: String,
    spawned_at: u64,
    background: bool,
    muted: bool,
}

/// Parse the `processes` topic's array into [`BgProc`]s (docs/supervisor.md). A
/// missing/malformed field is an empty list — the panel just hides.
///
/// The topic carries every running process, flagged rather than filtered
/// (docs/control-api.md); the panel's choices are made here — muted rows are
/// dismissed-from-view, and the foreground entry already renders live in the
/// transcript's in-flight block, so both are dropped.
///
/// Tail and last-chunk are left empty here: the topic does not carry them, and
/// [`State::handle_event`](crate::tui::State::handle_event) merges each process's
/// locally-accumulated tail back in so a topic replacement can't wipe it.
pub(super) fn parse_processes(value: &serde_json::Value) -> Vec<BgProc> {
    serde_json::from_value::<Vec<WireProcess>>(value.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.background && !w.muted)
        .map(|w| {
            let mut proc = BgProc {
                id: w.id,
                command: w.command,
                spawned_at: w.spawned_at,
                last_chunk_at: None,
                tail: String::new(),
                running_secs: 0,
                idle_secs: 0,
            };
            proc.refresh_elapsed();
            proc
        })
        .collect()
}

/// Parse the `gate` topic's object into a [`PendingGate`]. `null` — the topic's
/// cleared value — is no gate.
pub(super) fn parse_pending_gate(pending: &serde_json::Value) -> Option<PendingGate> {
    if pending.is_null() {
        return None;
    }
    Some(PendingGate {
        call_id: pending.get("call_id")?.as_str()?.to_string(),
        name: pending.get("name")?.as_str()?.to_string(),
        input: pending
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        reason: pending
            .get("reason")
            .and_then(|r| r.as_str())
            .map(String::from),
    })
}

/// Filesystem perms → a compact tag for the POLICY readout.
pub(super) fn perm_str(read: bool, write: bool) -> &'static str {
    match (read, write) {
        (_, true) => "rw",
        (true, false) => "r",
        (false, false) => "-",
    }
}

/// Merge the `role`, `proposals`, and `review_level` topics into a [`PolicyView`] for
/// the sidebar (docs/permissions.md). Three topics, one view: the daemon serves them
/// separately (docs/control-api.md), and this client merges them only for its own
/// rendering convenience, so the bridge holds the latest value of each and re-merges
/// when any of them moves. Proposals are merged into their section and sorted alongside
/// the committed grants, flagged `proposed`, so a commit drops the flag in place
/// without reflow. `ConfigChange` is externally tagged
/// (`{"Command":{"command":…}}`, `{"Path":{…}}`, `{"Tool":{"name":…}}`, `{"Scope":{"text":…}}`).
pub(super) fn parse_policy(
    role: &serde_json::Value,
    proposals: &serde_json::Value,
    review_level: &serde_json::Value,
) -> Option<PolicyView> {
    if role.is_null() {
        return None;
    }

    // `label` renders the stored key for a human. Commands are already what the operator
    // typed; a tool name arrives in its wire form and is shown dotted, the same as in the
    // transcript and in the config file the sidebar mirrors (docs/tools.md).
    let str_entries = |key: &str, label: fn(&str) -> String| -> Vec<PolicyEntry> {
        role.get(key)
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| PolicyEntry {
                        label: label(s),
                        proposed: false,
                        seq: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut commands = str_entries("commands", |s| s.to_string());
    let mut tools = str_entries("tools", crate::mcp::display_name);
    let mut paths: Vec<PolicyEntry> = role
        .get("paths")
        .and_then(|p| p.as_object())
        .map(|m| {
            m.iter()
                .map(|(path, perms)| {
                    let r = perms.get("read").and_then(|b| b.as_bool()).unwrap_or(false);
                    let w = perms
                        .get("write")
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false);
                    PolicyEntry {
                        label: format!("{path}  {}", perm_str(r, w)),
                        proposed: false,
                        seq: None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let mut scope_proposal = None;
    if let Some(arr) = proposals.as_array() {
        for prop in arr {
            let (Some(seq), Some(change)) =
                (prop.get("seq").and_then(|s| s.as_u64()), prop.get("change"))
            else {
                continue;
            };
            if let Some(c) = change
                .get("Command")
                .and_then(|c| c.get("command"))
                .and_then(|s| s.as_str())
            {
                commands.push(PolicyEntry {
                    label: c.to_string(),
                    proposed: true,
                    seq: Some(seq),
                });
            } else if let Some(p) = change.get("Path") {
                let path = p.get("path").and_then(|s| s.as_str()).unwrap_or("");
                let r = p.get("read").and_then(|b| b.as_bool()).unwrap_or(false);
                let w = p.get("write").and_then(|b| b.as_bool()).unwrap_or(false);
                paths.push(PolicyEntry {
                    label: format!("{path}  {}", perm_str(r, w)),
                    proposed: true,
                    seq: Some(seq),
                });
            } else if let Some(n) = change
                .get("Tool")
                .and_then(|t| t.get("name"))
                .and_then(|s| s.as_str())
            {
                tools.push(PolicyEntry {
                    // A proposal is shown in its committed section, in sorted position — so it
                    // must be labelled the same way a committed grant is, or it would sort and
                    // read as a different entry than the one it becomes.
                    label: crate::mcp::display_name(n),
                    proposed: true,
                    seq: Some(seq),
                });
            } else if let Some(t) = change
                .get("Scope")
                .and_then(|s| s.get("text"))
                .and_then(|s| s.as_str())
            {
                scope_proposal = Some((seq, t.to_string()));
            }
        }
    }

    // Sort each section so a proposed entry sits in its committed position (no reflow on commit).
    commands.sort_by(|a, b| a.label.cmp(&b.label));
    paths.sort_by(|a, b| a.label.cmp(&b.label));
    tools.sort_by(|a, b| a.label.cmp(&b.label));

    Some(PolicyView {
        scope: role.get("scope").and_then(|s| s.as_str()).map(String::from),
        scope_proposal,
        commands,
        paths,
        tools,
        review_level: review_level.as_str().map(String::from),
    })
}

/// Parse the `pace` topic's object into a [`PaceView`] (docs/tui.md).
pub(super) fn parse_pace(pace: &serde_json::Value) -> Option<PaceView> {
    Some(PaceView {
        speed_ms: pace.get("speed_ms")?.as_u64()?,
        held: pace.get("held")?.as_bool()?,
    })
}

/// Parse the `upstream_failure` topic — the optional last non-success upstream response.
pub(super) fn parse_upstream_failure(failure: &serde_json::Value) -> Option<UpstreamFailureView> {
    if failure.is_null() {
        return None;
    }
    Some(UpstreamFailureView {
        status: u16::try_from(failure.get("status")?.as_u64()?).ok()?,
        count: u32::try_from(failure.get("count")?.as_u64()?).ok()?,
    })
}

/// Parse the `upstream_retry` topic — the retry in flight, or `None` for the
/// all-clear (docs/transport.md). `next_attempt_at` and `detail` are read past:
/// see [`UpstreamRetryView`].
pub(super) fn parse_upstream_retry(retry: &serde_json::Value) -> Option<UpstreamRetryView> {
    if retry.is_null() {
        return None;
    }
    Some(UpstreamRetryView {
        surface: retry.get("surface")?.as_str()?.to_string(),
        message: retry.get("message")?.as_str()?.to_string(),
    })
}

/// Parse the `stall` topic — a trajectory that gave up, or `None` once one
/// starts again (docs/transport.md). Only the lead line is kept: see
/// [`StallView`].
pub(super) fn parse_stall(stall: &serde_json::Value) -> Option<StallView> {
    if stall.is_null() {
        return None;
    }
    Some(StallView {
        message: stall.get("message")?.as_str()?.to_string(),
    })
}

/// Parse the `session_identity` topic into a [`SessionIdentityView`]
/// (docs/control-api.md). `null` — a daemon that never set it — is no view, and the
/// sidebar's SESSION section simply doesn't render.
///
/// The cwd is shortened against `$HOME` here rather than at render time, so the
/// renderer stays a pure function of state and the snapshot harness stays
/// deterministic (the same reason the usage stats derive their elapsed at parse
/// time).
pub(super) fn parse_session_identity(value: &serde_json::Value) -> Option<SessionIdentityView> {
    if value.is_null() {
        return None;
    }
    Some(SessionIdentityView {
        model: value.get("model")?.as_str()?.to_string(),
        cwd: home_relative(value.get("cwd")?.as_str()?),
    })
}

/// `/Users/jbr/code/efference` → `~/code/efference`, when `$HOME` is a prefix of a
/// whole path component. Anything else — no home, a different tree, a partial
/// component match — is returned verbatim.
fn home_relative(cwd: &str) -> String {
    let Some(home) = std::env::home_dir() else {
        return cwd.to_string();
    };
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return cwd.to_string();
    }
    match cwd.strip_prefix(home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => cwd.to_string(),
    }
}

/// Parse the `usage` topic's object into a [`UsageStats`]. `null` — no requests yet —
/// is no stats.
pub(super) fn parse_usage(usage: &serde_json::Value) -> Option<UsageStats> {
    if usage.is_null() {
        return None;
    }
    let cumulative = usage.get("cumulative")?;
    // Context-window occupancy (docs/transport.md): `context` is null when the
    // daemon fetched no capabilities; `used` is null before the first request.
    let context = usage.get("context").filter(|c| !c.is_null());
    // Session cost: null when the provider has no per-request accounting surface.
    let cost = usage.get("cost").filter(|c| !c.is_null());
    // Triage retention (docs/redaction-fork.md). Absent on a daemon predating the
    // field; a missing counter reads as 0, which the sidebar renders as absent.
    let retention = |field: &str| {
        usage
            .get("retention")
            .and_then(|r| r.get(field))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };
    let last_request_at = usage.get("last_request_at").and_then(|v| v.as_u64());
    Some(UsageStats {
        input_tokens: cumulative.get("input_tokens")?.as_u64()?,
        output_tokens: cumulative.get("output_tokens")?.as_u64()?,
        cache_read_input_tokens: cumulative.get("cache_read_input_tokens")?.as_u64()?,
        cache_creation_input_tokens: cumulative.get("cache_creation_input_tokens")?.as_u64()?,
        last_request_at,
        seconds_since_last_request: last_request_at.map(super::secs_since),
        context_limit: context
            .and_then(|c| c.get("limit"))
            .and_then(|v| v.as_u64()),
        context_used: context.and_then(|c| c.get("used")).and_then(|v| v.as_u64()),
        cost_usd: cost.and_then(|c| c.get("usd")).and_then(|v| v.as_f64()),
        cost_unpriced: cost
            .and_then(|c| c.get("unpriced"))
            .and_then(|v| v.as_u64()),
        kept_lines: retention("kept_lines"),
        shown_lines: retention("shown_lines"),
        withheld_results: retention("withheld"),
    })
}
